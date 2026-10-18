//! Moving the current branch and resetting index entries, as `git reset` does.
//!
//! [`Repository::reset`] moves the branch HEAD names (or a detached HEAD) to a commit.
//! [`ResetMode::Soft`] moves only the branch; [`ResetMode::Mixed`] also replaces the index with the
//! commit's tree, keeping cached file metadata for unchanged entries so the next status stays
//! fast. Neither touches working files: changes between the old and new commit show up as
//! staged (soft) or unstaged (mixed) changes. [`ResetMode::Hard`] also rewrites the tracked
//! working files, discarding local changes. Like Git, a reset records the previous commit in
//! `ORIG_HEAD`, appends `reset: moving to <target>` to the reflogs of HEAD and the branch, and
//! forgets an interrupted cherry-pick, revert or merge.
//!
//! [`Repository::reset_paths`] replaces only the index entries that a pathspec selects, as
//! `git reset [<commit>] -- <paths>` and `git restore --staged` do; it is how a file is unstaged.
//! It moves no reference and writes no `ORIG_HEAD`.
//!
//! ```
//! use girt::pathspec::Pathspec;
//! use girt::reset::{ResetMode, ResetOptions};
//! use girt::{
//!     Commit, CommitFields, EntryMode, InitKind, ObjectFormat, Repository, Signature, Tree,
//!     TreeEntry,
//! };
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r"),
//!     InitKind::Worktree,
//! )?;
//! let loose = repo.loose_objects();
//! let person = Signature {
//!     name: b"A. Writer".to_vec(),
//!     email: b"writer@example.com".to_vec(),
//!     seconds: 1_700_000_000,
//!     offset_minutes: 0,
//! };
//! let tree = |content: &[u8]| -> Result<_, Box<dyn std::error::Error>> {
//!     let blob = loose.write_blob(content)?;
//!     let entry = TreeEntry {
//!         name: b"notes.txt".to_vec(),
//!         mode: EntryMode::Blob,
//!         id: blob,
//!     };
//!     Ok(loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![entry])?)?)
//! };
//! let commit = |tree, parents| {
//!     loose.write_commit(&Commit::new(CommitFields {
//!         tree,
//!         parents,
//!         author: person.clone(),
//!         committer: person.clone(),
//!         extra_headers: vec![],
//!         message: b"snapshot\n".to_vec(),
//!     })?)
//! };
//! let first = commit(tree(b"one\n")?, vec![])?;
//! let second = commit(tree(b"two\n")?, vec![first])?;
//!
//! let options = ResetOptions::new(person.clone(), "second");
//! repo.reset(second, ResetMode::Mixed, &options)?;
//!
//! // Move back one commit, keeping the second commit's change staged.
//! let options = ResetOptions::new(person, "HEAD~1");
//! let previous = repo.reset(first, ResetMode::Soft, &options)?;
//! assert_eq!(previous, Some(second));
//! assert_eq!(repo.head()?.id, Some(first));
//!
//! // Unstage it: the index entry goes back to the first commit's version.
//! let first_tree = repo.objects()?.read_commit(first)?.tree();
//! repo.reset_paths(Some(first_tree), &Pathspec::all(), &Default::default())?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Limits
//!
//! [`ResetMode::Hard`], [`ResetMode::Keep`] and [`ResetMode::Merge`] run on Linux and macOS, with
//! checkout's path and platform rules. Like Git, they remove directories that removing tracked
//! files leaves empty. Where the commit has a file and the working tree has an untracked
//! directory, girt replaces the directory only when it holds nothing but empty directories;
//! otherwise it refuses, naming an untracked file inside, before changing any working file. Git
//! deletes the directory with its untracked contents. Indexes with skip-worktree entries or sparse
//! directories (sparse checkout) are refused rather than reset partially, and commit resets refuse
//! reftable repositories, where Git keeps `ORIG_HEAD` and `AUTO_MERGE` in the reference table.
//! Callers must exclude other writers of HEAD, the index and operation files for the duration of a
//! call; the index lock and the conditional reference update detect cooperating writers.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::index::{self, Entry, Mode, Stage};
use crate::pathspec::Pathspec;
use crate::refs::{Dereference, Expected, RefEdit, RefName, Reflog, Target};
use crate::{EntryMode, ObjectId, Repository, Signature, TreeCompareLimits};

