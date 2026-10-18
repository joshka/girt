use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

use super::{Error, Snapshot, clean};
use crate::index::Index;
use crate::merge::{MarkerOptions, PreparationLimits, TreeMerge, TreeMergeOptions};
use crate::{ObjectId, Objects, Tree};

/// Current staged snapshot and existing untracked/ignored paths observed by the caller.
///
/// Application merges onto the index. Guarded checkout must additionally protect dirty tracked
/// files; this input does not authorize overwriting them. Supply all occupied untracked paths,
/// including ignored paths, so saved untracked data cannot overwrite them.
#[derive(Clone, Debug)]
pub struct ApplyInput {
    /// Complete stage-zero current index tree.
    pub index: ObjectId,
    /// Existing non-index leaf paths, in repository-relative byte spelling.
    pub occupied: Vec<Vec<u8>>,
}

/// Saved index restoration and existing merge/preparation bounds.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct ApplyOptions {
    /// Restore the saved index delta as well as working changes (`stash apply --index`).
    pub restore_index: bool,
    /// Rename/content merge policy.
    pub trees: TreeMergeOptions,
    /// Bounds for tree-to-index conversion and conflict rendering.
    pub preparation: PreparationLimits,
}

/// Candidate working/index states; conflicts retain original sides for rerere and other callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Apply {
    /// Tracked working-tree merge, before optional untracked restoration.
    pub tracked: TreeMerge,
    /// Saved index delta merge, when requested. An index conflict stops working-tree computation.
    pub index_merge: Option<TreeMerge>,
    /// Complete working tree with saved untracked files, only when tracked/index merges are clean.
    pub with_untracked: Option<TreeMerge>,
    /// Final index draft. Clean ordinary apply preserves existing staged entries and stages new
    /// paths; conflicted apply retains original merge stages. Absent when index restoration stops.
    pub index: Option<Index>,
}
impl Apply {
    /// Whether index and tracked changes are resolved; only then may pop discard the saved entry.
    pub fn is_clean(&self) -> bool {
        self.tracked.is_clean()
            && self.index_merge.as_ref().is_none_or(TreeMerge::is_clean)
            && self.with_untracked.as_ref().is_none_or(TreeMerge::is_clean)
    }

    /// Prepares markers and the final stash index for guarded checkout under one index lock.
    ///
    /// Unlike ordinary merge checkout, clean apply can leave restored working changes unstaged.
    /// The supplied index draft is published with the working result, without a temporary
    /// all-staged index. No objects, index or working files are written by preparation.
    ///
    /// # Errors
    ///
    /// Returns an index-restoration stop or the existing merge preparation failures.
    pub fn prepare_checkout(
        &self,
        objects: &Objects,
        markers: &MarkerOptions,
    ) -> Result<crate::merge::MergeCheckout, Error> {
        let index = self.index.clone().ok_or(Error::IndexConflict)?;
        let mut working = self.working().clone();
        if let Some(staged) = &self.index_merge {
            working.trees.splice(0..0, staged.trees.iter().cloned());
            working.blobs.splice(0..0, staged.blobs.iter().cloned());
        }
        Ok(working
            .prepare_checkout(objects, markers)?
            .with_index(index)?)
    }

    /// Complete working result, including restored untracked files when available.
    ///
    /// When index restoration conflicted, this is the unchanged current tree. Check
    /// [`Self::is_clean`] and [`Self::index_merge`] before choosing a publication path.
    pub fn working(&self) -> &TreeMerge {
        self.with_untracked.as_ref().unwrap_or(&self.tracked)
    }
}

