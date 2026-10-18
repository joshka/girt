use std::sync::atomic::{AtomicBool, Ordering};

use super::memory::Memory;
use super::{
    ContentHunk, ContentMerge, MarkerOptions, RenameError, TreeError, TreeMerge, TreeMergeOptions,
};
use crate::{EntryMode, ObjectId, Objects, Repository, Tree, index};

/// Bounds for preparing marker objects and a staged index, without writing them.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct PreparationLimits {
    /// Tree traversal bounds; rename inference is not performed during preparation.
    pub trees: TreeMergeOptions,
    /// Index entry/encoding bounds.
    pub index: index::Limits,
    /// Conservative aggregate rendered conflict-payload bound (default 64 MiB).
    ///
    /// Counts all retained sides and marker lines before rendering, so it can exceed actual
    /// output when common boundaries are removed. Labels and widths are included.
    pub max_marker_bytes: usize,
}

impl Default for PreparationLimits {
    fn default() -> Self {
        Self {
            trees: TreeMergeOptions::default(),
            index: index::Limits::default(),
            max_marker_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Read-only marker-tree and staged-index plan, prepared without external mutation.
///
/// [`Repository::checkout_merge`] installs its generated objects and materializes it through the
/// guarded checkout engine. The index retains original conflict IDs while working files receive
/// marker bytes. The plan does not authorize overwriting a dirty or changed baseline.
///
/// ```no_run
/// use girt::merge::MarkerOptions;
/// use girt::{ObjectId, Repository};
/// fn combine(repo: &Repository, ours: ObjectId, theirs: ObjectId) -> girt::Result<()> {
///     let objects = repo.objects()?;
///     let merged = objects.merge_commits(ours, theirs)?;
///     let baseline = objects.read_commit(ours)?.tree();
///     let plan = merged
///         .result
///         .prepare_checkout(&objects, &MarkerOptions::default())?;
///     // Requires an exclusively owned, clean checkout of ours.
///     let report = repo.checkout_merge(Some(baseline), &plan)?;
///     assert!(report.index_published);
///     Ok(())
/// }
/// ```
#[derive(Clone, Debug)]
pub struct MergeCheckout {
    tree: ObjectId,
    index: index::Index,
    blobs: Vec<(ObjectId, Vec<u8>)>,
    trees: Vec<Tree>,
}

impl MergeCheckout {
    // Stash application can restore working files while preserving a different staged snapshot.
    pub(crate) fn with_index(mut self, index: index::Index) -> Result<Self, PublicationError> {
        if index.object_format() != self.index.object_format() {
            return Err(PublicationError::ObjectFormat);
        }
        self.index = index;
        Ok(self)
    }

    /// Identity of the marker-bearing working-tree snapshot; not proof of object installation.
    pub fn tree_id(&self) -> ObjectId {
        self.tree
    }

    /// Validated index with resolved stage-zero entries and original unresolved stages 1–3.
    ///
    /// Clone this draft when the caller needs an index-only publication through
    /// [`crate::index::IndexEdit::replace_index`]. It does not contain marker blob IDs in stages.
    pub fn index(&self) -> &index::Index {
        &self.index
    }

    /// Installs generated blobs and trees without publishing an index, working files or refs.
    ///
    /// Existing objects are retained. A failure can leave some generated objects installed;
    /// installation is additive and can be retried. Index-only callers must complete this step
    /// before publishing [`Self::index`].
    ///
    /// # Errors
    ///
    /// Returns format or object-write errors; no previously stored object is removed.
    pub fn install_objects(&self, loose: &crate::LooseObjects) -> Result<(), PublicationError> {
        self.install_objects_controlled(loose, &AtomicBool::new(false))
    }

    /// Installs generated objects while checking cancellation between individual writes.
    ///
    /// # Errors
    ///
    /// Returns [`Self::install_objects`]'s errors or cancellation. Completed additive writes
    /// remain.
    pub fn install_objects_controlled(
        &self,
        loose: &crate::LooseObjects,
        cancel: &AtomicBool,
    ) -> Result<(), PublicationError> {
        check(cancel)?;
        if loose.object_format() != self.index.object_format() {
            return Err(PublicationError::ObjectFormat);
        }
        for (_, bytes) in &self.blobs {
            check(cancel)?;
            loose.write_blob(bytes)?;
        }
        for tree in &self.trees {
            check(cancel)?;
            loose.write_tree(tree)?;
        }
        Ok(())
    }
}

/// Preparation or publication failed; working-file failure retains the checkout partial report.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PublicationError {
    /// Candidate tree traversal could not complete.
    #[error("preparing merge paths")]
    Paths(#[from] RenameError),
    /// A directory conflict or generated tree could not be represented.
    #[error("preparing merge conflict objects")]
    Tree(#[from] TreeError),
    /// The staged index failed validation.
    #[error("preparing merge index")]
    Index(#[from] index::Error),
    /// Additive object installation failed; already installed objects may remain.
    #[error("installing merge objects")]
    Objects(#[from] crate::loose::Error),
    /// Checkout failed; inspect completed mutations, publication and cleanup before recovery.
    #[error("publishing merge checkout")]
    Checkout(#[source] Box<crate::checkout::Failure>),
    /// The plan and selected repository/reader use different object formats.
    #[error("merge publication object format mismatch")]
    ObjectFormat,
    /// Caller-edited generated bytes do not have the identity claimed by the merge result.
    #[error("generated merge blob identity mismatch")]
    ObjectIdentity,
    /// A configured preparation/output bound was reached before publication.
    #[error("merge preparation limit: {0}")]
    Limit(&'static str),
    /// Cancelled before mutable checkout; some additive object writes may already have completed.
    #[error("merge publication cancelled")]
    Cancelled,
}

impl TreeMerge {
    /// Prepares conflict working bytes and original index stages with default bounds.
    ///
    /// See [`Self::prepare_checkout_with`] for byte, object and failure contracts.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError`] without writing objects, index or working files.
    pub fn prepare_checkout(
        &self,
        objects: &Objects,
        markers: &MarkerOptions,
    ) -> Result<MergeCheckout, PublicationError> {
        self.prepare_checkout_with(
            objects,
            markers,
            PreparationLimits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Prepares a marker-tree snapshot and a staged index without changing external state.
    ///
    /// Content conflicts render with the supplied marker policy; binary/structural conflicts
    /// retain provisional working content. Original leaf entries become stages 1–3, including
    /// distinct rename/rename paths. Resolved entries remain stage zero. Directory conflicts and
    /// stored empty subdirectories are refused. Generated input objects are copied into the plan.
    /// Labels are emitted verbatim; their conservative output cost is bounded before rendering.
    /// The resulting plan is immutable; baseline/worktree validation occurs during publication.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError`] for input/traversal failures, directory stage conversion,
    /// invalid index paths, mixed formats, output bounds or cancellation. No partial plan escapes.
    pub fn prepare_checkout_with(
        &self,
        objects: &Objects,
        markers: &MarkerOptions,
        limits: PreparationLimits,
        cancel: &AtomicBool,
    ) -> Result<MergeCheckout, PublicationError> {
        check(cancel)?;
        if objects.object_format() != self.tree.object_format() {
            return Err(PublicationError::ObjectFormat);
        }
        let mut memory = Memory::default();
        for tree in &self.trees {
            if tree.object_format() != objects.object_format() {
                return Err(PublicationError::ObjectFormat);
            }
            if !memory.trees.contains_key(&tree.id()) {
                memory.order.push(tree.id());
            }
            memory.trees.insert(tree.id(), tree.clone());
        }
        for (id, bytes) in &self.blobs {
            check(cancel)?;
            if *id != ObjectId::for_blob(objects.object_format(), bytes) {
                return Err(PublicationError::ObjectIdentity);
            }
            memory.blobs.insert(*id, bytes.clone());
        }
        if !memory.trees.contains_key(&self.tree.id()) {
            memory.order.push(self.tree.id());
        }
        memory.trees.insert(self.tree.id(), self.tree.clone());
        let mut leaves =
            super::renames::flatten(objects, self.tree.id(), limits.trees, cancel, &memory)?;
        let mut staged = Vec::new();
        let mut conflict_paths = std::collections::BTreeSet::new();
        let mut remaining = limits.max_marker_bytes;
        for conflict in &self.conflicts {
            check(cancel)?;
            let entries = conflict.index_entries()?;
            for entry in &entries {
                check(cancel)?;
                if entry.mode != index::Mode::Gitlink {
                    memory.read_blob(objects, entry.id)?;
                }
            }
            conflict_paths.extend(entries.iter().map(|entry| entry.path.clone()));
            staged.extend(entries);
            if let Some(content) = &conflict.content {
                remaining = remaining
                    .checked_sub(render_bound(content, markers)?)
                    .ok_or(PublicationError::Limit("marker bytes"))?;
                if let Some(leaf) = leaves.get_mut(&conflict.path) {
                    let bytes = content.render(markers);
                    let id = ObjectId::for_blob(objects.object_format(), &bytes);
                    memory.blobs.insert(id, bytes);
                    leaf.id = id;
                }
            }
        }
        for (path, value) in &leaves {
            if !conflict_paths.contains(path) {
                staged.push(index::Entry::new(path.clone(), mode(value.mode)?, value.id));
            }
        }
        let index = index::Index::new(objects.object_format(), staged, limits.index)?;
        let tree = memory.build(objects, leaves)?;
        check(cancel)?;
        let trees = memory
            .order
            .iter()
            .map(|id| memory.trees[id].clone())
            .collect();
        Ok(MergeCheckout {
            tree,
            index,
            blobs: memory.blobs.into_iter().collect(),
            trees,
        })
    }
}

impl Repository {
    /// Installs generated merge objects, writes conflict working files, then publishes stages.
    ///
    /// Uses default checkout bounds and no cancellation. See [`Self::checkout_merge_with`] for
    /// baseline, platform, concurrency and partial-effect contracts.
    ///
    /// Git replays recorded resolutions at this point when rerere is enabled; girt leaves that to
    /// the caller. Follow with [`crate::rerere::Rerere::run_on_index`] when
    /// [`crate::rerere::is_enabled`], and run it again before committing the resolution.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError`]; checkout failures retain their full partial report.
    pub fn checkout_merge(
        &self,
        baseline: Option<ObjectId>,
        plan: &MergeCheckout,
    ) -> Result<crate::checkout::Report, PublicationError> {
        self.checkout_merge_with(
            baseline,
            plan,
            crate::checkout::Limits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Materializes a prepared merge and publishes its staged index under one index guard.
    ///
    /// Requires the same clean baseline and caller-owned exclusion as
    /// [`Self::checkout_tree_with`]. Baseline preparation, untracked/metadata/path protections,
    /// working-file verification, final index publication and partial reports use that engine.
    /// The replacement index is validated before any working-file mutation and stays under the
    /// same guard; there is no temporary stage-zero index representing conflict resolution.
    /// Linux/macOS checkout restrictions apply, including gitlink and unsupported path refusals.
    /// Other platforms return checkout's unsupported failure without installing objects.
    ///
    /// Generated object installation is additive and precedes working-file preparation. A later
    /// failure can leave unreachable objects. Working files are not rolled back; inspect
    /// [`PublicationError::Checkout`] reports before recovery. HEAD and references never move.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError`] for format mismatch, object installation, cancellation or
    /// guarded checkout failure. Only a successful report proves the staged index was published.
    pub fn checkout_merge_with(
        &self,
        baseline: Option<ObjectId>,
        plan: &MergeCheckout,
        limits: crate::checkout::Limits,
        cancel: &AtomicBool,
    ) -> Result<crate::checkout::Report, PublicationError> {
        let _operation = crate::locks::operation();
        check(cancel)?;
        if self.object_format() != plan.index.object_format()
            || baseline.is_some_and(|id| id.format() != self.object_format())
        {
            return Err(PublicationError::ObjectFormat);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            plan.install_objects_controlled(&self.loose_objects(), cancel)?;
            crate::checkout::run_with_index(
                self,
                baseline,
                plan.tree,
                plan.index.entries(),
                limits,
                cancel,
            )
            .map_err(|failure| PublicationError::Checkout(Box::new(failure)))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            self.checkout_tree_with(baseline, Some(plan.tree), limits, cancel)
                .map_err(|failure| PublicationError::Checkout(Box::new(failure)))
        }
    }
}

fn check(cancel: &AtomicBool) -> Result<(), PublicationError> {
    if cancel.load(Ordering::Relaxed) {
        Err(PublicationError::Cancelled)
    } else {
        Ok(())
    }
}

fn mode(mode: EntryMode) -> Result<index::Mode, PublicationError> {
    Ok(match mode {
        EntryMode::Blob => index::Mode::Regular,
        EntryMode::Executable => index::Mode::Executable,
        EntryMode::Symlink => index::Mode::Symlink,
        EntryMode::Gitlink => index::Mode::Gitlink,
        EntryMode::Tree => return Err(TreeError::DirectoryStages.into()),
    })
}

fn render_bound(
    content: &ContentMerge,
    markers: &MarkerOptions,
) -> Result<usize, PublicationError> {
    let limit = || PublicationError::Limit("marker bytes");
    let mut count = 0usize;
    for hunk in content.hunks() {
        let bytes = match hunk {
            ContentHunk::Resolved(bytes) => bytes.len(),
            ContentHunk::Conflict { ours, .. } if content.is_binary() => ours.len(),
            ContentHunk::Conflict { base, ours, theirs } => {
                let label = markers
                    .base
                    .len()
                    .max(markers.ours.len())
                    .max(markers.theirs.len());
                let lines = markers
                    .width
                    .get()
                    .checked_add(label)
                    .and_then(|n| n.checked_add(2))
                    .and_then(|n| n.checked_mul(4))
                    .ok_or_else(limit)?;
                base.len()
                    .checked_add(ours.len())
                    .and_then(|n| n.checked_add(theirs.len()))
                    .and_then(|n| n.checked_add(lines))
                    .and_then(|n| n.checked_add(3))
                    .ok_or_else(limit)?
            }
        };
        count = count.checked_add(bytes).ok_or_else(limit)?;
    }
    Ok(count)
}
