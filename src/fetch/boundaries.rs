//! Complete boundaries named by the trees of complete parents, so validation reads changed paths.
//!
//! A new commit whose parent is trusted complete usually keeps most of that parent's tree. Pairing
//! the new tree with the parent's tree at the same path, and descending only into changed
//! subtrees, finds the unchanged entries without reading them. Every typed edge of a paired
//! complete tree is reachable from an existing reference through verified edges, so it is trusted
//! complete exactly as the parent is, like Git's `--not --all` connectivity check.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;

use super::connectivity::ReceivedObjects;
use super::known::{KnownHistory, KnownObjects};
use super::{FetchError, FetchLimits, check_cancelled};
use crate::tree::RawEntries;
use crate::{EntryMode, IdMap, IdSet, Object, ObjectId, ObjectKind};

/// Objects named by complete trees paired with new trees, with the kind each edge requires.
///
/// Only checked reads of trees reachable from trusted complete commits add boundaries. A failed
/// read or parse supplies no evidence; the ordinary walk verifies that branch instead.
#[derive(Debug)]
pub(crate) struct TreeBoundaries {
    complete: IdMap<ObjectKind>,
    /// New tree to the complete tree at the same path in a complete parent.
    pairs: IdMap<ObjectId>,
    /// Readers synthesize the empty tree; it is never a physical boundary.
    empty_tree: Option<ObjectId>,
    read: crate::ReadLimits,
    bytes: usize,
}

impl TreeBoundaries {
    pub(crate) fn new(known: &KnownHistory, limits: FetchLimits) -> Self {
        Self {
            complete: IdMap::default(),
            pairs: IdMap::default(),
            empty_tree: known
                .store
                .as_ref()
                .map(|store| store.object_format().hash_object(ObjectKind::Tree, b"")),
            read: limits.known_read,
            bytes: limits.max_known_bytes,
        }
    }

    /// The kind a complete boundary has, if `id` is one.
    pub(crate) fn kind(&self, id: ObjectId) -> Option<ObjectKind> {
        self.complete.get(&id).copied()
    }

    /// Pairs a new commit's tree with the tree of its first trusted complete parent.
    pub(crate) fn pair_commit(
        &mut self,
        known: &KnownHistory,
        tree: ObjectId,
        parents: &[ObjectId],
    ) {
        if self.pairs.contains_key(&tree) {
            return;
        }
        let Some(parent) = parents.iter().find(|&&id| known.trusted_complete(id)) else {
            return;
        };
        let Some(object) = self.read(known, *parent, ObjectKind::Commit) else {
            return;
        };
        let Ok(commit) = crate::Commit::parse(object.object_format(), object.data()) else {
            return;
        };
        let old = commit.tree();
        if Some(old) == self.empty_tree {
            return;
        }
        self.complete.insert(old, ObjectKind::Tree);
        if old != tree {
            self.pairs.insert(tree, old);
        }
    }

    /// Records the edges of the complete tree paired with `tree`, whose payload is `data`, and
    /// pairs each changed subtree with the complete subtree of the same name.
    ///
    /// Returns the newly paired subtrees.
    pub(crate) fn pair_tree(
        &mut self,
        known: &KnownHistory,
        tree: ObjectId,
        data: &[u8],
    ) -> Vec<ObjectId> {
        let Some(&old) = self.pairs.get(&tree) else {
            return Vec::new();
        };
        let Some(object) = self.read(known, old, ObjectKind::Tree) else {
            return Vec::new();
        };
        let format = object.object_format();
        let Ok(old_entries) = RawEntries::new(format, object.data()).collect::<Result<Vec<_>, _>>()
        else {
            return Vec::new();
        };
        let mut names = std::collections::HashMap::new();
        for &(mode, name, id) in &old_entries {
            let kind = match mode {
                EntryMode::Gitlink => continue,
                EntryMode::Tree => ObjectKind::Tree,
                _ => ObjectKind::Blob,
            };
            if Some(id) == self.empty_tree {
                continue;
            }
            self.complete.insert(id, kind);
            if kind == ObjectKind::Tree {
                names.insert(name, id);
            }
        }
        let mut paired = Vec::new();
        for (mode, name, id) in RawEntries::new(format, data).flatten() {
            if mode == EntryMode::Tree
                && let Some(&old) = names.get(name)
                && old != id
                && !self.pairs.contains_key(&id)
            {
                self.pairs.insert(id, old);
                paired.push(id);
            }
        }
        paired
    }

    fn read(&mut self, known: &KnownHistory, id: ObjectId, kind: ObjectKind) -> Option<Object> {
        let mut read = self.read;
        read.max_object_bytes = read.max_object_bytes.min(self.bytes);
        let object = known.store.as_ref()?.read(id, read).ok().flatten()?;
        self.bytes = self.bytes.checked_sub(object.data().len())?;
        (object.kind() == kind).then_some(object)
    }
}

/// Pairs received commits and trees reachable from `wants` with complete local trees.
///
/// Reads only received commits' retained edges and the received trees that have a pair.
pub(super) fn discover(
    objects: &impl ReceivedObjects,
    known: &KnownHistory,
    wants: &[ObjectId],
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<TreeBoundaries, FetchError> {
    let mut boundaries = TreeBoundaries::new(known, limits);
    if known.store.is_none() || known.complete_roots.is_empty() {
        return Ok(boundaries);
    }
    let mut seen = IdSet::default();
    let mut pending: VecDeque<_> = wants.iter().copied().collect();
    while let Some(id) = pending.pop_front() {
        check_cancelled(cancel)?;
        if !seen.insert(id) || !objects.contains_key(&id) {
            continue;
        }
        match objects.kind(id, cancel)? {
            Some(kind @ (ObjectKind::Commit | ObjectKind::Tag)) => {
                let Some(edges) = objects.edges(id, cancel)? else {
                    continue;
                };
                let tree = edges
                    .iter()
                    .find(|(_, kind)| *kind == ObjectKind::Tree)
                    .map(|&(id, _)| id);
                let parents: Vec<_> = edges
                    .iter()
                    .filter(|(_, kind)| *kind == ObjectKind::Commit)
                    .map(|&(id, _)| id)
                    .collect();
                if let Some(tree) = tree
                    && kind == ObjectKind::Commit
                {
                    boundaries.pair_commit(known, tree, &parents);
                }
                pending.extend(edges.iter().map(|&(id, _)| id));
            }
            Some(ObjectKind::Tree) if boundaries.pairs.contains_key(&id) => {
                let Some(object) = objects.get(&id, cancel)? else {
                    continue;
                };
                pending.extend(boundaries.pair_tree(known, id, object.data()));
            }
            _ => {}
        }
    }
    Ok(boundaries)
}

/// Known history extended with paired tree boundaries.
pub(super) struct Bounded<'a> {
    pub(super) known: &'a KnownHistory,
    pub(super) boundaries: &'a TreeBoundaries,
}

impl KnownObjects for Bounded<'_> {
    fn get(&self, id: ObjectId) -> Option<Cow<'_, Object>> {
        self.known.get(id)
    }

    fn known_kind(&self, id: ObjectId) -> Option<ObjectKind> {
        self.boundaries
            .kind(id)
            .or_else(|| self.known.known_kind(id))
    }

    fn trusted_complete(&self, id: ObjectId) -> bool {
        self.boundaries.kind(id).is_some() || self.known.trusted_complete(id)
    }
}