impl Objects {
    /// Reapplies saved changes onto an explicit current index, without writing repository state.
    ///
    /// Uses the stash base as the common ancestor and the saved worktree as theirs. Optional
    /// index restoration merges the saved index delta first; a conflict there returns immediately
    /// with original sides. Ordinary clean apply keeps current staged changes and stages paths
    /// newly added by the stash, matching Git. A tracked conflict retains merge stages instead.
    ///
    /// Saved untracked paths are checked against current tracked and caller-observed occupied
    /// paths, including ancestor/descendant collisions. Even identical existing files are refused.
    /// Guarded checkout must protect tracked worktree edits and revalidate the occupied
    /// observation. This does not run filters, discover untracked paths, install objects or
    /// drop a stash entry.
    ///
    /// # Errors
    ///
    /// Returns input/merge/preparation failures, bounds or [`Error::UntrackedCollision`].
    /// Existing merge limits and unsupported structural layouts remain explicit refusals.
    pub fn apply_stash(
        &self,
        saved: &Snapshot,
        input: ApplyInput,
        options: ApplyOptions,
        cancel: &AtomicBool,
    ) -> Result<Apply, Error> {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let current = self.read_tree(input.index)?;
        let current_index = draft(self, &clean(current.clone()), options.preparation, cancel)?;
        check_untracked(
            self,
            saved,
            &input,
            &current_index,
            options.preparation,
            cancel,
        )?;
        let empty = Tree::new(input.index.format(), vec![])?;
        let mut memory = crate::merge::ReplayTrees::default();
        let index_merge = if options.restore_index {
            Some(memory.merge(
                self,
                [saved.base.tree(), input.index, saved.index.tree()],
                empty.clone(),
                options.trees,
                cancel,
                None,
            )?)
        } else {
            None
        };
        if index_merge
            .as_ref()
            .is_some_and(|result| !result.is_clean())
        {
            return Ok(Apply {
                tracked: clean(current),
                index_merge,
                with_untracked: None,
                index: None,
            });
        }
        let tracked = memory.merge(
            self,
            [saved.base.tree(), input.index, saved.worktree.tree()],
            empty.clone(),
            options.trees,
            cancel,
            None,
        )?;
        let index = if !tracked.is_clean() {
            draft(self, &tracked, options.preparation, cancel)?
        } else if let Some(result) = &index_merge {
            draft(self, result, options.preparation, cancel)?
        } else {
            let merged = draft(self, &tracked, options.preparation, cancel)?;
            let existing: BTreeSet<&[u8]> = current_index
                .entries()
                .iter()
                .map(|e| e.path.as_slice())
                .collect();
            let additions: BTreeSet<Vec<u8>> = merged
                .entries()
                .iter()
                .filter(|entry| !existing.contains(entry.path.as_slice()))
                .map(|entry| entry.path.clone())
                .collect();
            // Git stages additions even in ordinary apply. Those additions must also displace
            // old parent/child entries when a file becomes a directory or the reverse.
            let entries = current_index
                .entries()
                .iter()
                .filter(|entry| !collision(&additions, &entry.path))
                .cloned()
                .chain(
                    merged
                        .entries()
                        .iter()
                        .filter(|entry| additions.contains(&entry.path))
                        .cloned(),
                )
                .collect();
            Index::new(input.index.format(), entries, options.preparation.index)?
        };
        let mut with_untracked = if tracked.is_clean() {
            let working_index = draft(self, &tracked, options.preparation, cancel)?;
            check_untracked(
                self,
                saved,
                &input,
                &working_index,
                options.preparation,
                cancel,
            )?;
            saved
                .untracked
                .as_ref()
                .map(|commit| {
                    memory.merge(
                        self,
                        [empty.id(), tracked.tree.id(), commit.tree()],
                        empty.clone(),
                        options.trees,
                        cancel,
                        None,
                    )
                })
                .transpose()?
        } else {
            None
        };
        if let Some(result) = &mut with_untracked {
            // The next merge can reference generated content from the preceding merge. Keep
            // the complete working result installable/preparable without temporary writes.
            result.trees.splice(0..0, tracked.trees.iter().cloned());
            result.blobs.splice(0..0, tracked.blobs.iter().cloned());
        }
        Ok(Apply {
            tracked,
            index_merge,
            with_untracked,
            index: Some(index),
        })
    }
}

fn draft(
    objects: &Objects,
    result: &TreeMerge,
    limits: PreparationLimits,
    cancel: &AtomicBool,
) -> Result<Index, Error> {
    Ok(result
        .prepare_checkout_with(objects, &MarkerOptions::default(), limits, cancel)?
        .index()
        .clone())
}

fn check_untracked(
    objects: &Objects,
    saved: &Snapshot,
    input: &ApplyInput,
    current: &Index,
    limits: PreparationLimits,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    if input.occupied.len() > limits.index.max_entries
        || input
            .occupied
            .iter()
            .any(|p| p.len() > limits.index.max_path_bytes)
        || input
            .occupied
            .iter()
            .fold(0usize, |total, path| total.saturating_add(path.len()))
            > limits.index.max_bytes
    {
        return Err(Error::Limit);
    }
    let Some(untracked) = &saved.untracked else {
        return Ok(());
    };
    let saved = draft(
        objects,
        &clean(objects.read_tree(untracked.tree())?),
        limits,
        cancel,
    )?;
    let occupied: BTreeSet<Vec<u8>> = current
        .entries()
        .iter()
        .map(|e| e.path.clone())
        .chain(input.occupied.iter().cloned())
        .collect();
    for entry in saved.entries() {
        if collision(&occupied, &entry.path) {
            return Err(Error::UntrackedCollision(entry.path.clone()));
        }
    }
    Ok(())
}

fn collision(occupied: &BTreeSet<Vec<u8>>, path: &[u8]) -> bool {
    if occupied.contains(path) {
        return true;
    }
    for (position, byte) in path.iter().enumerate() {
        if *byte == b'/' && occupied.contains(&path[..position]) {
            return true;
        }
    }
    occupied.range(path.to_vec()..).next().is_some_and(|next| {
        next.strip_prefix(path)
            .is_some_and(|tail| tail.starts_with(b"/"))
    })
}
