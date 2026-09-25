//! Explicit sparse tree expansion, independent of staging and working-file policy.
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Entry, Error, Index, Limits, Mode};
use crate::{EntryMode, ObjectId, ObjectKind, Objects, ReadLimits, Tree};

/// Aggregate traversal bounds for one sparse expansion across all collapsed directories.
///
/// Index [`Limits`] additionally bound output entries, individual paths and encoded bytes.
/// A bounded tree payload is parsed before counting its entries. Memory includes that tree,
/// pending directory paths and the complete draft index; this is not a precise heap budget.
#[derive(Clone, Copy, Debug)]
pub struct SparseLimits {
    /// Maximum tree occurrences read, including repeated identities; default 100,000.
    pub max_trees: usize,
    /// Maximum total tree payload bytes; default 256 MiB.
    pub max_tree_bytes: usize,
    /// Maximum tree entries visited, including directories; default one million.
    pub max_entries: usize,
    /// Maximum depth below each collapsed directory; default 1,024.
    pub max_depth: usize,
    /// Maximum total generated path bytes, including directories; default 64 MiB.
    pub max_path_bytes: usize,
    /// Independent object reconstruction bounds for each tree read.
    pub read: ReadLimits,
}
impl Default for SparseLimits {
    fn default() -> Self {
        Self {
            max_trees: 100_000,
            max_tree_bytes: 256 * 1024 * 1024,
            max_entries: 1_000_000,
            max_depth: 1024,
            max_path_bytes: 64 * 1024 * 1024,
            read: ReadLimits::default(),
        }
    }
}