/// How much [`Repository::reset`] changes besides the branch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ResetMode {
    /// Move the branch only, as `git reset --soft`; the index and working files stay.
    Soft,
    /// Move the branch and replace the index with the commit's tree, as `git reset --mixed`.
    Mixed,
    /// Like [`Self::Mixed`], and update the working files that differ between HEAD and the
    /// commit, as `git reset --keep`.
    ///
    /// A file whose staged version already matches the commit keeps its working content. Refused
    /// with [`ResetError::LocalChanges`], changing nothing, when a file to update has a staged
    /// change, a modified working file, or an untracked file in its way; a deleted working file
    /// is restored. Refused with [`ResetError::MergeInProgress`] when the index has conflicts.
    /// Local changes to other files are kept as unstaged changes.
    Keep,
    /// Like [`Self::Mixed`], and update the working files whose index entry differs from the
    /// commit, as `git reset --merge`; this is how a conflicted merge is aborted.
    ///
    /// Staged changes to those files are discarded, conflicted files are reset and deleted ones
    /// restored. Refused with [`ResetError::LocalChanges`], changing nothing, when one of them
    /// also has unstaged changes or an untracked file is in the way. Unstaged changes to other
    /// files are kept.
    Merge,
    /// Also make the tracked working files match the commit, as `git reset --hard`.
    ///
    /// Local changes to tracked files are lost: every path in the index, at any stage, is
    /// rewritten or removed, files deleted locally are restored, and an untracked file where the
    /// commit has a file or needs a directory is replaced. Other untracked files stay; an untracked
    /// directory where the commit has a file is replaced only when it holds nothing but empty
    /// directories, and is refused otherwise, where Git would delete its contents. Files are
    /// written raw; use [`Repository::reset_with_filters`] to convert them as checkout does.
    Hard,
}

/// Who records a reset and how it is described in the reflog.
#[derive(Clone, Debug)]
pub struct ResetOptions {
    committer: Signature,
    target: Vec<u8>,
    index: index::Limits,
    trees: TreeCompareLimits,
}

impl ResetOptions {
    /// Records the reset as `committer`, with the reflog message `reset: moving to <target>`.
    ///
    /// Git writes the revision as the user typed it, such as `HEAD~1` or `origin/main`.
    pub fn new(committer: Signature, target: impl Into<Vec<u8>>) -> Self {
        Self {
            committer,
            target: target.into(),
            index: index::Limits::default(),
            trees: TreeCompareLimits::default(),
        }
    }

    /// Bounds reading and writing the index.
    pub fn with_index_limits(mut self, limits: index::Limits) -> Self {
        self.index = limits;
        self
    }

    /// Bounds reading the target tree.
    pub fn with_tree_limits(mut self, limits: TreeCompareLimits) -> Self {
        self.trees = limits;
        self
    }
}

