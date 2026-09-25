//! Recursive structural comparison of SHA-1 trees.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{
    EntryMode, ObjectId, ObjectKind, ObjectReadError, Objects, ReadLimits, Tree, TreeError,
};

/// The identity and mode recorded for a leaf path in a tree.
///
/// Regular files, executable files, symlinks and gitlinks are leaves. IDs describe content
/// identity independently of mode: an executable-bit or symlink transition can retain an ID.
/// Gitlinks name commits in another repository, not trees to traverse locally.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeValue {
    /// Referenced object identity; comparison does not read or verify leaf targets.
    pub id: ObjectId,
    /// Recorded entry mode.
    pub mode: EntryMode,
}

/// One added, removed or changed leaf path.
///
/// Results from [`Objects::compare_trees`] have at least one side and never have equal sides.
/// `old == None` means addition; `new == None` means removal; two present sides mean change.
/// Compare IDs to distinguish content identity changes from mode/type changes. A mode change
/// alone still produces a record. Trees themselves are omitted, including empty directories.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeChange {
    /// Relative slash-separated Git path bytes, without UTF-8 conversion or a trailing slash.
    pub path: Vec<u8>,
    /// Previous leaf, absent for additions.
    pub old: Option<TreeValue>,
    /// Replacement leaf, absent for removals.
    pub new: Option<TreeValue>,
}

/// Input and output bounds for one recursive tree comparison.
///
/// Counters charge occurrences, not distinct identities: the same subtree at two paths may be
/// read twice. Equal-ID tree pairs consume no read/entry budget. Bounds are checked before queue
/// growth or result insertion; object bytes are bounded before decoding and entries before pairing.
/// Parsing one bounded payload still allocates proportional entry/name storage before the entry
/// count check. Memory is O(total tree bytes + entries + generated path bytes + changes), plus
/// the existing pack snapshot and per-read decoding workspace. These are not exact heap limits.
#[derive(Clone, Copy, Debug)]
pub struct TreeCompareLimits {
    /// Maximum tree reads across both sides (default 100,000).
    pub max_trees: usize,
    /// Maximum sum of tree payload bytes read across both sides (default 256 MiB).
    pub max_tree_bytes: usize,
    /// Maximum sum of entries in trees read across both sides (default 1,000,000).
    pub max_entries: usize,
    /// Maximum directory depth below the root; zero permits root leaves only (default 1,024).
    pub max_depth: usize,
    /// Maximum sum of generated full path lengths, including unchanged entries (default 64 MiB).
    pub max_path_bytes: usize,
    /// Maximum number of output records (default 1,000,000).
    pub max_changes: usize,
    /// Independent storage reconstruction bounds for each read.
    pub read: ReadLimits,
}

impl Default for TreeCompareLimits {
    fn default() -> Self {
        Self {
            max_trees: 100_000,
            max_tree_bytes: 256 * 1024 * 1024,
            max_entries: 1_000_000,
            max_depth: 1_024,
            max_path_bytes: 64 * 1024 * 1024,
            max_changes: 1_000_000,
            read: ReadLimits::default(),
        }
    }
}

