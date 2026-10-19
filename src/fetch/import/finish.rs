//! Identity hashing and edge parsing of decoded objects on worker threads.
//!
//! Decoding a pack is sequential: each entry's end is only known after inflating it, and deltas
//! need their bases. Hashing each decoded object and parsing its edges are independent per object
//! and dominate import time, so workers do them while the decoding thread moves on. Every
//! identity and description is merged on the decoding thread, in [`Finisher::finish`] or
//! [`Finisher::drain`], before anything reads it.

use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};

use super::{Error, IndexedObjects};
use crate::fetch::verified::Described;
use crate::fetch::{FetchError, check_cancelled};
use crate::{Object, ObjectId};

/// Memory charged to submitted objects not yet merged: each payload plus, for a tree, its parsed
/// edges. One larger object may still run alone.
const IN_FLIGHT_BYTES: usize = 64 * 1024 * 1024;

/// What one submitted object can hold until merged. A parsed tree edge takes at most 34 bytes and
/// its entry at least 28, so a tree's edges take at most 1.25 times its payload.
fn charge(object: &Object) -> usize {
    let bytes = object.data().len();
    if object.kind() == crate::ObjectKind::Tree {
        bytes + bytes + bytes / 4
    } else {
        bytes
    }
}

/// Packs smaller than this are hashed on the decoding thread; workers would not pay for themselves.
const MIN_PARALLEL_OBJECTS: usize = 1024;

/// Objects or payload bytes gathered before a batch goes to a worker. Most objects take far less
/// time to hash than one channel handoff, so workers receive them in batches.
const BATCH_OBJECTS: usize = 64;
const BATCH_BYTES: usize = 1024 * 1024;

type Job = Vec<(usize, Object)>;

struct Done {
    finished: Vec<(usize, ObjectId, Described)>,
    bytes: usize,
}

/// Hashes and parses decoded objects, inline or on scoped worker threads.
pub(super) struct Finisher {
    jobs: Option<SyncSender<Job>>,
    done: Option<Receiver<Done>>,
    /// Objects submitted but not yet sent to a worker, and their charged bytes.
    batch: Job,
    batch_bytes: usize,
    /// Payload bytes submitted but not yet merged, including the unsent batch.
    in_flight: usize,
    /// Batches sent but not yet merged.
    pending: usize,
}

/// Runs `body` with a finisher whose workers live until `body` returns.
///
/// Uses at most `max_threads` workers, and none for small packs or when fewer than two are allowed
/// or available.
pub(super) fn with_finisher<R>(
    objects: usize,
    max_threads: usize,
    body: impl FnOnce(&mut Finisher) -> R,
) -> R {
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(max_threads);
    if threads < 2 || objects < MIN_PARALLEL_OBJECTS {
        return body(&mut Finisher {
            jobs: None,
            done: None,
            batch: Vec::new(),
            batch_bytes: 0,
            in_flight: 0,
            pending: 0,
        });
    }
    let (jobs, queue) = std::sync::mpsc::sync_channel::<Job>(threads * 4);
    let (results, done) = std::sync::mpsc::channel::<Done>();
    let queue = Mutex::new(queue);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let results = results.clone();
            let queue = &queue;
            scope.spawn(move || {
                loop {
                    let job = queue.lock().unwrap_or_else(|e| e.into_inner()).recv();
                    let Ok(job) = job else {
                        return;
                    };
                    let mut bytes = 0;
                    let finished = job
                        .into_iter()
                        .map(|(position, object)| {
                            let id = object.id();
                            bytes += charge(&object);
                            (position, id, Described::new(id, &object))
                        })
                        .collect();
                    if results.send(Done { finished, bytes }).is_err() {
                        return;
                    }
                }
            });
        }
        drop(results);
        let mut finisher = Finisher {
            jobs: Some(jobs),
            done: Some(done),
            batch: Vec::with_capacity(BATCH_OBJECTS),
            batch_bytes: 0,
            in_flight: 0,
            pending: 0,
        };
        let result = body(&mut finisher);
        // Closing the queue stops idle workers; the scope joins them.
        finisher.jobs = None;
        finisher.done = None;
        result
    })
}

