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

#[cfg(test)]
mod capped_tests {
    use super::*;
    use crate::{AlternateLimits, ObjectFormat, PackLimits};

    /// Empty blocks make compressed input exceed the cap while decoding to a valid empty blob.
    fn padded_source(format: ObjectFormat) -> (tempfile::TempDir, Objects, ObjectId, usize) {
        let root = tempfile::tempdir().unwrap();
        let id = ObjectId::for_blob(format, b"");
        let payload = b"blob 0\0";
        let mut compressed = vec![0x78, 0x01];
        for _ in 0..10_000 {
            compressed.extend_from_slice(&[0, 0, 0, 0xff, 0xff]);
        }
        compressed.extend_from_slice(&[1, 7, 0, 0xf8, 0xff]);
        compressed.extend_from_slice(payload);
        let (mut a, mut b) = (1_u32, 0_u32);
        for byte in payload {
            a = (a + u32::from(*byte)) % 65521;
            b = (b + a) % 65521;
        }
        compressed.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let hex = id.to_string();
        let path = root.path().join(&hex[..2]).join(&hex[2..]);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &compressed).unwrap();
        let objects = Objects::open(
            format,
            root.path(),
            PackLimits::default(),
            AlternateLimits::default(),
        )
        .unwrap();
        (root, objects, id, compressed.len())
    }

    #[rstest::rstest]
    #[case::sha1_metadata(crate::ObjectFormat::Sha1, None)]
    #[case::sha1_blob(crate::ObjectFormat::Sha1, Some(ObjectKind::Blob))]
    #[case::sha1_payload(crate::ObjectFormat::Sha1, Some(ObjectKind::Tree))]
    #[case::sha256_metadata(crate::ObjectFormat::Sha256, None)]
    #[case::sha256_blob(crate::ObjectFormat::Sha256, Some(ObjectKind::Blob))]
    #[case::sha256_payload(crate::ObjectFormat::Sha256, Some(ObjectKind::Tree))]
    fn controlled_workers_cap_source_input(
        #[case] format: ObjectFormat,
        #[case] kind: Option<ObjectKind>,
        #[values(1, 2)] threads: usize,
    ) {
        let (_root, objects, id, length) = padded_source(format);
        let cancel = AtomicBool::new(false);
        let limits = ReadLimits::default().with_max_input_bytes(length - 1);
        let outcome = with_prefetch::<true>(&objects, limits, &cancel, threads, |prefetch| {
            prefetch.take(id, kind, &VecDeque::new(), &IdMap::default())
        });
        assert!(matches!(
            outcome,
            Err(FetchError::Limit("loose compressed bytes"))
        ));
        let admitted = with_prefetch::<true>(
            &objects,
            limits.with_max_input_bytes(length),
            &cancel,
            threads,
            |prefetch| prefetch.take(id, kind, &VecDeque::new(), &IdMap::default()),
        )
        .unwrap();
        assert_eq!((admitted.kind, admitted.length), (ObjectKind::Blob, 0));
    }
}