impl Objects {
    /// Compares two tree IDs recursively, using `None` for an empty side.
    ///
    /// Returns leaf additions, removals and changes in ascending lexicographic **raw full-path
    /// byte order** (shorter prefix first). Git tree ordering is validated before pairing entries
    /// by byte name: files and directories with the same name must pair even when their positions
    /// in Git order differ. A file/directory replacement yields a removal/addition at the file
    /// path and additions/removals below the directory. Empty directories yield no leaf records.
    /// No rename/copy detection, text diff, path filtering, index or worktree access occurs.
    ///
    /// Equal subtree IDs, including equal roots, are trusted and skipped without any read. This
    /// establishes structural identity, **not validity or existence**. Other encountered trees
    /// are identity-verified by storage, parsed and structurally validated. Blob, symlink and
    /// gitlink targets are never read, even when changed; dangling or wrong-kind leaf targets
    /// therefore remain unvalidated. Gitlinks are not interpreted as local subtrees.
    ///
    /// I/O and comparison are synchronous and read-only. Traversal uses an explicit stack, not
    /// recursion. Set `cancel` from another thread and leave it set until return. Checks occur
    /// before starting (even for equal roots), between tree reads and entry processing, and before
    /// and after sorting output. Individual reads, parsing, validation and sorting cannot be
    /// interrupted; cancellation is cooperative, without a wall-clock guarantee. Callers using
    /// an async runtime should run the whole operation on a caller-bounded blocking worker.
    ///
    /// # Errors
    ///
    /// Returns [`TreeCompareError`] for a visited missing, wrong-kind, corrupt, malformed or
    /// structurally invalid tree, cancellation, or an exhausted bound. Read/parse failures include
    /// the tree ID and byte path (empty for the root). No partial results or filesystem mutations
    /// are produced. SHA-1 and the five modes accepted by [`Tree::parse`] are supported.
    ///
    /// ```no_run
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::{ObjectId, PackLimits, Repository, TreeCompareLimits};
    /// let repo = Repository::open("/path/to/repository")?;
    /// let objects = repo.objects(PackLimits::default())?;
    /// let tree: ObjectId = "4b825dc642cb6eb9a060e54bf8d69288fbee4904".parse()?;
    /// let changes = objects.compare_trees(
    ///     None,
    ///     Some(tree),
    ///     TreeCompareLimits::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    /// assert!(changes.is_empty()); // The specified ID is Git's empty tree.
    ///
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn compare_trees(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        limits: TreeCompareLimits,
        cancel: &AtomicBool,
    ) -> Result<Vec<TreeChange>, TreeCompareError> {
        let mut budget = Budget {
            remaining: limits,
            cancel,
        };
        budget.check()?;
        let mut pending = vec![Directory {
            path: vec![],
            old,
            new,
            depth: 0,
        }];
        let mut changes = Vec::new();
        while let Some(directory) = pending.pop() {
            budget.check()?;
            if directory.old == directory.new {
                continue;
            }
            let old = budget.read(self, directory.old, &directory.path)?;
            let new = budget.read(self, directory.new, &directory.path)?;
            // Pair by literal name, not Git's mode-dependent ordering. This handles a file/tree
            // replacement even when siblings sort between its old and new positions.
            let pairs = pair_entries(&old, &new, cancel)?;
            for (name, (old, new)) in pairs {
                budget.check()?;
                let length = directory
                    .path
                    .len()
                    .checked_add(name.len())
                    .and_then(|n| n.checked_add(usize::from(!directory.path.is_empty())))
                    .ok_or(TreeCompareError::Limit("path bytes"))?;
                charge(&mut budget.remaining.max_path_bytes, length, "path bytes")?;
                let mut path = Vec::with_capacity(length);
                path.extend_from_slice(&directory.path);
                if !path.is_empty() {
                    path.push(b'/');
                }
                path.extend_from_slice(name);

                let old_tree = tree_id(old);
                let new_tree = tree_id(new);
                if old_tree != new_tree {
                    if directory.depth == limits.max_depth {
                        return Err(TreeCompareError::Limit("depth"));
                    }
                    pending.push(Directory {
                        path: path.clone(),
                        old: old_tree,
                        new: new_tree,
                        depth: directory.depth + 1,
                    });
                }
                let old = old.filter(|value| value.mode != EntryMode::Tree);
                let new = new.filter(|value| value.mode != EntryMode::Tree);
                if old != new {
                    charge(&mut budget.remaining.max_changes, 1, "changes")?;
                    changes.push(TreeChange { path, old, new });
                }
            }
        }
        budget.check()?;
        changes.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        budget.check()?;
        Ok(changes)
    }
}

fn tree_id(value: Option<TreeValue>) -> Option<ObjectId> {
    value
        .filter(|value| value.mode == EntryMode::Tree)
        .map(|value| value.id)
}

type EntryPairs<'a> = BTreeMap<&'a [u8], (Option<TreeValue>, Option<TreeValue>)>;

/// Pure name alignment; borrowed names live only until this directory has been processed.
fn pair_entries<'a>(
    old: &'a Tree,
    new: &'a Tree,
    cancel: &AtomicBool,
) -> Result<EntryPairs<'a>, TreeCompareError> {
    let mut pairs: EntryPairs<'_> = BTreeMap::new();
    for entry in old.entries() {
        check_cancel(cancel)?;
        pairs.entry(&entry.name).or_default().0 = Some(TreeValue {
            id: entry.id,
            mode: entry.mode,
        });
    }
    for entry in new.entries() {
        check_cancel(cancel)?;
        pairs.entry(&entry.name).or_default().1 = Some(TreeValue {
            id: entry.id,
            mode: entry.mode,
        });
    }
    Ok(pairs)
}

