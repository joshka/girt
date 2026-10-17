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
    limits: FetchLimits,
    /// Discovery allowances shared by every proof read; exhausting one supplies no evidence.
    bytes: usize,
    objects: usize,
    edges: usize,
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
            limits,
            bytes: limits.max_known_bytes,
            objects: limits.max_known_objects,
            edges: limits.max_known_edges,
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
        // Every recorded edge is charged up front; a tree beyond the allowance proves nothing.
        let Some(edges) = self.edges.checked_sub(old_entries.len()) else {
            self.edges = 0;
            return Vec::new();
        };
        self.edges = edges;
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
        self.objects = self.objects.checked_sub(1)?;
        let read = super::known::discovery_read(self.limits, self.bytes);
        let object = known.store.as_ref()?.read_with(id, read).ok().flatten()?;
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

impl Bounded<'_> {
    /// The kind a complete dependency must have where publication stops at it, if it is one.
    pub(super) fn complete_kind(&self, id: ObjectId) -> Option<ObjectKind> {
        complete_kind(self.known, self.boundaries, id)
    }
}

/// The kind evidence requires of a complete dependency: a paired tree edge's kind, or a trusted
/// complete root's stored kind.
pub(super) fn complete_kind(
    known: &KnownHistory,
    boundaries: &TreeBoundaries,
    id: ObjectId,
) -> Option<ObjectKind> {
    boundaries.kind(id).or_else(|| {
        known
            .trusted_complete(id)
            .then(|| known.complete_kind(id))
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::{InitKind, ObjectFormat, Repository};

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A Git-written commit with a nested tree, its known history and the new tree's payload.
    fn parent() -> (tempfile::TempDir, KnownHistory, ObjectId, ObjectId) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(ObjectFormat::Sha1, root.path(), InitKind::Worktree).unwrap();
        std::fs::create_dir(root.path().join("dir")).unwrap();
        std::fs::write(root.path().join("dir/file"), "a\n").unwrap();
        std::fs::write(root.path().join("top"), "b\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "base"]);
        let commit: ObjectId = git(root.path(), &["rev-parse", "HEAD"]).parse().unwrap();
        let tree: ObjectId = git(root.path(), &["rev-parse", "HEAD^{tree}"])
            .parse()
            .unwrap();
        let objects = repo.objects_with(Default::default()).unwrap();
        let known =
            KnownHistory::from_store_roots(objects, &[commit], &AtomicBool::new(false)).unwrap();
        (root, known, commit, tree)
    }

    fn pair(limits: FetchLimits) -> TreeBoundaries {
        let (_root, known, commit, old) = parent();
        let new = ObjectId::for_blob(ObjectFormat::Sha1, b"new tree");
        let mut boundaries = TreeBoundaries::new(&known, limits);
        boundaries.pair_commit(&known, new, &[commit]);
        boundaries.pairs.insert(new, old);
        boundaries.pair_tree(&known, new, b"");
        boundaries
    }

    /// Pairing records the parent tree's edges within the discovery allowances.
    #[test]
    fn pairing_records_parent_tree_edges() {
        assert_eq!(pair(FetchLimits::trusted()).complete.len(), 3);
    }

    /// An exhausted object or edge allowance supplies no evidence, so the ordinary walk applies.
    #[test]
    fn exhausted_discovery_allowances_supply_no_evidence() {
        let objects = pair(FetchLimits {
            max_known_objects: 1,
            ..FetchLimits::trusted()
        });
        assert!(
            objects.complete.len() <= 1,
            "only the parent's tree edge is recorded"
        );
        let edges = pair(FetchLimits {
            max_known_edges: 1,
            ..FetchLimits::trusted()
        });
        assert_eq!(
            edges.complete.len(),
            1,
            "only the parent's tree edge is recorded"
        );
        let none = pair(FetchLimits {
            max_known_objects: 0,
            ..FetchLimits::trusted()
        });
        assert!(none.complete.is_empty());
    }
}
