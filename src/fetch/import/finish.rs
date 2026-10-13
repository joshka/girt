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

struct Job {
    position: usize,
    object: Object,
}

struct Done {
    position: usize,
    id: ObjectId,
    described: Described,
    bytes: usize,
}

/// Hashes and parses decoded objects, inline or on scoped worker threads.
pub(super) struct Finisher {
    jobs: Option<SyncSender<Job>>,
    done: Option<Receiver<Done>>,
    /// Payload bytes submitted but not yet merged.
    in_flight: usize,
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
                    let Ok(Job { position, object }) = job else {
                        return;
                    };
                    let id = object.id();
                    let described = Described::new(id, &object);
                    let bytes = charge(&object);
                    drop(object);
                    if results
                        .send(Done {
                            position,
                            id,
                            described,
                            bytes,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
        drop(results);
        let mut finisher = Finisher {
            jobs: Some(jobs),
            done: Some(done),
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
        if let Some(jobs) = &self.jobs {
            jobs.send(Job { position, object })
                .map_err(|_| FetchError::Pack(Error::Corrupt("import worker stopped")))?;
            self.pending += 1;
        }
        self.drain(objects, false)
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
            merge(objects, finished.position, finished.id, finished.described)?;
        }
        Ok(())
    }

    /// Merges every submitted object, so all identities and descriptions are final.
    pub fn finish(&mut self, objects: &mut IndexedObjects) -> Result<(), FetchError> {
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
    Ok(())
}

#[cfg(test)]
mod tests;
