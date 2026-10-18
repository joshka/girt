//! Save index and working-tree snapshots without moving HEAD.
//!
//! A stash is an ordinary commit with two or three parents: the original HEAD, an index commit,
//! and optionally a root commit containing untracked files. [`Snapshot`] reads this shape from
//! Git-created objects. [`Objects::create_stash`] builds the same objects from explicit trees and
//! identities, and returns a separate cleanup plan. It never captures files, applies filters,
//! installs objects or updates `refs/stash` implicitly.
//!
//! [`Objects::apply_stash`] returns working/index merge outcomes with original conflict sides.
//! [`Apply::prepare_checkout`] composes them with guarded merge checkout. [`History`] observes
//! the files-backend reflog stack and plans conditional push/drop publication. Pop is successful
//! apply/checkout followed by an explicit drop; conflict or partial checkout must keep the entry.
//! See the [stash guide](https://github.com/joshka/girt/blob/main/docs/stashing-changes.md) for
//! capture ownership, publication ordering and recovery.
//!
//! Callers own path selection and snapshot capture. Supply the tracked working-tree tree separately
//! from the optional untracked tree; ignored files belong there only when the caller requests them.
//! Save the generated objects and publish the stash entry before removing saved working changes.
//! A staged-only cleanup can conflict even though the saved commits are valid. Preserve the saved
//! entry and let the caller resolve that cleanup.
//!
//! ```no_run
//! use std::sync::atomic::AtomicBool;
//!
//! use girt::stash::{SaveInput, SaveOptions};
//! use girt::{ObjectId, Objects, Signature};
//!
//! fn save(
//!     objects: &Objects,
//!     head: ObjectId,
//!     index: ObjectId,
//!     worktree: ObjectId,
//!     author: Signature,
//!     committer: Signature,
//! ) -> girt::Result<()> {
//!     let input = SaveInput {
//!         head,
//!         index,
//!         worktree,
//!         untracked: None,
//!     };
//!     let plan = objects.create_stash(
//!         input,
//!         author,
//!         committer,
//!         SaveOptions::default(),
//!         &AtomicBool::new(false),
//!     )?;
//!     if let Some(plan) = plan {
//!         assert_eq!(plan.snapshot.index.parents(), &[head]);
//!         // Install the supplied trees and plan.commits(), then publish refs/stash explicitly.
//!         // Only after that succeeds, apply plan.cleanup and restore plan.index_after.
//!     }
//!     Ok(())
//! }
//! ```
use std::sync::atomic::AtomicBool;

mod apply;
pub use apply::{Apply, ApplyInput, ApplyOptions};
mod history;
pub use history::{History, LogEdit, LogError, LogLimits};

use crate::merge::{TreeMerge, TreeMergeOptions};
use crate::{Commit, CommitFields, ObjectId, Objects, Signature, Tree};

/// Saved commits, including the original base, with validated parent relationships.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Snapshot {
    /// Worktree commit; its parents are base, index and optional untracked commit, in that order.
    pub worktree: Commit,
    /// Saved index commit, whose sole parent is the base.
    pub index: Commit,
    /// HEAD at save time.
    pub base: Commit,
    /// Optional parentless commit containing saved untracked/ignored files.
    pub untracked: Option<Commit>,
}

impl Snapshot {
    /// Reads a Git-created stash and verifies its two/three-parent shape.
    ///
    /// Does not traverse the saved trees or infer whether their paths are tracked. Callers choose
    /// object-read limits through their [`Objects`] handle. No repository state changes.
    ///
    /// # Errors
    ///
    /// Returns unreadable commits or [`Error::Shape`] for unsupported parent relationships.
    pub fn read(objects: &Objects, id: ObjectId) -> Result<Self, Error> {
        let worktree = objects.read_commit(id)?;
        let [base, index, tail @ ..] = worktree.parents() else {
            return Err(Error::Shape);
        };
        if tail.len() > 1 {
            return Err(Error::Shape);
        }
        let base = objects.read_commit(*base)?;
        let index = objects.read_commit(*index)?;
        let untracked = tail
            .first()
            .map(|id| objects.read_commit(*id))
            .transpose()?;
        if index.parents() != [base.id()]
            || untracked
                .as_ref()
                .is_some_and(|commit| !commit.parents().is_empty())
        {
            return Err(Error::Shape);
        }
        Ok(Self {
            worktree,
            index,
            base,
            untracked,
        })
    }

    /// Saved identity used in `refs/stash` and its reflog.
    pub fn id(&self) -> ObjectId {
        self.worktree.id()
    }
}

