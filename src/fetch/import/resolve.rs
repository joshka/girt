//! Resolution of a received pack's delta trees on worker threads.
//!
//! Without REF_DELTA entries, every delta names its base by position inside the pack, so the
//! entries form trees rooted at whole objects. Each tree is independent: a worker takes a root,
//! inflates it, and resolves its descendants depth first, holding only the payloads on the path
//! from the root. Workers hash each result and parse its edges. The decoding thread merges every
//! identity and description as it arrives, as [`super::finish`] does for whole objects.
//!
//! Every check of sequential resolution still applies to every entry: exact stream lengths and
//! CRCs, the delta program against its base, [`FetchLimits::max_delta_depth`], the step budget
//! and the cumulative decode budget. Each base is decoded once, so the decode budget is charged no
//! more than sequential resolution charges it.
//!
//! [`FetchLimits::max_delta_depth`]: crate::fetch::FetchLimits::max_delta_depth

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};

use super::{Base, IndexedObjects, ReceivedEntry, fetch_read_error, inflate_at};
use crate::fetch::progress::ValidationObserver;
use crate::fetch::verified::Described;
use crate::fetch::{FetchError, FetchLimits};
use crate::pack::delta::apply;
use crate::{Object, ObjectId, ObjectKind, ObjectReadError as Error};

/// Objects or payload bytes a worker gathers before handing results to the decoding thread.
const BATCH_OBJECTS: usize = 64;
const BATCH_BYTES: usize = 1024 * 1024;

type Batch = Vec<(usize, ObjectId, Described)>;

/// What every worker shares: the received entries, their delta children and the budgets.
struct Trees<'a> {
    entries: &'a [ReceivedEntry],
    children: &'a [Vec<usize>],
    file: &'a std::fs::File,
    path: &'a std::path::Path,
    format: crate::ObjectFormat,
    limits: FetchLimits,
    cancel: &'a AtomicBool,
    /// Set after any failure, so other workers stop at their next entry.
    stop: AtomicBool,
    /// The cumulative decode budget left.
    remaining: AtomicUsize,
}

impl Trees<'_> {
    fn charge(&self, bytes: usize) -> Result<(), FetchError> {
        let mut left = self.remaining.load(Ordering::Relaxed);
        loop {
            let after = left
                .checked_sub(bytes)
                .ok_or(Error::Limit("cumulative decode bytes"))?;
            match self.remaining.compare_exchange_weak(
                left,
                after,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(current) => left = current,
            }
        }
    }

    fn inflate(&self, position: usize) -> Result<Vec<u8>, FetchError> {
        let entry = &self.entries[position];
        self.charge(entry.length)?;
        inflate_at(self.file, self.path, entry, self.cancel).map_err(fetch_read_error)
    }

    fn check(&self) -> Result<bool, FetchError> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(FetchError::Cancelled);
        }
        Ok(self.stop.load(Ordering::Relaxed))
    }

    /// Resolves every delta below `root`, sending each result to the decoding thread.
    fn resolve(&self, root: usize, results: &mpsc::SyncSender<Batch>) -> Result<(), FetchError> {
        let Base::Kind(kind) = self.entries[root].base else {
            return Err(Error::Corrupt("delta tree root").into());
        };
        let data = self.inflate(root)?;
        let object = Object {
            format: self.format,
            kind,
            data: data.into(),
        };
        let mut batch = Batch::with_capacity(BATCH_OBJECTS);
        let mut batch_bytes = 0;
        // Each frame is a resolved base, its depth, and the next of its children to resolve.
        let mut path = vec![(root, object, 0_usize, 0_usize)];
        while let Some((parent, base, depth, next)) = path.last_mut() {
            let Some(&child) = self.children[*parent].get(*next) else {
                path.pop();
                continue;
            };
            *next += 1;
            if self.check()? {
                return Ok(());
            }
            if *depth >= self.limits.max_delta_depth {
                return Err(FetchError::Limit("delta depth"));
            }
            let program = self.inflate(child)?;
            let mut left = self.remaining.load(Ordering::Relaxed);
            let before = left;
            let data = apply(
                base.data(),
                &program,
                self.limits.max_object_bytes,
                &mut left,
            )?;
            self.charge(before - left)?;
            let object = Object {
                format: self.format,
                kind,
                data: data.into(),
            };
            let id = object.id();
            batch_bytes += object.data().len();
            batch.push((child, id, Described::new(id, &object)));
            if batch.len() >= BATCH_OBJECTS || batch_bytes >= BATCH_BYTES {
                batch_bytes = 0;
                let full = std::mem::replace(&mut batch, Batch::with_capacity(BATCH_OBJECTS));
                if results.send(full).is_err() {
                    return Ok(());
                }
            }
            let depth = *depth + 1;
            if !self.children[child].is_empty() {
                path.push((child, object, depth, 0));
            }
        }
        if !batch.is_empty() {
            let _ = results.send(batch);
        }
        Ok(())
    }
}