impl Finisher {
    /// Hashes and describes `object`, decoded at `position`, now or on a worker.
    ///
    /// Deferred objects keep a placeholder identity until merged; they must already be marked
    /// resolved so no decode verifies against it.
    pub fn submit(
        &mut self,
        objects: &mut IndexedObjects,
        position: usize,
        object: Object,
        cancel: &AtomicBool,
    ) -> Result<(), FetchError> {
        if self.jobs.is_none() {
            let id = object.id();
            let described = Described::new(id, &object);
            return merge(objects, position, id, described);
        }
        let bytes = charge(&object);
        // Every in-flight byte belongs to a pending job, so waiting for one result always
        // makes progress.
        while self.in_flight != 0 && self.in_flight + bytes > IN_FLIGHT_BYTES {
            self.drain(objects, true)?;
            check_cancelled(cancel)?;
        }
        self.in_flight += bytes;
        self.batch_bytes += bytes;
        self.batch.push((position, object));
        if self.batch.len() >= BATCH_OBJECTS || self.batch_bytes >= BATCH_BYTES {
            self.send()?;
        }
        self.drain(objects, false)
    }

    /// Sends the gathered batch to a worker.
    fn send(&mut self) -> Result<(), FetchError> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let batch = std::mem::replace(&mut self.batch, Vec::with_capacity(BATCH_OBJECTS));
        self.batch_bytes = 0;
        if let Some(jobs) = &self.jobs {
            jobs.send(batch)
                .map_err(|_| FetchError::Pack(Error::Corrupt("import worker stopped")))?;
            self.pending += 1;
        }
        Ok(())
    }

    /// Hashes and describes `object` on this thread, so its identity is final on return.
    pub fn submit_now(
        &mut self,
        objects: &mut IndexedObjects,
        position: usize,
        object: Object,
    ) -> Result<(), FetchError> {
        let id = object.id();
        let described = Described::new(id, &object);
        merge(objects, position, id, described)
    }

    /// Merges finished objects, waiting for at least one when `block` is set and any are pending.
    fn drain(&mut self, objects: &mut IndexedObjects, mut block: bool) -> Result<(), FetchError> {
        if block {
            // Waiting on results requires every charged object to be with a worker.
            self.send()?;
        }
        let Some(done) = &self.done else {
            return Ok(());
        };
        while self.pending != 0 {
            let next = if block {
                block = false;
                done.recv().map_err(|_| TryRecvError::Disconnected)
            } else {
                done.try_recv()
            };
            let finished = match next {
                Ok(finished) => finished,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(FetchError::Pack(Error::Corrupt("import worker stopped")));
                }
            };
            self.pending -= 1;
            self.in_flight -= finished.bytes;
            for (position, id, described) in finished.finished {
                merge(objects, position, id, described)?;
            }
        }
        Ok(())
    }

    /// Merges every submitted object, so all identities and descriptions are final.
    pub fn finish(&mut self, objects: &mut IndexedObjects) -> Result<(), FetchError> {
        self.send()?;
        while self.pending != 0 {
            self.drain(objects, true)?;
        }
        Ok(())
    }
}

/// Assigns `id` to the entry at `position`, rejecting a second entry with the same identity.
fn merge(
    objects: &mut IndexedObjects,
    position: usize,
    id: ObjectId,
    described: Described,
) -> Result<(), FetchError> {
    if objects.identities.insert(id, position).is_some() {
        return Err(Error::Corrupt("duplicate received identity").into());
    }
    if let Some(resolved) = &mut objects.entries[position].resolved {
        resolved.0 = id;
    }
    objects.verified.record(id, described);
    // Peer-chosen tree targets are retained before connectivity charges edges, and may name
    // objects outside the pack, so bound the table by the received and local object budgets.
    let limits = &objects.limits;
    if objects.verified.tree_targets() > limits.max_objects.saturating_add(limits.max_known_objects)
    {
        return Err(FetchError::Limit("tree targets"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
