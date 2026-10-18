//! Exact include-minus-exclude closure with optional derived-metadata shortcuts.
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::AtomicBool;

use super::{Error, Limits, check, kind_index};
use crate::{Commit, EntryMode, ObjectId, ObjectKind, Objects, ReadLimits, Tag};

/// Aggregate bounds for both sides of one reachability difference.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct QueryLimits {
    /// Objects visited across both closures, counting a shared object once per side (5 million).
    pub max_objects: usize,
    /// Graph edges scheduled across both closures (20 million).
    pub max_edges: usize,
    /// Payload bytes read by fallback walks across both closures (1 GiB).
    pub max_walk_bytes: u64,
    /// Per-object verified decoding limits.
    pub read: ReadLimits,
    /// Aggregate optional metadata input, inventory and bitmap expansion bounds.
    pub bitmap: Limits,
}
impl Default for QueryLimits {
    fn default() -> Self {
        Self {
            max_objects: 5_000_000,
            max_edges: 20_000_000,
            max_walk_bytes: 1024 * 1024 * 1024,
            read: ReadLimits::default(),
            bitmap: Limits::default(),
        }
    }
}

/// An exact object-ID set, sorted by identity, retaining the object-store snapshot used to find it.
///
/// Packed handles stay pinned through [`Self::objects`]. Loose objects remain live filesystem
/// reads; this result does not prevent another writer from removing them. It is suitable for
/// selecting a fetch response after the caller has resolved and authorized the want/have tips.
#[derive(Clone, Debug)]
pub struct Reachability {
    objects: Objects,
    ids: Vec<ObjectId>,
    kinds: [usize; 4],
}
impl Reachability {
    /// Object identities in ascending order.
    pub fn ids(&self) -> impl ExactSizeIterator<Item = ObjectId> + '_ {
        self.ids.iter().copied()
    }
    /// Number of objects in the difference.
    pub fn len(&self) -> usize {
        self.ids.len()
    }
    /// Whether the difference contains no objects.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
    /// Number of result objects of `kind`.
    pub fn count(&self, kind: ObjectKind) -> usize {
        self.kinds[kind_index(kind)]
    }
    /// Whether `id` belongs to this difference.
    pub fn contains(&self, id: ObjectId) -> bool {
        self.ids.binary_search(&id).is_ok()
    }
    /// The snapshot used by the query, for reading or packing the selected objects.
    pub fn objects(&self) -> &Objects {
        &self.objects
    }
}

impl Objects {
    /// Returns this snapshot with bitmap shortcuts disabled, including after refresh.
    ///
    /// Use this for independent graph verification or when derived metadata's producer is not
    /// trusted. Queries still use the ordinary verified object reader.
    pub fn without_bitmaps(mut self) -> Self {
        self.bitmaps_enabled = false;
        self
    }