impl IndexedObjects {
    /// Resolves every delta of a pack without REF_DELTA entries on `threads` workers.
    ///
    /// `roots` are the whole objects decoded in the first pass, whose identities are already
    /// merged. Afterwards every entry has its identity, depth and kind, as after sequential
    /// resolution.
    pub(super) fn resolve_parallel(
        &mut self,
        children: &[Vec<usize>],
        mut roots: Vec<usize>,
        remaining: &mut usize,
        observer: &mut ValidationObserver<'_>,
        threads: usize,
    ) -> Result<(), FetchError> {
        // Sequential resolution takes one step per entry and one per delta.
        let deltas = self.count - roots.len();
        if self.count.saturating_add(deltas) > self.limits.max_resolution_steps {
            return Err(FetchError::Limit("delta resolution steps"));
        }
        roots.retain(|&root| !children[root].is_empty());
        let path = self
            .pack
            .path()
            .expect("indexed packs are file backed")
            .to_owned();
        let trees = Trees {
            entries: &self.entries,
            children,
            file: &self.file,
            path: &path,
            format: self.format,
            limits: self.limits,
            cancel: observer.cancel,
            stop: AtomicBool::new(false),
            remaining: AtomicUsize::new(*remaining),
        };
        let next = AtomicUsize::new(0);
        let failure = Mutex::new(None);
        let identities = &mut self.identities;
        let verified = &mut self.verified;
        let merged = std::thread::scope(|scope| {
            let (sender, results) = mpsc::sync_channel::<Batch>(threads * 4);
            for _ in 0..threads {
                let sender = sender.clone();
                let (trees, next, failure, roots) = (&trees, &next, &failure, &roots);
                scope.spawn(move || {
                    while let Some(&root) = roots.get(next.fetch_add(1, Ordering::Relaxed)) {
                        if let Err(error) = trees.resolve(root, &sender) {
                            trees.stop.store(true, Ordering::Relaxed);
                            failure
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .get_or_insert(error);
                            return;
                        }
                        if trees.stop.load(Ordering::Relaxed) {
                            return;
                        }
                    }
                });
            }
            drop(sender);
            let mut merged = Ok(());
            // Keep receiving after a failure so no worker stays blocked on a full channel.
            for batch in results {
                if merged.is_err() {
                    continue;
                }
                for (position, id, described) in batch {
                    if identities.insert(id, position).is_some() {
                        merged = Err(Error::Corrupt("duplicate received identity").into());
                        trees.stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    verified.record(id, described);
                    observer.resolved_delta();
                }
                if merged.is_ok() && observer.cancel.load(Ordering::Relaxed) {
                    merged = Err(FetchError::Cancelled);
                    trees.stop.store(true, Ordering::Relaxed);
                }
            }
            merged
        });
        if let Some(error) = failure.into_inner().unwrap_or_else(|e| e.into_inner()) {
            return Err(error);
        }
        merged?;
        *remaining = trees.remaining.into_inner();
        // Record each delta's identity, then its depth and kind from its tree's root.
        for (&id, &position) in &self.identities {
            let entry = &mut self.entries[position];
            if entry.resolved.is_none() {
                entry.resolved = Some((id, 0, ObjectKind::Blob));
            }
        }
        for &root in &roots {
            let Some((_, _, kind)) = self.entries[root].resolved else {
                return Err(Error::Corrupt("delta tree root").into());
            };
            let mut pending = vec![(root, 0)];
            while let Some((parent, depth)) = pending.pop() {
                for &child in &children[parent] {
                    let resolved = self.entries[child]
                        .resolved
                        .as_mut()
                        .ok_or(Error::DeltaCycle)?;
                    resolved.1 = depth + 1;
                    resolved.2 = kind;
                    pending.push((child, depth + 1));
                }
            }
        }
        if self.entries.iter().any(|entry| entry.resolved.is_none()) {
            return Err(Error::DeltaCycle.into());
        }
        Ok(())
    }
}