struct Directory {
    path: Vec<u8>,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    depth: usize,
}

struct Budget<'a> {
    remaining: TreeCompareLimits,
    cancel: &'a AtomicBool,
}

impl Budget<'_> {
    fn check(&self) -> Result<(), TreeCompareError> {
        check_cancel(self.cancel)
    }

    fn read(
        &mut self,
        objects: &Objects,
        id: Option<ObjectId>,
        path: &[u8],
    ) -> Result<Tree, TreeCompareError> {
        self.check()?;
        let Some(id) = id else {
            return Ok(Tree::new(vec![]).expect("empty tree"));
        };
        charge(&mut self.remaining.max_trees, 1, "trees")?;
        let mut read = self.remaining.read;
        read.max_object_bytes = read.max_object_bytes.min(self.remaining.max_tree_bytes);
        let object = objects
            .read(id, read)
            .map_err(|source| TreeCompareError::Read {
                id,
                path: path.to_vec(),
                source,
            })?
            .ok_or_else(|| TreeCompareError::Missing {
                id,
                path: path.to_vec(),
            })?;
        self.check()?;
        if object.kind() != ObjectKind::Tree {
            return Err(TreeCompareError::NotTree {
                id,
                path: path.to_vec(),
                actual: object.kind(),
            });
        }
        charge(
            &mut self.remaining.max_tree_bytes,
            object.data().len(),
            "tree bytes",
        )?;
        let invalid = |source| TreeCompareError::Invalid {
            id,
            path: path.to_vec(),
            source,
        };
        let tree = Tree::parse(object.data()).map_err(invalid)?;
        charge(
            &mut self.remaining.max_entries,
            tree.entries().len(),
            "entries",
        )?;
        tree.validate().map_err(invalid)?;
        self.check()?;
        Ok(tree)
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), TreeCompareError> {
    if cancel.load(Ordering::Relaxed) {
        Err(TreeCompareError::Cancelled)
    } else {
        Ok(())
    }
}

fn charge(remaining: &mut usize, count: usize, name: &'static str) -> Result<(), TreeCompareError> {
    *remaining = remaining
        .checked_sub(count)
        .ok_or(TreeCompareError::Limit(name))?;
    Ok(())
}

/// A structural comparison failed; no partial result is returned.
#[derive(Debug, thiserror::Error)]
pub enum TreeCompareError {
    /// A tree to be traversed is absent.
    #[error("missing tree {id} at {path:?}")]
    Missing {
        /// Expected tree identity.
        id: ObjectId,
        /// Relative directory path bytes; empty for a root.
        path: Vec<u8>,
    },
    /// A visited tree reference resolves to a different object kind.
    #[error("expected tree {id} at {path:?}, found {actual:?}")]
    NotTree {
        /// Expected tree identity.
        id: ObjectId,
        /// Relative directory path bytes; empty for a root.
        path: Vec<u8>,
        /// Object kind found in storage.
        actual: ObjectKind,
    },
    /// Storage, identity verification or per-read decoding failed.
    ///
    /// The effective object-byte limit also includes the remaining cumulative tree-byte budget.
    #[error("reading tree {id} at {path:?}: {source}")]
    Read {
        /// Tree being read.
        id: ObjectId,
        /// Relative directory path bytes; empty for a root.
        path: Vec<u8>,
        /// Underlying storage failure.
        #[source]
        source: ObjectReadError,
    },
    /// A visited tree has unsupported syntax, invalid names, duplicates or invalid Git ordering.
    #[error("invalid tree {id} at {path:?}: {source}")]
    Invalid {
        /// Tree being parsed or validated.
        id: ObjectId,
        /// Relative directory path bytes; empty for a root.
        path: Vec<u8>,
        /// Parsing or structural validation failure.
        #[source]
        source: TreeError,
    },
    /// A named traversal or output bound was exceeded.
    #[error("tree comparison limit exceeded: {0}")]
    Limit(&'static str),
    /// The caller requested cancellation at a cooperative checkpoint.
    #[error("tree comparison cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests;