    /// Finds every object reachable from `include` except those reachable from `exclude`.
    ///
    /// Roots may be commits, tags, trees or blobs. Tags are peeled, commit trees and parents are
    /// followed, tree gitlinks are skipped, and this snapshot's shallow boundaries stop parent
    /// traversal. Sets are independent: exclusion never prunes the include walk. Missing roots or
    /// reachable objects are errors, so a server should pass only recognized have IDs.
    ///
    /// Valid pack/MIDX bitmaps may replace covered commit walks. Missing, stale, unsupported,
    /// malformed or uncovered bitmap data falls back to walking. Bitmap checks establish inventory
    /// consistency, not graph truth; use [`Self::without_bitmaps`] for an independent walk.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for missing, malformed or unreadable objects, mismatched edge types, or
    /// exhausted aggregate limits. No partial set is returned.
    pub fn reachable_difference(
        &self,
        include: &[ObjectId],
        exclude: &[ObjectId],
    ) -> Result<Reachability, Error> {
        self.reachable_difference_with(
            include,
            exclude,
            QueryLimits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Finds a reachability difference under aggregate bounds and cooperative cancellation.
    ///
    /// Limits cover both closures, metadata loading and repeated XOR expansion. Optional metadata
    /// that exceeds its bounds is skipped; graph-walk limits remain authoritative. A missing bitmap
    /// never makes an object unreachable. Filesystem reads and one bounded metadata parse or
    /// inventory verification cannot be interrupted.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::reachable_difference`] or [`Error::Cancelled`].
    pub fn reachable_difference_with(
        &self,
        include: &[ObjectId],
        exclude: &[ObjectId],
        limits: QueryLimits,
        cancel: &AtomicBool,
    ) -> Result<Reachability, Error> {
        check(cancel)?;
        let mut budget = Budget::new(limits);
        budget.edges(
            include
                .len()
                .checked_add(exclude.len())
                .ok_or(Error::Limit("edges"))?,
        )?;
        let tips: Vec<_> = include.iter().chain(exclude).copied().collect();
        let (indexes, decoded) = super::inventory::load(self, &tips, limits, cancel)?;
        self.difference_from(&indexes, decoded, budget, include, exclude, cancel)
    }

    /// Finds a reachability difference like [`Self::reachable_difference_with`], but only when a
    /// bitmap index covers some of the tips' history.
    ///
    /// Returns `None` when bitmaps are disabled or none applies, so the caller can choose its own
    /// walk rather than this one's verified reads.
    pub(crate) fn bitmap_difference(
        &self,
        include: &[ObjectId],
        exclude: &[ObjectId],
        limits: QueryLimits,
        cancel: &AtomicBool,
    ) -> Result<Option<Reachability>, Error> {
        check(cancel)?;
        let mut budget = Budget::new(limits);
        budget.edges(
            include
                .len()
                .checked_add(exclude.len())
                .ok_or(Error::Limit("edges"))?,
        )?;
        let tips: Vec<_> = include.iter().chain(exclude).copied().collect();
        let (indexes, decoded) = super::inventory::load(self, &tips, limits, cancel)?;
        if indexes.is_empty() {
            return Ok(None);
        }
        self.difference_from(&indexes, decoded, budget, include, exclude, cancel)
            .map(Some)
    }

    fn difference_from(
        &self,
        indexes: &[super::inventory::Bound],
        mut decoded: usize,
        mut budget: Budget,
        include: &[ObjectId],
        exclude: &[ObjectId],
        cancel: &AtomicBool,
    ) -> Result<Reachability, Error> {
        for source in indexes {
            if let Some((bitmap, visits)) =
                source.difference(include, exclude, &mut decoded, cancel)?
            {
                if visits > budget.objects {
                    return Err(Error::Limit("query objects"));
                }
                let mut ids = Vec::new();
                let mut kinds = [0; 4];
                for position in bitmap.positions() {
                    check(cancel)?;
                    let (id, kind) = source.object(position)?;
                    ids.push(id);
                    kinds[kind_index(kind)] += 1;
                }
                ids.sort_unstable();
                return Ok(Reachability {
                    objects: self.clone(),
                    ids,
                    kinds,
                });
            }
        }
        let mut walk = Walk {
            objects: self,
            indexes,
            budget: &mut budget,
            decoded: &mut decoded,
            cancel,
        };
        let included = walk.closure(include)?;
        let excluded = walk.closure(exclude)?;
        check(cancel)?;
        let mut ids = Vec::new();
        let mut kinds = [0; 4];
        for (id, kind) in included {
            check(cancel)?;
            if !excluded.contains_key(&id) {
                ids.push(id);
                kinds[kind_index(kind)] += 1;
            }
        }
        Ok(Reachability {
            objects: self.clone(),
            ids,
            kinds,
        })
    }
}

struct Budget {
    objects: usize,
    edges: usize,
    bytes: u64,
    read: ReadLimits,
}
impl Budget {
    fn new(limits: QueryLimits) -> Self {
        Self {
            objects: limits.max_objects,
            edges: limits.max_edges,
            bytes: limits.max_walk_bytes,
            read: limits.read,
        }
    }
    fn object(&mut self) -> Result<(), Error> {
        self.objects = self
            .objects
            .checked_sub(1)
            .ok_or(Error::Limit("query objects"))?;
        Ok(())
    }
    fn edges(&mut self, count: usize) -> Result<(), Error> {
        self.edges = self
            .edges
            .checked_sub(count)
            .ok_or(Error::Limit("query edges"))?;
        Ok(())
    }
}

struct Walk<'a> {
    objects: &'a Objects,
    indexes: &'a [super::inventory::Bound],
    budget: &'a mut Budget,
    decoded: &'a mut usize,
    cancel: &'a AtomicBool,
}
impl Walk<'_> {
    fn closure(&mut self, roots: &[ObjectId]) -> Result<BTreeMap<ObjectId, ObjectKind>, Error> {
        let mut found = BTreeMap::new();
        let mut queue: VecDeque<_> = roots.iter().map(|&id| (id, None)).collect();
        while let Some((id, expected)) = queue.pop_front() {
            check(self.cancel)?;
            if let Some(&kind) = found.get(&id) {
                kind_matches(id, kind, expected)?;
                continue;
            }
            let mut shortcut = false;
            for source in self.indexes {
                if let Some(bitmap) = source.reachable(id, self.decoded, self.cancel)? {
                    kind_matches(id, ObjectKind::Commit, expected)?;
                    for position in bitmap.positions() {
                        check(self.cancel)?;
                        let (id, kind) = source.object(position)?;
                        if let Some(&previous) = found.get(&id) {
                            kind_matches(id, kind, Some(previous))?;
                        } else {
                            self.budget.object()?;
                            found.insert(id, kind);
                        }
                    }
                    shortcut = true;
                    break;
                }
            }
            if shortcut {
                continue;
            }
            self.budget.object()?;
            let object = self
                .objects
                .read_controlled(id, self.budget.read, self.cancel)
                .map_err(|error| match error {
                    crate::ObjectReadError::Cancelled => Error::Cancelled,
                    error => Error::Read(error),
                })?
                .ok_or(Error::MissingObject(id))?;
            let kind = object.kind();
            kind_matches(id, kind, expected)?;
            self.budget.bytes = self
                .budget
                .bytes
                .checked_sub(object.data().len() as u64)
                .ok_or(Error::Limit("walk bytes"))?;
            found.insert(id, kind);
            let mut push = |target, kind| -> Result<(), Error> {
                check(self.cancel)?;
                self.budget.edges(1)?;
                queue.push_back((target, Some(kind)));
                Ok(())
            };
            let malformed = |source| Error::Object { id, source };
            match kind {
                ObjectKind::Blob => {}
                ObjectKind::Commit => {
                    let commit = Commit::parse(self.objects.object_format(), object.data())
                        .map_err(|e| malformed(Box::new(e)))?;
                    push(commit.tree(), ObjectKind::Tree)?;
                    if !self.objects.shallow_roots().contains(id) {
                        for &parent in commit.parents() {
                            push(parent, ObjectKind::Commit)?;
                        }
                    }
                }
                ObjectKind::Tag => {
                    let tag = Tag::parse(self.objects.object_format(), object.data())
                        .map_err(|e| malformed(Box::new(e)))?;
                    push(tag.target(), tag.target_kind())?;
                }
                ObjectKind::Tree => {
                    for entry in
                        crate::tree::RawEntries::new(self.objects.object_format(), object.data())
                    {
                        let (mode, _, target) = entry.map_err(|e| malformed(Box::new(e)))?;
                        if mode != EntryMode::Gitlink {
                            push(target, mode.object_kind())?;
                        }
                    }
                }
            }
        }
        Ok(found)
    }
}
fn kind_matches(
    id: ObjectId,
    actual: ObjectKind,
    expected: Option<ObjectKind>,
) -> Result<(), Error> {
    if expected.is_some_and(|expected| actual != expected) {
        Err(Error::Kind(id))
    } else {
        Ok(())
    }
}
