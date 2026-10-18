use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{AppliedFile, AppliedTree, FileOperation, Patch, TreeError, TreeLimits, TreeOptions};
use crate::index::{Entry, Index, Mode, Stage};
use crate::{EntryMode, Objects, Tree};

/// Bounds for the index snapshot and shared content/tree planner.
///
/// The default tree path budget is 16 MiB across the whole index and directory paths, not only
/// patched paths. Large indexes can exhaust it before the independent one-million-entry bound;
/// raise [`TreeLimits::max_path_bytes`] explicitly for those repositories.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct IndexLimits {
    /// Input bounds are checked before cloning; the resulting draft is checked before return.
    pub index: crate::index::Limits,
    /// Bounds for all index paths/directories and generated objects, plus affected blob reads.
    pub tree: TreeLimits,
}

/// Unpublished index draft and generated objects. Computing it changes no stored data.
#[derive(Clone, Debug)]
pub struct AppliedIndex {
    index: Index,
    tree: AppliedTree,
}

impl AppliedIndex {
    /// Proposed index. Unaffected entries retain their flags and cached stat data.
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Takes the draft for explicit publication through [`crate::index::IndexEdit::replace_index`].
    /// Install generated objects before committing the index edit.
    pub fn into_index(self) -> Index {
        self.index
    }

    /// Proposed root tree, computed without installing any objects.
    pub fn tree(&self) -> &Tree {
        self.tree.tree()
    }

    /// Mapped per-file and per-hunk results in patch order.
    pub fn files(&self) -> &[AppliedFile] {
        self.tree.files()
    }

    /// Installs immutable generated objects without publishing the index draft.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError`] on format mismatch or storage failure. A failed installation may leave
    /// additive objects; it never changes the stored index or working files.
    pub fn install_objects(&self, loose: &crate::LooseObjects) -> Result<(), TreeError> {
        self.tree.install_objects(loose)
    }
}

/// Index patch preparation failed without changing objects, index storage or working files.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IndexError {
    /// The shared path/content/tree planner failed.
    #[error("preparing index patch")]
    Tree(#[from] TreeError),
    /// Input or draft index limits, entries or extensions prevent replacement.
    #[error("validating patch index")]
    Index(#[from] crate::index::Error),
    /// Conflict stages, sparse directories or intent-to-add placeholders need an explicit policy.
    #[error("unsupported patch index entry")]
    UnsupportedEntry {
        /// Repository-relative byte path of the entry needing a policy.
        path: Vec<u8>,
        /// Unsupported index state.
        reason: &'static str,
    },
}

impl Patch {
    /// Computes an index-only patch with default path mapping and bounds.
    ///
    /// Wraps [`Self::apply_index_with`] with defaults and no cancellation. No working tree is
    /// required, and computing successfully serves as a check without publication.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] for unsupported index states, invalid paths, failed hunks or limits.
    ///
    /// ```
    /// use girt::apply::Patch;
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let temporary = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     temporary.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let patch = Patch::parse(b"--- /dev/null\n+++ b/file\n@@ -0,0 +1 @@\n+hello\n")?;
    /// let mut edit = repo.edit_index(Default::default())?;
    /// let applied = patch.apply_index(&repo.objects()?, edit.index())?;
    /// let expected = applied.tree().id();
    ///
    /// applied.install_objects(&repo.loose_objects())?;
    /// edit.replace_index(applied.into_index())?;
    /// edit.commit()?;
    /// assert_eq!(
    ///     repo.read_index(Default::default())?.unwrap().tree_id(),
    ///     Some(expected)
    /// );
    /// # Ok::<(), girt::Error>(())
    /// ```
    pub fn apply_index(
        &self,
        objects: &Objects,
        index: &Index,
    ) -> Result<AppliedIndex, IndexError> {
        self.apply_index_with(
            objects,
            index,
            TreeOptions::default(),
            IndexLimits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Computes all edits against the original index before returning an owned draft.
    ///
    /// Reuses tree path/mode/preimage checks, including rename cycles and copy source retention.
    /// Every index entry is bounded and visited; only affected blobs are read. Unaffected gitlinks
    /// are retained, but patches cannot modify them. Conflict stages, sparse directories and
    /// intent-to-add entries anywhere in the index are explicitly refused. Expand sparse entries
    /// and resolve conflicts first. Touched entries receive fresh flags/stat data; unaffected
    /// entries retain theirs. Fresh flags include clearing skip-worktree and assume-valid on
    /// touched entries, matching Git's cached application. Derived extensions are invalidated
    /// using [`Index::replace_entries`], which refuses unknown optional extensions unless the
    /// caller discarded them explicitly beforehand. Resolved split indexes produce standalone
    /// drafts.
    ///
    /// To publish, compute against an [`crate::index::IndexEdit::index`] snapshot while holding its
    /// lock, install generated objects, then replace and commit the index draft. Working files and
    /// refs are never changed. [`Self::reversed`] selects reverse application before planning.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] without a partial draft or external effects. Cancellation is checked
    /// between entries and files; individual bounded content operations cannot be interrupted.
    pub fn apply_index_with(
        &self,
        objects: &Objects,
        index: &Index,
        options: TreeOptions,
        limits: IndexLimits,
        cancel: &AtomicBool,
    ) -> Result<AppliedIndex, IndexError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(TreeError::Cancelled.into());
        }
        index.encoded_len(limits.index)?;
        for entry in index.entries() {
            if cancel.load(Ordering::Relaxed) {
                return Err(TreeError::Cancelled.into());
            }
            let reason = if entry.stage != Stage::Normal {
                Some("unmerged stage")
            } else if entry.intent_to_add {
                Some("intent-to-add placeholder")
            } else if entry.id.is_null() {
                Some("null object identity")
            } else if entry.mode == Mode::SparseDirectory {
                Some("sparse directory")
            } else {
                None
            };
            if let Some(reason) = reason {
                return Err(IndexError::UnsupportedEntry {
                    path: entry.path.clone(),
                    reason,
                });
            }
        }
        let tree =
            super::tree::apply_index_tree(self, objects, index, options, limits.tree, cancel)?;
        let removals: BTreeSet<_> = self
            .files()
            .iter()
            .zip(tree.files())
            .filter(|(patch, _)| patch.operation() != FileOperation::Copy)
            .filter_map(|(_, file)| file.source_path.as_deref())
            .collect();
        let mut entries = Vec::new();
        for entry in index.entries() {
            if cancel.load(Ordering::Relaxed) {
                return Err(TreeError::Cancelled.into());
            }
            if !removals.contains(entry.path.as_slice()) {
                entries.push(entry.clone());
            }
        }
        for file in tree.files() {
            if cancel.load(Ordering::Relaxed) {
                return Err(TreeError::Cancelled.into());
            }
            if let Some(value) = file.new {
                let mode = match value.mode {
                    EntryMode::Blob => Mode::Regular,
                    EntryMode::Executable => Mode::Executable,
                    EntryMode::Symlink => Mode::Symlink,
                    _ => return Err(TreeError::Mode.into()),
                };
                entries.push(Entry::new(file.path.clone(), mode, value.id));
            }
        }
        let mut draft = index.clone();
        draft.make_standalone(limits.index)?;
        draft.replace_entries(entries, limits.index)?;

        Ok(AppliedIndex { index: draft, tree })
    }
}
