//! Per-receive read accounting, separate from construction of known-history evidence.

use std::borrow::Cow;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{KnownHistory, KnownObjects};
use crate::fetch::{FetchError, FetchLimits, check_cancelled};
use crate::{Object, ObjectId, ObjectKind, ObjectReadError, ReadLimits};

/// Carries the caller's read limits and cancellation through lazy destination lookups.
///
/// Controlled fallback attempts, including missing/repeated IDs, share the known-object count.
/// Fallback payloads are capped by the remaining aggregate bytes before decoding and charged once
/// read successfully. Payloads already retained in `KnownHistory` were admitted during construction
/// and require no fallback.
/// A bounded filesystem read, decode or hash can finish before cancellation is observed.
/// The false specialization delegates to the existing trusted methods and ignores these controls.
pub(in crate::fetch) struct KnownRead<'a, const CONTROLLED: bool> {
    known: &'a KnownHistory,
    limits: FetchLimits,
    cancel: &'a AtomicBool,
    attempts: Cell<usize>,
    bytes: Cell<usize>,
}

impl<'a, const CONTROLLED: bool> KnownRead<'a, CONTROLLED> {
    pub(in crate::fetch) fn new(
        known: &'a KnownHistory,
        limits: FetchLimits,
        cancel: &'a AtomicBool,
    ) -> Self {
        Self {
            known,
            limits,
            cancel,
            attempts: Cell::new(limits.max_known_objects),
            bytes: Cell::new(limits.max_known_bytes),
        }
    }