/// Caller-captured trees; construction does not establish their connectivity or path selection.
#[derive(Clone, Copy, Debug)]
pub struct SaveInput {
    /// Existing HEAD commit. Unborn repositories cannot create this stash shape.
    pub head: ObjectId,
    /// Complete stage-zero index snapshot; unmerged indexes must be resolved before capture.
    pub index: ObjectId,
    /// Tracked working files, including staged additions and deletions.
    pub worktree: ObjectId,
    /// Separate untracked tree (`--include-untracked`), including an empty tree if requested.
    pub untracked: Option<ObjectId>,
}

/// Which changes to save and which tracked state to leave behind.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum SaveMode {
    /// Save staged and unstaged changes, restoring base in both index and working files.
    #[default]
    All,
    /// Save both kinds of changes, leaving the saved index in index and working files.
    KeepIndex,
    /// Save staged changes only; subtract them from working files and restore the base index.
    Staged,
}

/// Message context, selection and cleanup merge policy.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SaveOptions {
    /// Short branch label, or `(no branch)` for a detached HEAD. No ref lookup is performed.
    pub branch: Vec<u8>,
    /// Number of base-ID hex digits in default messages; Git's usual minimum is seven.
    /// Callers must choose a repository-unique abbreviation when that matters.
    pub abbreviation: usize,
    /// Custom message body. `None` uses `WIP on branch: abbreviated-id subject`.
    pub message: Option<Vec<u8>>,
    /// Save/cleanup selection, independent of optional untracked capture.
    pub mode: SaveMode,
    /// Existing merge engine's bounds and conflict policy for staged-only cleanup.
    pub trees: TreeMergeOptions,
}

impl Default for SaveOptions {
    fn default() -> Self {
        Self {
            branch: b"(no branch)".to_vec(),
            abbreviation: 7,
            message: None,
            mode: SaveMode::All,
            trees: TreeMergeOptions::default(),
        }
    }
}

/// Saved commits and an independently computed tracked cleanup plan.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Save {
    /// New stash shape; input trees must also be installed before publishing its identity.
    pub snapshot: Snapshot,
    /// Working-tree cleanup, including original conflict sides for staged-only overlap.
    pub cleanup: TreeMerge,
    /// Stage-zero index tree after successful cleanup.
    pub index_after: ObjectId,
    /// Whether caller-selected untracked paths should be removed after durable stash publication.
    /// True means an untracked capture was requested, even if its tree is empty. There may be
    /// no paths to remove. This is not permission to delete paths that changed after capture.
    pub remove_untracked: bool,
}

impl Save {
    /// New commits in dependency order. The existing base is not repeated.
    pub fn commits(&self) -> impl Iterator<Item = &Commit> {
        std::iter::once(&self.snapshot.index)
            .chain(self.snapshot.untracked.iter())
            .chain(std::iter::once(&self.snapshot.worktree))
    }
}

