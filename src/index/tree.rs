//! Canonical nested trees from validated index entries, with optional immutable publication.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::cache_tree::{self, Claim};
use super::{Index, Mode, Stage};
use crate::{EntryMode, ObjectId, Tree, TreeEntry};

impl Index {
    /// Stores the staged snapshot as nested trees with default pack-opening limits.
    ///
    /// Wraps object lookup, `Tree::new` and loose tree writes. Like `git write-tree`, rejects
    /// missing objects (except gitlinks), omits intent-to-add entries and retains collapsed sparse
    /// directory IDs. Object presence is checked, not recursive connectivity or object kind.
    ///
    /// # Errors
    ///
    /// Returns the completeness, lookup and storage errors described by [`Self::write_tree_with`].
    pub fn write_tree(&self, repo: &crate::Repository) -> Result<ObjectId, WriteTreeError> {
        self.write_tree_with(
            repo,
            crate::PackLimits::default(),
            &std::sync::atomic::AtomicBool::new(false),
        )
    }

    /// Stores the staged snapshot with explicit pack-opening limits and cancellation.
    ///
    /// A `TREE` (cache-tree) extension is used only as a claim that a directory's tree was already
    /// stored. Every tree identity is still computed from the entries. Where a valid record's
    /// entry count and identity match the computed directory, as Git's `write-tree` would reuse
    /// it, the stored tree's presence is checked once and neither it, its subtrees nor the blobs
    /// beneath it are rewritten or checked again: their presence is inferred from the stored
    /// tree, as Git does. A stale or malformed record falls back to checking and writing those
    /// directories normally, so it can cost time but never change the result.
    ///
    /// # Errors
    ///
    /// Rejects unmerged entries, non-placeholder null IDs, foreign formats, absent objects or
    /// cancellation. Lookup errors preserve their causes. All entries are checked before writing
    /// any trees. A storage failure or cancellation may leave immutable subtrees; retrying is safe
    /// under loose storage rules. The index and references remain unchanged. Callers needing a
    /// stable object set must exclude concurrent object retirement. Required object IDs are
    /// checked once per invocation, without retaining presence across calls; a blob removed from
    /// beneath a stored tree reused through the cache-tree is not detected. Memory grows with
    /// admitted index paths and distinct required IDs.
    pub fn write_tree_with(
        &self,
        repo: &crate::Repository,
        limits: crate::PackLimits,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ObjectId, WriteTreeError> {
        check_cancelled(cancel)?;
        ObjectId::null(self.format).require_format(repo.object_format())?;
        let objects = repo.objects_controlled(limits, crate::AlternateLimits::default(), cancel)?;
        let claims = match self.extensions.iter().find(|e| e.signature == *b"TREE") {
            Some(tree) => cache_tree::claims(self.format, &tree.data, &self.entries),
            None => Vec::new(),
        };
        let claims = claims
            .into_iter()
            .map(|claim| Ok(objects.contains(claim.id)?.then_some(claim)))
            .filter_map(Result::transpose)
            .collect::<Result<Vec<_>, WriteTreeError>>()?;
        let loose = repo.loose_objects();
        if !claims.is_empty() {
            match self.write_claimed(&loose, &objects, &claims, cancel) {
                Err(Attempt::Stale) => (),
                Err(Attempt::Failed(error)) => return Err(error),
                Ok(id) => return Ok(id),
            }
        }
        self.check_required(&objects, &[], cancel)?;
        self.build_tree(cancel, |tree, _, _| write(&loose, &tree))
    }

    // Checks required objects outside `claims`, then writes every tree outside them. A claimed
    // directory whose computed identity differs from its claim stops as stale.
    fn write_claimed(
        &self,
        loose: &crate::LooseObjects,
        objects: &crate::Objects,
        claims: &[Claim],
        cancel: &AtomicBool,
    ) -> Result<ObjectId, Attempt> {
        self.check_required(objects, claims, cancel)?;
        self.build_tree(cancel, |tree, start, end| {
            let index = claims.partition_point(|claim| claim.end <= start);
            let within = |claim: &&Claim| claim.start <= start && end <= claim.end;
            if let Some(claim) = claims.get(index).filter(within) {
                let id = tree.id();
                if (claim.start, claim.end) == (start, end) && id != claim.id {
                    return Err(Attempt::Stale);
                }
                return Ok(id);
            }
            // Directories and claims both follow path prefixes, so a written directory can only
            // contain whole claims. Anything else means the ranges disagree; distrust the claims.
            let overlapping = claims.get(index..).unwrap_or_default();
            if overlapping
                .iter()
                .take_while(|claim| claim.start < end)
                .any(|claim| claim.start < start || end < claim.end)
            {
                return Err(Attempt::Stale);
            }
            Ok(write(loose, &tree)?)
        })
    }

    // Rejects incomplete entries and absent objects outside claimed directories.
    fn check_required(
        &self,
        objects: &crate::Objects,
        claims: &[Claim],
        cancel: &AtomicBool,
    ) -> Result<(), WriteTreeError> {
        let mut present = crate::PeerIdSet::default();
        let mut claim = claims.iter().peekable();
        for (position, entry) in self.entries.iter().enumerate() {
            check_cancelled(cancel)?;
            if entry.stage != Stage::Normal {
                return Err(WriteTreeError::Unmerged {
                    path: entry.path.clone(),
                });
            }
            if entry.intent_to_add {
                continue;
            }
            if entry.id.is_null() {
                return Err(WriteTreeError::NullId {
                    path: entry.path.clone(),
                });
            }
            while claim.next_if(|claim| claim.end <= position).is_some() {}
            let claimed = claim.peek().is_some_and(|claim| claim.start <= position);
            if !claimed
                && entry.mode != Mode::Gitlink
                && present.insert(entry.id)
                && !objects.contains(entry.id)?
            {
                return Err(WriteTreeError::MissingObject {
                    path: entry.path.clone(),
                    id: entry.id,
                });
            }
        }
        Ok(())
    }

    /// Computes the canonical Git tree identity without reading or writing objects.
    ///
    /// Like `git write-tree`, omits intent-to-add entries and uses collapsed sparse directory
    /// identities directly. Stat data, assume-valid, skip-worktree and optional extensions do not
    /// affect the result. Returns `None` for unmerged entries or non-placeholder null IDs.
    /// Object existence and type are not checked; this is an identity calculation, not proof of
    /// repository connectivity. Memory is proportional to the admitted index, and directory
    /// nesting uses an explicit stack rather than recursion.
    ///
    /// ```
    /// use girt::index::Index;
    /// use girt::{ObjectFormat, Tree};
    /// let index = Index::empty(ObjectFormat::Sha1);
    /// assert_eq!(
    ///     index.tree_id(),
    ///     Some(Tree::new(ObjectFormat::Sha1, vec![])?.id())
    /// );
    /// # Ok::<(), girt::TreeError>(())
    /// ```
    pub fn tree_id(&self) -> Option<ObjectId> {
        self.build_tree::<WriteTreeError>(&AtomicBool::new(false), |tree, _, _| Ok(tree.id()))
            .ok()
    }

    /// Computes [`Self::tree_id`] and keeps every tree built along the way, by identity.
    ///
    /// Each identity is the hash of its own tree, so a lookup can stand in for reading that tree
    /// from object storage. Returns `None` where [`Self::tree_id`] does.
    /// Cancellation also returns `None`; the caller checks `cancel` afterwards.
    pub(crate) fn trees(&self, cancel: &AtomicBool) -> Option<(ObjectId, HashMap<ObjectId, Tree>)> {
        let mut trees = HashMap::new();
        let root = self
            .build_tree::<WriteTreeError>(cancel, |tree, _, _| {
                let id = tree.id();
                trees.insert(id, tree);
                Ok(id)
            })
            .ok()?;
        Some((root, trees))
    }

    // Index paths are already sorted and validated. Close each directory once when its prefix
    // ends, sharing canonical construction between identity calculation and checked publication.
    fn build_tree<E: From<WriteTreeError>>(
        &self,
        cancel: &AtomicBool,
        mut publish: impl FnMut(Tree, usize, usize) -> Result<ObjectId, E>,
    ) -> Result<ObjectId, E> {
        let mut root = Directory {
            name: &[],
            start: 0,
            entries: Vec::new(),
        };
        let mut directories: Vec<Directory<'_>> = Vec::new();
        // One past the last entry placed in a tree. Directories end there, so omitted
        // intent-to-add entries never widen the range a directory reports.
        let mut included = 0;
        for (position, entry) in self.entries.iter().enumerate() {
            check_cancelled(cancel)?;
            if entry.stage != Stage::Normal {
                return Err(WriteTreeError::Unmerged {
                    path: entry.path.clone(),
                }
                .into());
            }
            if entry.intent_to_add {
                continue;
            }
            if entry.id.is_null() {
                return Err(WriteTreeError::NullId {
                    path: entry.path.clone(),
                }
                .into());
            }
            let path = entry.path.strip_suffix(b"/").unwrap_or(&entry.path);
            let mut components: Vec<_> = path.split(|&byte| byte == b'/').collect();
            let name =
                components
                    .pop()
                    .ok_or(WriteTreeError::from(crate::tree::TreeWriteError::from(
                        crate::TreeError::InvalidName,
                    )))?;
            let common = directories
                .iter()
                .zip(&components)
                .take_while(|(directory, component)| directory.name == **component)
                .count();
            while directories.len() > common {
                if let Some(directory) = directories.pop() {
                    let tree = directory.finish(self.format, included, &mut publish)?;
                    directories
                        .last_mut()
                        .unwrap_or(&mut root)
                        .entries
                        .push(tree);
                }
            }
            for &name in components.iter().skip(common) {
                directories.push(Directory {
                    name,
                    start: position,
                    entries: Vec::new(),
                });
            }
            let mode = match entry.mode {
                Mode::Regular => EntryMode::Blob,
                Mode::Executable => EntryMode::Executable,
                Mode::Symlink => EntryMode::Symlink,
                Mode::Gitlink => EntryMode::Gitlink,
                Mode::SparseDirectory => EntryMode::Tree,
            };
            directories
                .last_mut()
                .unwrap_or(&mut root)
                .entries
                .push(TreeEntry {
                    name: name.to_vec(),
                    mode,
                    id: entry.id,
                });
            included = position + 1;
        }
        let end = included;
        while let Some(directory) = directories.pop() {
            let tree = directory.finish(self.format, end, &mut publish)?;
            directories
                .last_mut()
                .unwrap_or(&mut root)
                .entries
                .push(tree);
        }
        check_cancelled(cancel)?;
        Ok(root.finish(self.format, end, &mut publish)?.id)
    }
}

/// An index snapshot is incomplete, its objects are unavailable, or tree publication failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WriteTreeError {
    /// An index path has conflict stages.
    #[error("index contains an unmerged path")]
    Unmerged {
        /// Conflicted byte path.
        path: Vec<u8>,
    },
    /// A staged entry has no usable object ID.
    #[error("index contains a null object ID")]
    NullId {
        /// Incomplete byte path.
        path: Vec<u8>,
    },
    /// A referenced object is absent from the repository and alternates.
    #[error("index object {id} is missing")]
    MissingObject {
        /// Staged byte path.
        path: Vec<u8>,
        /// Absent object identity.
        id: ObjectId,
    },
    /// Object lookup or pack opening failed.
    #[error(transparent)]
    Read(#[from] crate::ObjectReadError),
    /// Canonical tree construction or immutable publication failed.
    #[error(transparent)]
    Write(#[from] crate::tree::TreeWriteError),
    /// Index and destination object formats differ.
    #[error(transparent)]
    ObjectFormat(#[from] crate::ObjectFormatError),
    /// Cancellation was requested. No index or reference changed.
    #[error("writing index tree cancelled")]
    Cancelled,
}

// Checked writing stops early when a cache-tree claim proves stale, then retries without claims.
enum Attempt {
    Failed(WriteTreeError),
    Stale,
}

impl From<WriteTreeError> for Attempt {
    fn from(error: WriteTreeError) -> Self {
        Self::Failed(error)
    }
}

fn write(loose: &crate::LooseObjects, tree: &Tree) -> Result<ObjectId, WriteTreeError> {
    loose
        .write_tree(tree)
        .map_err(crate::tree::TreeWriteError::from)
        .map_err(WriteTreeError::from)
}

fn check_cancelled(cancel: &AtomicBool) -> Result<(), WriteTreeError> {
    if cancel.load(Ordering::Relaxed) {
        Err(WriteTreeError::Cancelled)
    } else {
        Ok(())
    }
}

// An open directory and the position of its first index entry.
struct Directory<'a> {
    name: &'a [u8],
    start: usize,
    entries: Vec<TreeEntry>,
}

impl Directory<'_> {
    // `end` is one past the directory's last index entry.
    fn finish<E: From<WriteTreeError>>(
        self,
        format: crate::ObjectFormat,
        end: usize,
        publish: &mut impl FnMut(Tree, usize, usize) -> Result<ObjectId, E>,
    ) -> Result<TreeEntry, E> {
        // Validated index entries are unique, well-formed and already in Git tree order.
        let tree = Tree::from_ordered(format, self.entries);
        let id = publish(tree, self.start, end)?;
        Ok(TreeEntry {
            name: self.name.to_vec(),
            mode: EntryMode::Tree,
            id,
        })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::ObjectFormat;
    use crate::index::{Entry, Limits};

    #[rstest]
    #[case::unmerged(Stage::Ours, ObjectId::for_blob(ObjectFormat::Sha1, b"x"))]
    #[case::null(Stage::Normal, ObjectId::null(ObjectFormat::Sha1))]
    fn incomplete_index_has_no_tree_identity(#[case] stage: Stage, #[case] id: ObjectId) {
        let index = Index::new(
            ObjectFormat::Sha1,
            vec![Entry::new(b"file".to_vec(), Mode::Regular, id).with_stage(stage)],
            Limits::default(),
        )
        .unwrap();
        assert_eq!(index.tree_id(), None);
    }

    #[test]
    fn null_intent_to_add_is_omitted() {
        let format = ObjectFormat::Sha1;
        let index = Index::new(
            format,
            vec![
                Entry::new(
                    b"future/file".to_vec(),
                    Mode::Regular,
                    ObjectId::null(format),
                )
                .with_intent_to_add(true),
            ],
            Limits::default(),
        )
        .unwrap();
        assert_eq!(index.tree_id(), Index::empty(format).tree_id());
    }
}
