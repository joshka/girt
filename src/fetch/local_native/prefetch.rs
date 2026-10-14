//! Reads queued source objects ahead of the selection walk on worker threads.
//!
//! The walk itself stays sequential: it takes each object's read in queue order and applies every
//! kind, limit and edge check exactly as before. Workers only perform the reads the walk would
//! make next. A read the walk never takes, because the walk failed first, is discarded.
//!
//! Memory is bounded by 64 MiB of finished reads the walk has not taken, plus up to two reads in
//! progress per worker, each within the walk's per-object read limits.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};

use crate::fetch::FetchError;
use crate::{
    IdMap, ObjectId, ObjectKind, Objects, PeerIdMap, PeerIdSet, ReadLimits, UnverifiedObject,
};

/// Queue entries read ahead and not yet taken, at most.
const AHEAD: usize = 256;

/// Payload bytes of finished reads not yet taken, beyond which no new reads start.
const AHEAD_BYTES: usize = 64 * 1024 * 1024;

/// One object's read: its kind and size, plus its payload when the walk needs edges.
pub(super) struct Fetched {
    pub kind: ObjectKind,
    pub length: usize,
    pub payload: Option<UnverifiedObject>,
}

type Outcome = Result<Fetched, FetchError>;

/// An object to read and the kind its first queued edge requires.
type Job = (ObjectId, Option<ObjectKind>);

/// Reads `id` as the walk would: an edge requiring a commit, tree or tag needs the payload, whose
/// read supplies the kind and length a separate header read would.
pub(super) fn read(
    store: &Objects,
    id: ObjectId,
    kind: Option<ObjectKind>,
    limits: ReadLimits,
    cancel: &AtomicBool,
) -> Outcome {
    if kind.is_some_and(|kind| kind != ObjectKind::Blob) {
        let object = store
            .read_unverified(id, limits, cancel)
            .map_err(|source| FetchError::LocalRead { id, source })?
            .ok_or(FetchError::Missing(id))?;
        Ok(Fetched {
            kind: object.kind(),
            length: object.data().len(),
            payload: Some(object),
        })
    } else {
        let (kind, length, _) = store
            .transfer_object(id, limits, cancel)
            .map_err(|source| FetchError::LocalRead { id, source })?
            .ok_or(FetchError::Missing(id))?;
        Ok(Fetched {
            kind,
            length,
            payload: None,
        })
    }
}

/// Channels to the read workers: jobs out, finished reads back.
struct Workers {
    jobs: Sender<Job>,
    done: Receiver<(ObjectId, Outcome)>,
}

/// Hands the walk each object's read, reading queued objects ahead on workers when available.
pub(super) struct Prefetch<'a> {
    store: &'a Objects,
    limits: ReadLimits,
    cancel: &'a AtomicBool,
    workers: Option<Workers>,
    /// Submitted reads not yet taken by the walk. Keyed by names from source trees, before any
    /// read.
    submitted: PeerIdSet,
    /// Finished reads not yet taken by the walk.
    finished: PeerIdMap<Outcome>,
    /// Queue entries already considered for reading ahead, counted from the queue's front.
    scanned: usize,
    /// Reads sent to workers whose results have not arrived.
    in_flight: usize,
    /// Reads allowed in flight at once.
    depth: usize,
    /// Payload bytes of finished reads not yet taken.
    ahead_bytes: usize,
}

/// Runs `walk` with a prefetcher whose workers live until `walk` returns.
///
/// Uses at most `max_threads` workers, and none when fewer than two are allowed or available.
pub(super) fn with_prefetch<R>(
    store: &Objects,
    limits: ReadLimits,
    cancel: &AtomicBool,
    max_threads: usize,
    walk: impl FnOnce(&mut Prefetch<'_>) -> R,
) -> R {
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(max_threads);
    let mut prefetch = Prefetch {
        store,
        limits,
        cancel,
        workers: None,
        submitted: PeerIdSet::default(),
        finished: PeerIdMap::default(),
        scanned: 0,
        in_flight: 0,
        depth: threads * 2,
        ahead_bytes: 0,
    };
    if threads < 2 {
        return walk(&mut prefetch);
    }
    let (jobs, queue) = std::sync::mpsc::channel::<Job>();
    let (results, done) = std::sync::mpsc::channel();
    let queue = Mutex::new(queue);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let results = results.clone();
            let queue = &queue;
            scope.spawn(move || {
                loop {
                    let job = queue.lock().unwrap_or_else(|e| e.into_inner()).recv();
                    let Ok((id, kind)) = job else {
                        return;
                    };
                    if results
                        .send((id, read(store, id, kind, limits, cancel)))
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
        drop(results);
        prefetch.workers = Some(Workers { jobs, done });
        let result = walk(&mut prefetch);
        // Closing the queue stops the workers; the scope joins them.
        prefetch.workers = None;
        result
    })
}

impl Prefetch<'_> {
    /// Notes that the walk removed the entry at the queue's front.
    pub fn popped(&mut self) {
        self.scanned = self.scanned.saturating_sub(1);
    }

    /// Returns the read of `id`, which the walk just took from the front of `pending` with the
    /// expected `kind`. Reads queued entries the walk has not yet seen ahead of time.
    pub fn take(
        &mut self,
        id: ObjectId,
        kind: Option<ObjectKind>,
        pending: &VecDeque<(ObjectId, Option<ObjectKind>)>,
        observed: &IdMap<ObjectKind>,
    ) -> Outcome {
        let Some(Workers { jobs, done }) = &self.workers else {
            return read(self.store, id, kind, self.limits, self.cancel);
        };
        if self.submitted.insert(id) {
            // Nothing read it ahead, so the walk's own kind decides the read.
            jobs.send((id, kind))
                .map_err(|_| FetchError::Protocol("local read worker stopped"))?;
            self.in_flight += 1;
        }
        while self.submitted.len() < AHEAD
            && self.in_flight < self.depth
            && self.ahead_bytes < AHEAD_BYTES
            && let Some(&(next, next_kind)) = pending.get(self.scanned)
        {
            self.scanned += 1;
            // The first queued occurrence is the one the walk takes, so its kind decides the read.
            if !observed.contains_key(&next) && self.submitted.insert(next) {
                jobs.send((next, next_kind))
                    .map_err(|_| FetchError::Protocol("local read worker stopped"))?;
                self.in_flight += 1;
            }
        }
        loop {
            if let Some(outcome) = self.finished.remove(&id) {
                self.submitted.remove(&id);
                self.ahead_bytes -= payload_bytes(&outcome);
                return outcome;
            }
            let (finished, outcome) = done
                .recv()
                .map_err(|_| FetchError::Protocol("local read worker stopped"))?;
            self.in_flight -= 1;
            self.ahead_bytes += payload_bytes(&outcome);
            self.finished.insert(finished, outcome);
        }
    }
}

fn payload_bytes(outcome: &Outcome) -> usize {
    outcome
        .as_ref()
        .ok()
        .and_then(|fetched| fetched.payload.as_ref())
        .map_or(0, |payload| payload.data().len())
}