/// Stash computation failed without changing repository state.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A commit could not be read.
    #[error("reading stash commit")]
    ReadCommit(#[from] crate::ReadCommitError),
    /// A supplied tree could not be read.
    #[error("reading stash tree")]
    ReadTree(#[from] crate::ReadTreeError),
    /// New commit metadata is invalid.
    #[error("constructing stash commit")]
    Commit(#[from] crate::CommitError),
    /// Existing merge engine failed.
    #[error("merging stash trees")]
    Merge(#[from] crate::merge::RenameError),
    /// Generated empty tree is invalid.
    #[error("constructing stash tree")]
    Tree(#[from] crate::TreeError),
    /// Saved commits do not have the supported parent shape.
    #[error("invalid stash commit shape")]
    Shape,
    /// Message context contains line separators/NUL, or abbreviation is outside the ID width.
    #[error("invalid stash message context")]
    Message,
    /// Cancellation was observed before computation.
    #[error("stash computation cancelled")]
    Cancelled,
    /// Index restoration must be resolved before working-tree publication.
    #[error("stash index restoration conflicted")]
    IndexConflict,
    /// Existing merge checkout preparation failed.
    #[error("preparing stash index")]
    Preparation(#[from] crate::merge::PublicationError),
    /// The final index cannot represent the proposed paths.
    #[error("constructing stash index")]
    Index(#[from] crate::index::Error),
    /// Occupied path input exceeds preparation limits.
    #[error("stash input limit exceeded")]
    Limit,
    /// Saved untracked data would overwrite an existing file or directory.
    #[error("saved untracked path collides with current files: {0:?}")]
    UntrackedCollision(Vec<u8>),
    /// Git refuses staged-only saving with untracked capture.
    #[error("staged-only stash cannot include untracked files")]
    StagedUntracked,
}

impl Objects {
    /// Constructs saved commits and cleanup, returning `None` when no selected changes exist.
    ///
    /// Both identities are explicit, matching Git's separate author/committer environment.
    /// Default index/untracked messages end in LF; the worktree message has no added final LF.
    /// No trees, commits, refs, index or files are written. Staged cleanup uses the existing merge
    /// engine; it can retain conflicts where Git's patch-based cleanup refuses an overlapping
    /// worktree edit. Its conflict result never authorizes cleanup publication.
    ///
    /// # Errors
    ///
    /// Returns invalid input metadata, unsupported selection or object/merge errors. Tree roots
    /// are read, but this does not prove recursive object connectivity.
    pub fn create_stash(
        &self,
        input: SaveInput,
        author: Signature,
        committer: Signature,
        options: SaveOptions,
        cancel: &AtomicBool,
    ) -> Result<Option<Save>, Error> {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if options.mode == SaveMode::Staged && input.untracked.is_some() {
            return Err(Error::StagedUntracked);
        }
        let base = self.read_commit(input.head)?;
        let base_tree = self.read_tree(base.tree())?;
        let index = self.read_tree(input.index)?;
        let working = self.read_tree(input.worktree)?;
        let untracked = input.untracked.map(|id| self.read_tree(id)).transpose()?;
        let selected = if options.mode == SaveMode::Staged {
            &index
        } else {
            &working
        };
        if index.id() == base_tree.id()
            && selected.id() == base_tree.id()
            && untracked
                .as_ref()
                .is_none_or(|tree| tree.entries().is_empty())
        {
            return Ok(None);
        }
        let context = context(&base, &options)?;
        let make = |tree, parents, message| {
            Commit::new(CommitFields {
                tree,
                parents,
                author: author.clone(),
                committer: committer.clone(),
                extra_headers: vec![],
                message,
            })
        };
        let index = make(
            index.id(),
            vec![base.id()],
            [b"index on ".as_slice(), &context, b"\n"].concat(),
        )?;
        let untracked = untracked
            .map(|tree| {
                make(
                    tree.id(),
                    vec![],
                    [b"untracked files on ".as_slice(), &context, b"\n"].concat(),
                )
            })
            .transpose()?;
        let message = match &options.message {
            Some(message) => [b"On ".as_slice(), &options.branch, b": ", message].concat(),
            None => [b"WIP on ".as_slice(), &context].concat(),
        };
        let parents = std::iter::once(base.id())
            .chain(std::iter::once(index.id()))
            .chain(untracked.iter().map(Commit::id))
            .collect();
        let worktree = make(selected.id(), parents, message)?;
        let (cleanup, index_after) = match options.mode {
            SaveMode::All => (clean(base_tree.clone()), base_tree.id()),
            SaveMode::KeepIndex => (clean(self.read_tree(index.tree())?), index.tree()),
            SaveMode::Staged => {
                let empty = Tree::new(base.id().format(), vec![])?;
                let cleanup = crate::merge::ReplayTrees::default().merge(
                    self,
                    [input.index, input.worktree, base.tree()],
                    empty,
                    options.trees,
                    cancel,
                    None,
                )?;
                (cleanup, base.tree())
            }
        };
        Ok(Some(Save {
            snapshot: Snapshot {
                worktree,
                index,
                base,
                untracked,
            },
            cleanup,
            index_after,
            remove_untracked: input.untracked.is_some(),
        }))
    }
}

fn context(base: &Commit, options: &SaveOptions) -> Result<Vec<u8>, Error> {
    if options
        .branch
        .iter()
        .any(|b| matches!(b, 0 | b'\r' | b'\n'))
        || options.abbreviation == 0
        || options.abbreviation > base.id().to_string().len()
    {
        return Err(Error::Message);
    }
    let abbreviated: Vec<u8> = base
        .id()
        .to_string()
        .bytes()
        .take(options.abbreviation)
        .collect();
    Ok([
        options.branch.as_slice(),
        b": ",
        &abbreviated,
        b" ",
        &base.subject(),
    ]
    .concat())
}

fn clean(tree: Tree) -> TreeMerge {
    TreeMerge {
        tree: tree.clone(),
        conflicts: vec![],
        blobs: vec![],
        trees: vec![tree],
    }
}