/// A reset was refused or failed.
///
/// Refusals and read failures leave the repository unchanged, including keep and merge refusals,
/// which the checkout makes before writing any file. girt locks HEAD's branch before writing
/// anything, so a locked branch fails with [`Self::Reference`] and nothing written; Git instead
/// publishes `ORIG_HEAD` and the index before failing on the branch. The index and working files
/// are written next, then `ORIG_HEAD`, then HEAD. An [`Self::IndexStorage`] or [`Self::Checkout`]
/// failure leaves `ORIG_HEAD` and HEAD unchanged; a failure to write `ORIG_HEAD` stops before HEAD
/// moves, where Git warns and continues; [`Self::Reference`] and [`Self::OperationFile`] report
/// what was already written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResetError {
    /// The repository has no working tree, so it has no index to reset.
    #[error("a mixed or path reset needs a working tree")]
    Bare,
    /// `MERGE_HEAD` exists or the index has conflict stages; Git refuses a soft reset in the
    /// middle of a merge.
    #[error("cannot do a soft reset in the middle of a merge")]
    MergeInProgress,
    /// Updating working files for a hard, keep or merge reset failed. `ORIG_HEAD` and HEAD are
    /// unchanged; the failure's report lists the files already changed, and the index is
    /// unchanged unless the report says it was published.
    #[error("updating working files: {0}")]
    Checkout(#[source] Box<crate::checkout::Failure>),
    /// A keep reset on a branch with no commits has nothing to compare with, as Git refuses it.
    #[error("a keep reset needs a commit on HEAD")]
    NoCommit,
    /// A keep or merge reset would overwrite local changes to `path`, or an untracked file there;
    /// nothing was changed.
    #[error(
        "{} has local changes that the reset would overwrite",
        String::from_utf8_lossy(path)
    )]
    LocalChanges {
        /// The first path with conflicting local changes.
        path: Vec<u8>,
    },
    /// The index uses a feature reset does not handle yet.
    #[error("reset does not support {0} yet")]
    Unsupported(&'static str),
    /// The pathspec needs attributes, which path reset does not read.
    #[error("pathspecs with attr: magic are not supported for reset")]
    PathspecAttributes,
    /// The target commit could not be read.
    #[error(transparent)]
    Commit(#[from] crate::ReadCommitError),
    /// The target tree could not be read.
    #[error(transparent)]
    Tree(#[from] crate::TreeCompareError),
    /// The index could not be read, locked or written.
    #[error(transparent)]
    IndexStorage(#[from] index::StorageError),
    /// The new index entries were rejected.
    #[error(transparent)]
    Index(#[from] index::Error),
    /// HEAD could not be read.
    #[error(transparent)]
    Head(#[from] crate::refs::ReferenceError),
    /// Writing `ORIG_HEAD` failed after the index and working files were updated; HEAD has not
    /// moved.
    #[error("writing ORIG_HEAD: {0}")]
    OrigHead(#[source] std::io::Error),
    /// Moving HEAD's branch failed. Its lock is taken before anything is written, so a lock
    /// failure changes nothing; a later publication failure leaves `ORIG_HEAD` written, and with
    /// `index_updated` the index
    /// already holds the target tree.
    #[error("moving HEAD (index updated: {index_updated}): {source}")]
    Reference {
        /// Whether the index was already replaced.
        index_updated: bool,
        /// The reference failure.
        #[source]
        source: crate::refs::TransactionError,
    },
    /// The reset completed, but an operation file could not be removed.
    #[error("removing {}: {source}", path.display())]
    OperationFile {
        /// The file that remains.
        path: PathBuf,
        /// The removal failure.
        #[source]
        source: std::io::Error,
    },
}

/// A keep or merge reset's paths, those that must be clean, and the index it publishes.
struct Selected {
    paths: BTreeSet<Vec<u8>>,
    clean: BTreeSet<Vec<u8>>,
    replacement: Vec<Entry>,
}

impl Selected {
    fn selection(&self) -> crate::checkout::Selection<'_> {
        crate::checkout::Selection {
            paths: &self.paths,
            clean: &self.clean,
            replacement: &self.replacement,
        }
    }
}

/// Files that record an interrupted operation and that any commit reset removes, as Git does.
const OPERATION_FILES: &[&str] = &[
    "MERGE_HEAD",
    "MERGE_MODE",
    "MERGE_MSG",
    "AUTO_MERGE",
    "MERGE_RR",
    "SQUASH_MSG",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
];

impl Repository {
    /// Moves the branch HEAD names, or a detached HEAD, to `commit`, as `git reset` does.
    ///
    /// Returns the commit HEAD named before, which is also written to `ORIG_HEAD`; `None` when
    /// the branch was unborn. Only HEAD and the branch it names move; no other reference
    /// changes. See the [module documentation](crate::reset) for what each mode changes.
    ///
    /// # Errors
    ///
    /// Returns [`ResetError::MergeInProgress`] for a soft reset while `MERGE_HEAD` exists,
    /// [`ResetError::Bare`] for a mixed reset without a working tree and
    /// [`ResetError::Unsupported`] for sparse indexes; these and read failures change nothing.
    /// If HEAD moved concurrently, the conditional update fails with [`ResetError::Reference`].
    /// See [`ResetError`] for what a later failure leaves written.
    pub fn reset(
        &self,
        commit: ObjectId,
        mode: ResetMode,
        options: &ResetOptions,
    ) -> Result<Option<ObjectId>, ResetError> {
        self.reset_with(commit, mode, options, None)
    }

    /// Resets as [`Self::reset`] does, converting working files through `filters` for the modes
    /// that write them ([`ResetMode::Keep`], [`ResetMode::Merge`] and [`ResetMode::Hard`]).
    ///
    /// Load `filters` with [`Self::filters_for_checkout`] for the target commit, so the commit's
    /// own `.gitattributes` decide line endings and filter drivers, as Git reads them. Keep and
    /// merge resets also compare files through `filters` when proving them clean.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::reset`], with filter failures inside
    /// [`ResetError::Checkout`].
    pub fn reset_with_filters(
        &self,
        commit: ObjectId,
        mode: ResetMode,
        options: &ResetOptions,
        filters: &mut crate::filter::Pipeline,
    ) -> Result<Option<ObjectId>, ResetError> {
        self.reset_with(commit, mode, options, Some(filters))
    }

    fn reset_with(
        &self,
        commit: ObjectId,
        mode: ResetMode,
        options: &ResetOptions,
        filters: Option<&mut crate::filter::Pipeline>,
    ) -> Result<Option<ObjectId>, ResetError> {
        if self.reference_backend() == crate::refs::Backend::Reftable {
            // Reftable keeps ORIG_HEAD and AUTO_MERGE in the table, not as files.
            return Err(ResetError::Unsupported("reftable repositories"));
        }
        if mode == ResetMode::Soft && self.git_dir().join("MERGE_HEAD").exists() {
            return Err(ResetError::MergeInProgress);
        }
        if mode != ResetMode::Soft && self.worktree().is_none() {
            return Err(ResetError::Bare);
        }
        let objects = self
            .objects()
            .map_err(|source| ResetError::Commit(crate::ReadCommitError::Read(source)))?;
        let tree = objects.read_commit(commit)?.tree();
        // An unmerged index is a merge in progress too, even without MERGE_HEAD.
        if mode == ResetMode::Soft
            && let Some(index) = self.read_index(options.index)?
            && index
                .entries()
                .iter()
                .any(|entry| entry.stage != Stage::Normal)
        {
            return Err(ResetError::MergeInProgress);
        }

        let selected = match mode {
            ResetMode::Keep | ResetMode::Merge => {
                Some(self.select_local(&objects, Some(tree), mode, options)?)
            }
            _ => None,
        };

        let mut edit = match mode {
            ResetMode::Soft | ResetMode::Hard | ResetMode::Keep | ResetMode::Merge => None,
            ResetMode::Mixed => {
                let mut edit = self.edit_index(options.index)?;
                refuse_sparse(edit.index().entries())?;
                let entries = tree_entries(&objects, Some(tree), &Pathspec::all(), options.trees)?;
                let entries = keep_unchanged(edit.index().entries(), entries);
                edit.replace_entries_reusing_stat(entries)?;
                Some(edit)
            }
        };

        let references = self.references()?;
        let previous = references.resolve(&RefName::HEAD)?.id;
        let message = [b"reset: moving to ".as_slice(), &options.target].concat();
        let reflog = Reflog::Append {
            committer: options.committer.clone(),
            message,
        };
        let update = match references.read(&RefName::HEAD)? {
            // Git logs a reset to the current commit in HEAD's reflog only: rewriting symbolic
            // HEAD to the same branch logs just HEAD and leaves the branch and its log alone.
            Some(Target::Symbolic(branch)) if previous == Some(commit) => RefEdit::new(
                RefName::HEAD,
                Some(Target::Symbolic(branch.clone())),
                Expected::Value(Target::Symbolic(branch)),
                reflog,
            )
            .with_dereference(Dereference::Stored),
            _ => {
                let expected =
                    previous.map_or(Expected::Absent, |id| Expected::Value(Target::Direct(id)));
                RefEdit::new(
                    RefName::HEAD,
                    Some(Target::Direct(commit)),
                    expected,
                    reflog,
                )
                .with_dereference(Dereference::Follow)
            }
        };
        let prepared = references
            .prepare_transaction(&[update])
            .map_err(|source| ResetError::Reference {
                index_updated: false,
                source,
            })?;

        let mut index_updated = edit.is_some();
        if let Some(edit) = edit.take() {
            edit.commit()?;
        }
        // The working files go first, so a refused keep or merge reset changes nothing.
        if mode == ResetMode::Hard || selected.is_some() {
            let selection = selected.as_ref().map(Selected::selection);
            hard(self, tree, filters, selection.as_ref(), options)?;
            index_updated = true;
        }
        if let Some(previous) = previous {
            write_orig_head(self.git_dir(), previous).map_err(ResetError::OrigHead)?;
        }
        prepared.publish().map_err(|source| ResetError::Reference {
            index_updated,
            source,
        })?;
        remove_operation_files(self.git_dir())?;
        Ok(previous)
    }

    /// The paths a keep or merge reset rewrites, the subset that must still be clean, and the
    /// index it publishes, after refusing what Git refuses from the index alone. Whether working
    /// files are clean is checked by the checkout itself, against the files it then rewrites.
    fn select_local(
        &self,
        objects: &crate::Objects,
        tree: Option<ObjectId>,
        mode: ResetMode,
        options: &ResetOptions,
    ) -> Result<Selected, ResetError> {
        let current = self
            .read_index(options.index)?
            .map(|index| index.entries().to_vec())
            .unwrap_or_default();
        refuse_sparse(&current)?;
        let unmerged: BTreeSet<Vec<u8>> = current
            .iter()
            .filter(|entry| entry.stage != Stage::Normal)
            .map(|entry| entry.path.clone())
            .collect();
        // Git refuses a keep reset with any conflict in the index.
        if mode == ResetMode::Keep && !unmerged.is_empty() {
            return Err(ResetError::MergeInProgress);
        }
        let target = tree_entries(objects, tree, &Pathspec::all(), options.trees)?;
        let leaves = |entries: &[Entry]| -> BTreeMap<Vec<u8>, (Mode, ObjectId)> {
            entries
                .iter()
                .filter(|entry| entry.stage == Stage::Normal)
                .map(|entry| (entry.path.clone(), (entry.mode, entry.id)))
                .collect()
        };
        let index_leaves = leaves(&current);
        let target_leaves = leaves(&target);
        // Files whose index entry already matches the target keep their working content.
        let mut paths: BTreeSet<Vec<u8>> = index_leaves
            .keys()
            .chain(target_leaves.keys())
            .filter(|path| index_leaves.get(*path) != target_leaves.get(*path))
            .cloned()
            .collect();
        paths.extend(unmerged.iter().cloned());
        if mode == ResetMode::Keep {
            // Keep rewrites only files whose index entry still matches HEAD.
            let head_tree = match self.head()?.id {
                Some(commit) => Some(objects.read_commit(commit)?.tree()),
                None => None,
            };
            let head_leaves = leaves(&tree_entries(
                objects,
                head_tree,
                &Pathspec::all(),
                options.trees,
            )?);
            // Files HEAD and the target agree on keep their working content; their index entry is
            // simply reset.
            paths.retain(|path| head_leaves.get(path) != target_leaves.get(path));
            if let Some(path) = paths
                .iter()
                .find(|path| index_leaves.get(*path) != head_leaves.get(*path))
            {
                return Err(ResetError::LocalChanges { path: path.clone() });
            }
        }
        // An intent-to-add entry has no staged content to compare a working file with.
        if let Some(entry) = current
            .iter()
            .find(|entry| entry.intent_to_add && paths.contains(&entry.path))
        {
            return Err(ResetError::LocalChanges {
                path: entry.path.clone(),
            });
        }
        let clean = paths.difference(&unmerged).cloned().collect();
        let replacement = keep_unchanged(&current, target);
        Ok(Selected {
            paths,
            clean,
            replacement,
        })
    }

    /// Resets on a branch with no commits, as `git reset` without a revision does there.
    ///
    /// Nothing moves and no `ORIG_HEAD` is written. [`ResetMode::Soft`] changes nothing,
    /// [`ResetMode::Mixed`] empties the index, and [`ResetMode::Hard`] and [`ResetMode::Merge`]
    /// also remove every file the index tracks, keeping untracked files. [`ResetMode::Keep`] is
    /// refused with [`ResetError::NoCommit`].
    ///
    /// # Errors
    ///
    /// Returns [`ResetError::Bare`] without a working tree for mixed and hard resets,
    /// [`ResetError::Unsupported`] for sparse indexes, and [`ResetError::Checkout`] when removing
    /// files fails.
    pub fn reset_unborn(&self, mode: ResetMode, limits: &index::Limits) -> Result<(), ResetError> {
        match mode {
            ResetMode::Soft => Ok(()),
            ResetMode::Mixed => self.reset_paths(None, &Pathspec::all(), limits),
            // As in Git, a merge reset removes the tracked files like a hard one, and a keep reset
            // needs a commit to compare with.
            ResetMode::Hard => {
                if self.worktree().is_none() {
                    return Err(ResetError::Bare);
                }
                let checkout = crate::checkout::Limits::default().with_index(*limits);
                hard_to(self, None, None, None, checkout)
            }
            ResetMode::Merge => {
                if self.worktree().is_none() {
                    return Err(ResetError::Bare);
                }
                let objects = self
                    .objects()
                    .map_err(|source| ResetError::Commit(crate::ReadCommitError::Read(source)))?;
                let options = ResetOptions {
                    committer: Signature {
                        name: Vec::new(),
                        email: Vec::new(),
                        seconds: 0,
                        offset_minutes: 0,
                    },
                    target: Vec::new(),
                    index: *limits,
                    trees: TreeCompareLimits::default(),
                };
                let selected = self.select_local(&objects, None, ResetMode::Merge, &options)?;
                let checkout = crate::checkout::Limits::default().with_index(*limits);
                hard_to(self, None, None, Some(&selected.selection()), checkout)
            }
            ResetMode::Keep => Err(ResetError::NoCommit),
        }
    }

    /// Replaces the index entries `paths` selects with those of `tree`, as
    /// `git reset [<commit>] -- <paths>` and `git restore --staged` do.
    ///
    /// Every selected entry, at any conflict stage, is removed, and each selected file in `tree`
    /// is added at stage zero; `None` is the empty tree, so selected entries are simply removed,
    /// as when unstaging in a repository with no commits. Entries outside `paths` are untouched,
    /// except a file where an added file needs a directory, or the reverse, which is removed as
    /// Git removes it. Unchanged entries keep their cached file metadata and assume-unchanged
    /// flag. Pass the tree of the commit to
    /// reset from, usually HEAD's. No reference, working file or `ORIG_HEAD` changes.
    ///
    /// # Errors
    ///
    /// Returns [`ResetError::Bare`] without a working tree, [`ResetError::PathspecAttributes`] for
    /// `attr:` magic and [`ResetError::Unsupported`] when a selected entry is skip-worktree or a
    /// sparse directory. On any error the index is unchanged.
    pub fn reset_paths(
        &self,
        tree: Option<ObjectId>,
        paths: &Pathspec,
        limits: &index::Limits,
    ) -> Result<(), ResetError> {
        if self.worktree().is_none() {
            return Err(ResetError::Bare);
        }
        let objects = self
            .objects()
            .map_err(|source| ResetError::Commit(crate::ReadCommitError::Read(source)))?;
        let mut edit = self.edit_index(*limits)?;
        let current = edit.index().entries();
        let mut kept = Vec::with_capacity(current.len());
        for entry in current {
            if !paths
                .matches(&entry.path)
                .map_err(|_| ResetError::PathspecAttributes)?
            {
                kept.push(entry.clone());
                continue;
            }
            refuse_sparse(std::slice::from_ref(entry))?;
        }
        let added = tree_entries(&objects, tree, paths, TreeCompareLimits::default())?;
        let added = keep_unchanged(current, added);
        // A file that becomes a directory, or the reverse, replaces the entries in its way even
        // outside `paths`, as Git does.
        let added_paths: std::collections::BTreeSet<&[u8]> =
            added.iter().map(|entry| entry.path.as_slice()).collect();
        kept.retain(|entry| {
            !added_paths
                .iter()
                .any(|path| is_inside(path, &entry.path) || is_inside(&entry.path, path))
        });
        let mut entries = kept;
        entries.extend(added);
        entries.sort_by(|a, b| (&a.path, a.stage).cmp(&(&b.path, b.stage)));
        edit.replace_entries_reusing_stat(entries)?;
        edit.commit()?;
        Ok(())
    }
}

/// Uses the current entry instead of a fresh one where path, content and mode are unchanged,
/// keeping its cached metadata and assume-unchanged flag, as Git does.
fn keep_unchanged(current: &[Entry], replacement: Vec<Entry>) -> Vec<Entry> {
    replacement
        .into_iter()
        .map(|new| {
            current
                .binary_search_by(|old| {
                    (old.path.as_slice(), old.stage).cmp(&(&new.path, Stage::Normal))
                })
                .ok()
                .map(|at| &current[at])
                .filter(|old| old.id == new.id && old.mode == new.mode && !old.intent_to_add)
                .cloned()
                .unwrap_or(new)
        })
        .collect()
}

/// Whether `path` lies inside the directory `directory`.
fn is_inside(path: &[u8], directory: &[u8]) -> bool {
    path.len() > directory.len() && path.starts_with(directory) && path[directory.len()] == b'/'
}

/// Rewrites tracked working files and the index to `tree`, discarding local changes.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hard(
    repository: &Repository,
    tree: ObjectId,
    filters: Option<&mut crate::filter::Pipeline>,
    selection: Option<&crate::checkout::Selection<'_>>,
    options: &ResetOptions,
) -> Result<(), ResetError> {
    let limits = crate::checkout::Limits::default()
        .with_index(options.index)
        .with_trees(options.trees);
    hard_to(repository, Some(tree), filters, selection, limits)
}

/// Rewrites tracked files and the index to `tree`, or removes them all for `None`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hard_to(
    repository: &Repository,
    tree: Option<ObjectId>,
    filters: Option<&mut crate::filter::Pipeline>,
    selection: Option<&crate::checkout::Selection<'_>>,
    limits: crate::checkout::Limits,
) -> Result<(), ResetError> {
    crate::checkout::run_discarding(
        repository,
        tree,
        filters,
        selection,
        limits,
        &AtomicBool::new(false),
    )
    .map(drop)
    .map_err(|failure| match &*failure.cause {
        crate::checkout::Error::Refused { path, reason }
            if selection.is_some()
                && [
                    crate::checkout::LOCAL_CHANGES,
                    "untracked obstruction",
                    "untracked ancestor obstruction",
                ]
                .contains(reason) =>
        {
            ResetError::LocalChanges { path: path.clone() }
        }
        _ => ResetError::Checkout(Box::new(failure)),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn hard(
    _: &Repository,
    _: ObjectId,
    _: Option<&mut crate::filter::Pipeline>,
    _: Option<&crate::checkout::Selection<'_>>,
    _: &ResetOptions,
) -> Result<(), ResetError> {
    Err(ResetError::Unsupported("hard reset on this platform"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn hard_to(
    _: &Repository,
    _: Option<ObjectId>,
    _: Option<&mut crate::filter::Pipeline>,
    _: Option<&crate::checkout::Selection<'_>>,
    _: crate::checkout::Limits,
) -> Result<(), ResetError> {
    Err(ResetError::Unsupported("hard reset on this platform"))
}

fn refuse_sparse(entries: &[Entry]) -> Result<(), ResetError> {
    for entry in entries {
        if entry.mode == Mode::SparseDirectory {
            return Err(ResetError::Unsupported("sparse index directories"));
        }
        if entry.skip_worktree {
            return Err(ResetError::Unsupported("skip-worktree entries"));
        }
    }
    Ok(())
}

/// Stage-zero index entries for the files of `tree` that `paths` selects.
fn tree_entries(
    objects: &crate::Objects,
    tree: Option<ObjectId>,
    paths: &Pathspec,
    limits: TreeCompareLimits,
) -> Result<Vec<Entry>, ResetError> {
    let leaves =
        objects.compare_trees_matching(None, tree, paths, limits, &AtomicBool::new(false))?;
    Ok(leaves
        .into_iter()
        .filter_map(|change| {
            let leaf = change.new?;
            let mode = match leaf.mode {
                EntryMode::Blob => Mode::Regular,
                EntryMode::Executable => Mode::Executable,
                EntryMode::Symlink => Mode::Symlink,
                EntryMode::Gitlink => Mode::Gitlink,
                _ => return None,
            };
            Some(Entry::new(change.path, mode, leaf.id).with_stage(Stage::Normal))
        })
        .collect())
}

/// Writes `ORIG_HEAD` through a lock file, so readers see the old or the new value.
fn write_orig_head(git_dir: &Path, id: ObjectId) -> std::io::Result<()> {
    let path = git_dir.join("ORIG_HEAD");
    let lock = git_dir.join("ORIG_HEAD.lock");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)?;
    let written = writeln!(file, "{id}").and_then(|()| file.sync_all());
    drop(file);
    match written.and_then(|()| std::fs::rename(&lock, &path)) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&lock);
            Err(error)
        }
    }
}

fn remove_operation_files(git_dir: &Path) -> Result<(), ResetError> {
    for name in OPERATION_FILES {
        let path = git_dir.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(ResetError::OperationFile { path, source }),
        }
    }
    Ok(())
}
