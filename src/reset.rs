//! Moving the current branch and resetting index entries, as `git reset` does.
//!
//! [`Repository::reset`] moves the branch HEAD names (or a detached HEAD) to a commit.
//! [`ResetMode::Soft`] moves only the branch; [`ResetMode::Mixed`] also replaces the index with the
//! commit's tree, keeping cached file metadata for unchanged entries so the next status stays
//! fast. Working files are never touched: changes between the old and new commit show up as
//! staged (soft) or unstaged (mixed) changes. Like Git, a reset records the previous commit in
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
//! Hard, keep and merge resets, which rewrite working files, are not available yet. Indexes with
//! skip-worktree entries or sparse directories (sparse checkout) are refused rather than reset
//! partially, and commit resets refuse reftable repositories, where Git keeps `ORIG_HEAD` and
//! `AUTO_MERGE` in the reference table. Callers must exclude other writers of HEAD, the index and
//! operation files for the duration of a call; the index lock and the conditional reference update
//! detect cooperating writers.

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
/// Refusals and read failures leave the repository unchanged. girt locks HEAD's branch before
/// writing anything, so a locked branch fails with [`Self::Reference`] and nothing written; Git
/// instead publishes `ORIG_HEAD` and the index before failing on the branch. `ORIG_HEAD` is
/// written next, before the index and HEAD; if that fails, nothing else changes, where Git warns
/// and continues. A failure after `ORIG_HEAD` (including an [`Self::IndexStorage`] error while
/// publishing the index) leaves `ORIG_HEAD` updated, and [`Self::Reference`] and
/// [`Self::OperationFile`] also report what else was written.
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
    /// Writing `ORIG_HEAD` failed; nothing else was changed.
    #[error("writing ORIG_HEAD: {0}")]
    OrigHead(#[source] std::io::Error),
    /// Moving HEAD's branch failed. `ORIG_HEAD` was written, and with `index_updated` the index
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
        if self.reference_backend() == crate::refs::Backend::Reftable {
            // Reftable keeps ORIG_HEAD and AUTO_MERGE in the table, not as files.
            return Err(ResetError::Unsupported("reftable repositories"));
        }
        if mode == ResetMode::Soft && self.git_dir().join("MERGE_HEAD").exists() {
            return Err(ResetError::MergeInProgress);
        }
        if mode == ResetMode::Mixed && self.worktree().is_none() {
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

        let mut edit = match mode {
            ResetMode::Soft => None,
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

        if let Some(previous) = previous {
            write_orig_head(self.git_dir(), previous).map_err(ResetError::OrigHead)?;
        }
        let index_updated = edit.is_some();
        if let Some(edit) = edit.take() {
            edit.commit()?;
        }
        prepared.publish().map_err(|source| ResetError::Reference {
            index_updated,
            source,
        })?;
        remove_operation_files(self.git_dir())?;
        Ok(previous)
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