/// Sparse expansion failure; the original index remains unchanged.
#[derive(Debug, thiserror::Error)]
pub enum SparseError {
    /// Output validation, object format or resource failure.
    #[error(transparent)]
    Index(#[from] Error),
    /// Cooperative cancellation was observed before replacing the draft.
    #[error("sparse expansion cancelled")]
    Cancelled,
    /// Tree lookup failed; the storage error retains the underlying cause.
    #[error("cannot read sparse tree {id}: {source}")]
    Read {
        /// Requested identity.
        id: ObjectId,
        /// Underlying object-store failure.
        #[source]
        source: crate::ObjectReadError,
    },
    /// An encountered tree does not exist.
    #[error("missing sparse tree {0}")]
    Missing(ObjectId),
    /// An encountered identity names a non-tree object.
    #[error("sparse tree {0} has another object kind")]
    Kind(ObjectId),
    /// Tree framing, names or ordering are invalid.
    #[error("invalid sparse tree {id}: {source}")]
    Tree {
        /// Requested identity.
        id: ObjectId,
        /// Parse or structural validation cause.
        #[source]
        source: crate::TreeError,
    },
}

impl Index {
    /// Expands all sparse directories into skip-worktree leaves without touching working files.
    ///
    /// Tree lookups use the caller's object snapshot and validate each visited tree. Blob, symlink
    /// and gitlink targets are retained without reads. Expanded leaves have zero stat words,
    /// stage zero, inherited assume-valid and skip-worktree set; intent-to-add stays false.
    /// Ordinary entries retain their flags and metadata. Empty trees produce no leaves.
    ///
    /// Only successful expansion replaces this draft and invalidates derived caches. An index
    /// without sparse entries is unchanged. This deliberately replaces sparse storage with a
    /// full entry list; publication is a separate [`super::IndexEdit::commit`] operation.
    /// Cancellation is checked between trees and entries; individual reads/parses/sorts may finish
    /// first. No staging, sparse-checkout matching or file materialization is performed.
    ///
    /// # Errors
    ///
    /// Missing, corrupt, wrong-kind or invalid trees, cancellation, extension-policy errors and
    /// exhausted aggregate bounds leave the original index untouched.
    pub fn expand_sparse(
        &mut self,
        objects: &Objects,
        limits: Limits,
        mut budget: SparseLimits,
        cancelled: &AtomicBool,
    ) -> Result<(), SparseError> {
        check(cancelled)?;
        if objects.object_format() != self.object_format() {
            ObjectId::null(objects.object_format())
                .require_format(self.object_format())
                .map_err(Error::from)?;
        }
        if !self.entries.iter().any(|e| e.mode == Mode::SparseDirectory) {
            return Ok(());
        }
        let mut output = Vec::new();
        let mut pending = Vec::new();
        for entry in &self.entries {
            check(cancelled)?;
            if entry.mode == Mode::SparseDirectory {
                charge(
                    &mut budget.max_path_bytes,
                    entry.path.len(),
                    "sparse path bytes",
                )?;
                pending.push((entry.path.clone(), entry.id, 0, entry.assume_valid));
            } else {
                push(&mut output, entry.clone(), limits)?;
            }
        }
        while let Some((prefix, id, depth, assume_valid)) = pending.pop() {
            check(cancelled)?;
            if depth > budget.max_depth {
                return Err(Error::Limit("sparse depth").into());
            }
            charge(&mut budget.max_trees, 1, "sparse tree reads")?;
            let read = ReadLimits {
                max_object_bytes: budget.read.max_object_bytes.min(budget.max_tree_bytes),
                ..budget.read
            };
            let object = objects
                .read_controlled(id, read, cancelled)
                .map_err(|source| SparseError::Read { id, source })?
                .ok_or(SparseError::Missing(id))?;
            if object.kind() != ObjectKind::Tree {
                return Err(SparseError::Kind(id));
            }
            charge(
                &mut budget.max_tree_bytes,
                object.data().len(),
                "sparse tree bytes",
            )?;
            let tree = Tree::parse(self.object_format(), object.data())
                .map_err(|source| SparseError::Tree { id, source })?;
            tree.validate()
                .map_err(|source| SparseError::Tree { id, source })?;
            charge(
                &mut budget.max_entries,
                tree.entries().len(),
                "sparse tree entries",
            )?;
            for child in tree.entries() {
                check(cancelled)?;
                let length = prefix
                    .len()
                    .checked_add(child.name.len())
                    .and_then(|n| n.checked_add(usize::from(child.mode == EntryMode::Tree)))
                    .ok_or(Error::Limit("path bytes"))?;
                if length > limits.max_path_bytes {
                    return Err(Error::Limit("path bytes").into());
                }
                charge(&mut budget.max_path_bytes, length, "sparse path bytes")?;
                let mut path = prefix.clone();
                path.extend_from_slice(&child.name);
                let mode = match child.mode {
                    EntryMode::Tree => {
                        path.push(b'/');
                        pending.push((path, child.id, depth + 1, assume_valid));
                        continue;
                    }
                    EntryMode::Blob => Mode::Regular,
                    EntryMode::Executable => Mode::Executable,
                    EntryMode::Symlink => Mode::Symlink,
                    EntryMode::Gitlink => Mode::Gitlink,
                };
                let mut entry = Entry::new(path, mode, child.id);
                entry.skip_worktree = true;
                entry.assume_valid = assume_valid;
                push(&mut output, entry, limits)?;
            }
        }
        check(cancelled)?;
        self.replace_entries(output, limits)?;
        Ok(())
    }
}
fn charge(remaining: &mut usize, count: usize, label: &'static str) -> Result<(), Error> {
    *remaining = remaining.checked_sub(count).ok_or(Error::Limit(label))?;
    Ok(())
}
fn push(entries: &mut Vec<Entry>, entry: Entry, limits: Limits) -> Result<(), Error> {
    if entries.len() >= limits.max_entries {
        return Err(Error::Limit("entries"));
    }
    entries.push(entry);
    Ok(())
}
fn check(cancelled: &AtomicBool) -> Result<(), SparseError> {
    if cancelled.load(Ordering::Relaxed) {
        Err(SparseError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{InitKind, ObjectFormat, Repository, TreeEntry};

    fn directory(id: ObjectId) -> Entry {
        let mut entry = Entry::new(b"outside/".to_vec(), Mode::SparseDirectory, id);
        entry.skip_worktree = true;
        entry
    }
    fn fixture(format: ObjectFormat) -> (tempfile::TempDir, Objects, Index) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("repo"), InitKind::Worktree).unwrap();
        let leaves = [
            EntryMode::Blob,
            EntryMode::Executable,
            EntryMode::Symlink,
            EntryMode::Gitlink,
        ];
        let tree = Tree::new(
            format,
            leaves
                .into_iter()
                .enumerate()
                .map(|(n, mode)| TreeEntry {
                    name: format!("leaf{n}").into_bytes(),
                    mode,
                    id: ObjectId::null(format),
                })
                .collect(),
        )
        .unwrap();
        let id = repo.loose_objects().write_tree(&tree).unwrap();
        let index = Index::new(format, vec![directory(id)], Limits::default()).unwrap();
        (
            root,
            repo.objects(crate::PackLimits::default()).unwrap(),
            index,
        )
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn expands_modes_without_reading_leaf_targets(#[case] format: ObjectFormat) {
        let (_root, objects, mut index) = fixture(format);
        index
            .expand_sparse(
                &objects,
                Limits::default(),
                SparseLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(
            index.entries().iter().map(|e| e.mode).collect::<Vec<_>>(),
            [
                Mode::Regular,
                Mode::Executable,
                Mode::Symlink,
                Mode::Gitlink
            ]
        );
        assert!(
            index
                .entries()
                .iter()
                .all(|e| e.skip_worktree && e.stat == super::super::Stat::default())
        );
        assert!(index.extensions().is_empty());
    }

    #[rstest]
    #[case::trees(SparseLimits {max_trees:0, ..SparseLimits::default()}, Limits::default(), false)]
    #[case::bytes(SparseLimits {max_tree_bytes:0, ..SparseLimits::default()}, Limits::default(), false)]
    #[case::visits(SparseLimits {max_entries:0, ..SparseLimits::default()}, Limits::default(), false)]
    #[case::paths(SparseLimits {max_path_bytes:0, ..SparseLimits::default()}, Limits::default(), false)]
    #[case::output(SparseLimits::default(), Limits { max_entries: 1, ..Limits::default()}, false)]
    #[case::path(SparseLimits::default(), Limits { max_path_bytes: 1, ..Limits::default()}, false)]
    #[case::cancel(SparseLimits::default(), Limits::default(), true)]
    fn bounded_failure_preserves_encoding(
        #[case] trees: SparseLimits,
        #[case] limits: Limits,
        #[case] cancel: bool,
    ) {
        let (_root, objects, mut index) = fixture(ObjectFormat::Sha256);
        let before = index.encode(Limits::default()).unwrap();
        assert!(
            index
                .expand_sparse(&objects, limits, trees, &AtomicBool::new(cancel))
                .is_err()
        );
        assert_eq!(index.encode(Limits::default()).unwrap(), before);
    }

    #[rstest]
    #[case::missing(false)]
    #[case::wrong_kind(true)]
    fn refuses_unusable_tree(#[case] present_blob: bool) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            ObjectFormat::Sha1,
            root.path().join("repo"),
            InitKind::Worktree,
        )
        .unwrap();
        let id = tree_target(&repo, present_blob);
        let mut index =
            Index::new(ObjectFormat::Sha1, vec![directory(id)], Limits::default()).unwrap();
        let before = index.encode(Limits::default()).unwrap();
        let objects = repo.objects(crate::PackLimits::default()).unwrap();
        let result = index.expand_sparse(
            &objects,
            Limits::default(),
            SparseLimits::default(),
            &AtomicBool::new(false),
        );
        assert!(matches!(
            result,
            Err(SparseError::Missing(_) | SparseError::Kind(_))
        ));
        assert_eq!(index.encode(Limits::default()).unwrap(), before);
    }
    fn tree_target(repo: &Repository, present: bool) -> ObjectId {
        if present {
            repo.loose_objects().write_blob(b"blob").unwrap()
        } else {
            ObjectId::null(repo.object_format())
        }
    }

    #[rstest]
    #[case::no_slash(b"outside", true, false, crate::index::Stage::Normal)]
    #[case::no_skip(b"outside/", false, false, crate::index::Stage::Normal)]
    #[case::intent(b"outside/", true, true, crate::index::Stage::Normal)]
    #[case::stage(b"outside/", true, false, crate::index::Stage::Ours)]
    #[case::empty(b"/", true, false, crate::index::Stage::Normal)]
    fn rejects_invalid_directory(
        #[case] path: &[u8],
        #[case] skip: bool,
        #[case] intent: bool,
        #[case] stage: crate::index::Stage,
    ) {
        let mut entry = directory(ObjectId::null(ObjectFormat::Sha1));
        entry.path = path.to_vec();
        entry.skip_worktree = skip;
        entry.intent_to_add = intent;
        entry.stage = stage;
        assert!(Index::new(ObjectFormat::Sha1, vec![entry], Limits::default()).is_err());
    }

    #[rstest]
    #[case::child(b"outside/a")]
    #[case::file(b"outside")]
    fn rejects_directory_collisions(#[case] path: &[u8]) {
        let id = ObjectId::null(ObjectFormat::Sha1);
        assert!(
            Index::new(
                ObjectFormat::Sha1,
                vec![directory(id), Entry::new(path.to_vec(), Mode::Regular, id)],
                Limits::default()
            )
            .is_err()
        );
    }
}