    pub(in crate::fetch) fn get(
        &self,
        id: ObjectId,
    ) -> Result<Option<Cow<'_, Object>>, FetchError> {
        if !CONTROLLED {
            return Ok(self.known.get(id));
        }
        self.get_raw(id, self.limits.known_read)
            .map_err(|source| read_error(id, source))
    }

    fn get_raw(
        &self,
        id: ObjectId,
        read: ReadLimits,
    ) -> Result<Option<Cow<'_, Object>>, ObjectReadError> {
        self.check()?;
        if self.known.missing_empty_tree == Some(id) {
            return Ok(None);
        }
        if let Some(object) = self.known.objects.get(&id) {
            return Ok(Some(Cow::Borrowed(object)));
        }
        let Some(store) = &self.known.store else {
            return Ok(None);
        };
        let attempts = self
            .attempts
            .get()
            .checked_sub(1)
            .ok_or(ObjectReadError::Limit("known read attempts"))?;
        self.attempts.set(attempts);
        let read = intersect(read, self.limits.known_read);
        let remaining = self.bytes.get();
        let aggregate_is_tighter = remaining < read.max_object_bytes.min(read.max_decode_bytes);
        let capped = read.with_max_object_bytes(read.max_object_bytes.min(remaining));
        // One read supplies both the verified payload and its byte charge; a separate header
        // lookup could consume uncapped compressed input or select a different loose copy.
        let object = store
            .read_capped(id, capped, self.cancel)
            .map_err(|error| match error {
                ObjectReadError::Limit("object bytes") if aggregate_is_tighter => {
                    ObjectReadError::Limit("known read bytes")
                }
                error => error,
            })?;
        self.check()?;
        if let Some(object) = &object {
            let bytes = remaining
                .checked_sub(object.data().len())
                .ok_or(ObjectReadError::Limit("known read bytes"))?;
            self.bytes.set(bytes);
        }
        Ok(object.map(Cow::Owned))
    }

    pub(in crate::fetch) fn kind(&self, id: ObjectId) -> Result<Option<ObjectKind>, FetchError> {
        if !CONTROLLED {
            return Ok(self.known.kind(id));
        }
        Ok(self.get(id)?.map(|object| object.kind()))
    }

    pub(in crate::fetch) fn stored_kind(
        &self,
        id: ObjectId,
    ) -> Result<Option<ObjectKind>, FetchError> {
        if !CONTROLLED {
            return Ok(self.known.stored_kind(id));
        }
        Ok(self.get(id)?.map(|object| object.kind()))
    }

    /// Discards inapplicable evidence without replenishing work already charged to this receive.
    pub(in crate::fetch) fn use_history(&mut self, known: &'a KnownHistory) {
        self.known = known;
    }

    pub(in crate::fetch) fn history(&self) -> &'a KnownHistory {
        self.known
    }

    pub(in crate::fetch) fn complete_kind(
        &self,
        id: ObjectId,
    ) -> Result<Option<ObjectKind>, FetchError> {
        if !CONTROLLED {
            return Ok(self.known.complete_kind(id));
        }
        check_cancelled(self.cancel)?;
        if self.known.missing_empty_tree.is_none()
            && let Some(object) = self.known.objects.get(&id)
        {
            return Ok(Some(object.kind()));
        }
        if self.known.trusted_complete(id) {
            self.kind(id)
        } else {
            Ok(None)
        }
    }

    pub(in crate::fetch) fn applies_to(&self, wants: &[ObjectId]) -> Result<bool, FetchError> {
        if !CONTROLLED {
            return Ok(self.known.applies_to(wants));
        }
        if !self.known.only_when_all_wants_known {
            return Ok(true);
        }
        for batch in wants.chunks(64) {
            check_cancelled(self.cancel)?;
            for &id in batch {
                if self.complete_kind(id)?.is_none() {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    pub(in crate::fetch) fn complete_dependencies(
        &self,
        dependencies: &[ObjectId],
        boundaries: &crate::fetch::boundaries::TreeBoundaries,
    ) -> Result<crate::IdMap<ObjectKind>, FetchError> {
        if !CONTROLLED {
            return Ok(dependencies
                .iter()
                .filter_map(|&id| {
                    Some((
                        id,
                        crate::fetch::boundaries::complete_kind(self.known, boundaries, id)?,
                    ))
                })
                .collect());
        }
        let mut complete = crate::IdMap::default();
        for batch in dependencies.chunks(64) {
            check_cancelled(self.cancel)?;
            for &id in batch {
                let kind = match boundaries.kind(id) {
                    Some(kind) => Some(kind),
                    None if self.known.trusted_complete(id) => self.complete_kind(id)?,
                    None => None,
                };
                if let Some(kind) = kind {
                    complete.insert(id, kind);
                }
            }
        }
        Ok(complete)
    }

    fn check(&self) -> Result<(), ObjectReadError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(ObjectReadError::Cancelled)
        } else {
            Ok(())
        }
    }
}

fn read_error(id: ObjectId, source: ObjectReadError) -> FetchError {
    match source {
        ObjectReadError::Cancelled => FetchError::Cancelled,
        ObjectReadError::Limit(reason) => FetchError::Limit(reason),
        source => FetchError::LocalRead { id, source },
    }
}

fn intersect(read: ReadLimits, ceiling: ReadLimits) -> ReadLimits {
    ReadLimits {
        max_object_bytes: read.max_object_bytes.min(ceiling.max_object_bytes),
        max_delta_bytes: read.max_delta_bytes.min(ceiling.max_delta_bytes),
        max_decode_bytes: read.max_decode_bytes.min(ceiling.max_decode_bytes),
        max_input_bytes: read.max_input_bytes.min(ceiling.max_input_bytes),
        max_delta_depth: read.max_delta_depth.min(ceiling.max_delta_depth),
    }
}

impl KnownHistory {
    /// Admits reference roots under aggregate read and tag-edge budgets, without a history walk.
    ///
    /// Root occurrences use max_wants; fallback reads use max_known_objects/max_known_bytes and
    /// known_read. Each tag chain also respects peel limits. Unreadable roots refuse instead of
    /// silently dropping evidence. Destination references establish completeness of peeled commits;
    /// this does not validate their reachable history. The supplied controlled snapshot stays held.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "maintenance root composition follows")
    )]
    pub(crate) fn from_store_roots_controlled(
        store: crate::Objects,
        roots: &[ObjectId],
        limits: FetchLimits,
        peel: crate::PeelLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, FetchError> {
        check_cancelled(cancel)?;
        if roots.len() > limits.max_wants {
            return Err(FetchError::Limit("known roots"));
        }
        if store.shallow_roots().iter().len() > limits.max_shallow_roots {
            return Err(FetchError::Limit("shallow roots"));
        }
        let missing_empty_tree = super::missing_empty_tree(&store, cancel)?;
        let mut known = Self {
            shallow: store.shallow_roots().iter().collect(),
            store: Some(std::sync::Arc::new(store)),
            missing_empty_tree,
            ..Self::default()
        };
        let mut complete = crate::IdSet::default();
        {
            let reads = KnownRead::<true>::new(&known, limits, cancel);
            let mut edges = limits.max_known_edges;
            for &root in roots {
                check_cancelled(cancel)?;
                let peeled = crate::peel::peel(root, peel, cancel, |id, read| {
                    let object = reads.get_raw(id, read)?;
                    if object
                        .as_ref()
                        .is_some_and(|object| object.kind() == ObjectKind::Tag)
                    {
                        edges = edges
                            .checked_sub(1)
                            .ok_or(ObjectReadError::Limit("known root tag edges"))?;
                    }
                    Ok(object.map(Cow::into_owned))
                })
                .map_err(peel_error)?;
                if peeled.kind == ObjectKind::Commit {
                    complete.insert(peeled.target);
                }
            }
        }
        check_cancelled(cancel)?;
        known.complete_roots = complete;
        Ok(known)
    }
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "maintenance root composition follows")
)]
fn peel_error(error: crate::PeelError) -> FetchError {
    match error.source.as_ref() {
        crate::PeelFailure::Cancelled => FetchError::Cancelled,
        crate::PeelFailure::Bytes => FetchError::Limit("known root peel bytes"),
        crate::PeelFailure::Depth => FetchError::Limit("known root tag depth"),
        crate::PeelFailure::Read(ObjectReadError::Limit(reason)) => FetchError::Limit(reason),
        crate::PeelFailure::Read(ObjectReadError::Cancelled) => FetchError::Cancelled,
        _ => FetchError::Peel(Box::new(error)),
    }
}

impl<const CONTROLLED: bool> KnownObjects for KnownRead<'_, CONTROLLED> {
    fn get(&self, id: ObjectId) -> Option<Cow<'_, Object>> {
        if CONTROLLED {
            (self.known.missing_empty_tree != Some(id))
                .then(|| self.known.objects.get(&id).map(Cow::Borrowed))
                .flatten()
        } else {
            self.known.get(id)
        }
    }

    fn try_get(&self, id: ObjectId) -> Result<Option<Cow<'_, Object>>, FetchError> {
        KnownRead::get(self, id)
    }

    fn try_known_kind(&self, id: ObjectId) -> Result<Option<ObjectKind>, FetchError> {
        if CONTROLLED {
            self.kind(id)
        } else {
            Ok(self.known.known_kind(id))
        }
    }

    fn trusted_complete(&self, id: ObjectId) -> bool {
        self.known.trusted_complete(id)
    }
}
