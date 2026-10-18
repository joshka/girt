//! Replay one commit onto an explicit destination, without changing repository state.
//!
//! Cherry-pick compares the source with its selected parent; revert reverses that comparison.
//! Both use the rename-aware [`crate::merge`] engine. Results retain generated objects and original
//! conflict sides. Callers install objects and publish references separately.
//!
//! ```
//! use std::sync::atomic::AtomicBool;
//!
//! use girt::replay::{EmptyPolicy, Options, Status};
//! use girt::{Commit, InitKind, ObjectFormat, Repository, Signature, Tree};
//!
//! let temporary = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     temporary.path().join("repo"),
//!     InitKind::Bare,
//! )?;
//! let loose = repo.loose_objects();
//! let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
//! let writer = Signature {
//!     name: b"Writer".to_vec(),
//!     email: b"writer@example.com".to_vec(),
//!     seconds: 1_700_000_000,
//!     offset_minutes: 0,
//! };
//! let source = loose.write_commit(&Commit::builder(tree, writer.clone(), b"empty\n").build()?)?;
//! let mut options = Options::default();
//! options.originally_empty = EmptyPolicy::Keep;
//! let replay =
//!     repo.objects()?
//!         .cherry_pick(source, source, writer, options, &AtomicBool::new(false))?;
//! assert!(matches!(replay.status, Status::KeptEmpty(_)));
//! for (_, bytes) in &replay.result.blobs {
//!     loose.write_blob(bytes)?;
//! }
//! for tree in &replay.result.trees {
//!     loose.write_tree(tree)?;
//! }
//! let proposed = replay.commit.expect("kept empty commit");
//! let id = loose.write_commit(&proposed)?;
//! assert_eq!(repo.objects()?.read_commit(id)?.parents(), &[source]);
//! // Installing the object does not move a reference.
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Empty policy distinguishes an originally empty source (`--allow-empty`) from a nonempty source
//! made redundant by the destination (`--keep-redundant-commits`). Set both policies to keep for
//! the latter Git behavior. Stop and skip retain the candidate tree but propose no commit. Merge
//! limitations and per-comparison bounds are inherited from [`Objects::merge_trees_with`].
use std::num::NonZeroUsize;
use std::sync::atomic::AtomicBool;

mod rebase;
pub use rebase::{Rebase, RebaseError, RebaseOptions, RebasedCommit};

use crate::merge::{TreeMerge, TreeMergeOptions};
use crate::{Commit, ObjectId, Objects, Signature};

/// What to do when a clean replay produces no tree change.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum EmptyPolicy {
    /// Return an empty outcome without proposing a commit.
    #[default]
    Stop,
    /// Propose a commit even though its tree equals the destination tree.
    Keep,
    /// Return a skipped outcome without proposing a commit.
    Skip,
}

/// Replay comparison and empty-result policy.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct Options {
    /// One-based parent for a merge source. Required for commits with several parents.
    pub mainline: Option<NonZeroUsize>,
    /// Append Git's cherry-pick provenance line. Ignored for revert.
    pub record_origin: bool,
    /// Policy for a source whose tree equals its selected parent's tree.
    pub originally_empty: EmptyPolicy,
    /// Policy for a nonempty source whose replay changes nothing.
    pub redundant: EmptyPolicy,
    /// Rename-aware merge policy and resource bounds.
    pub trees: TreeMergeOptions,
}

/// Why no new tree changes result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EmptyKind {
    /// The original commit changed no entries relative to the selected parent.
    Original,
    /// Destination already contains the effect being replayed.
    Redundant,
}

/// Replay state, independent of object installation or reference publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Status {
    /// A clean, nonempty commit is proposed.
    Ready,
    /// Original merge sides remain available for caller resolution.
    Conflicted,
    /// Empty policy kept the proposed commit.
    KeptEmpty(EmptyKind),
    /// Empty policy requires a caller decision.
    Empty(EmptyKind),
    /// Empty policy skipped this source.
    Skipped(EmptyKind),
}

/// Candidate objects and commit metadata from replaying a source onto a destination.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Replay {
    /// Original source commit.
    pub source: ObjectId,
    /// Destination commit, which becomes the proposed commit's sole parent.
    pub destination: ObjectId,
    /// Candidate tree and original conflict sides, never implicitly installed.
    pub result: TreeMerge,
    /// Disposition under the selected empty policy.
    pub status: Status,
    /// Proposed unsigned commit, present only for ready or kept-empty results.
    pub commit: Option<Commit>,
}

/// Replay failed without installing objects or changing repository state.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An input commit could not be read.
    #[error("reading replay commit")]
    Read(#[from] crate::ReadCommitError),
    /// Source metadata or proposed commit is invalid.
    #[error("constructing replay commit")]
    Commit(#[from] crate::CommitError),
    /// Rename-aware merge computation failed.
    #[error("merging replay trees")]
    Merge(#[from] crate::merge::RenameError),
    /// A merge source requires a selected parent.
    #[error("merge source requires a mainline parent")]
    MainlineRequired,
    /// The selected one-based parent does not exist.
    #[error("mainline parent is outside the source parent list")]
    InvalidMainline,
}

impl Objects {
    /// Computes a cherry-pick using source author and explicit new committer metadata.
    ///
    /// Source signatures and opaque headers are not copied. Message bytes are retained, with an
    /// optional provenance paragraph. Root sources use an empty base. Merge sources require a
    /// one-based mainline. Conflicts never propose a commit; empty policy is applied only after a
    /// clean merge. No objects, refs, index or working files are written.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for invalid mainline, unreadable inputs, invalid metadata or merge
    /// failure.
    pub fn cherry_pick(
        &self,
        source: ObjectId,
        destination: ObjectId,
        committer: Signature,
        options: Options,
        cancel: &AtomicBool,
    ) -> Result<Replay, Error> {
        self.replay(source, destination, committer, options, cancel, false)
    }

    /// Computes the inverse of a commit, using the supplied identity as author and committer.
    ///
    /// Uses [`Self::cherry_pick`]'s computation and publication contract. The message uses Git's
    /// `Revert "subject"` title and full source identity. For merge sources the explanation also
    /// names the selected parent. The original commit is the merge base; its parent supplies the
    /// inverse side. Root reverts select an empty inverse side.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for invalid mainline, unreadable inputs, invalid metadata or merge
    /// failure.
    pub fn revert(
        &self,
        source: ObjectId,
        destination: ObjectId,
        committer: Signature,
        options: Options,
        cancel: &AtomicBool,
    ) -> Result<Replay, Error> {
        self.replay(source, destination, committer, options, cancel, true)
    }

    fn replay(
        &self,
        source: ObjectId,
        destination: ObjectId,
        committer: Signature,
        options: Options,
        cancel: &AtomicBool,
        reverse: bool,
    ) -> Result<Replay, Error> {
        let original = self.read_commit(source)?;
        let onto = self.read_commit(destination)?;
        let operation = Operation {
            source,
            original: &original,
            onto: &onto,
            committer,
            options,
            reverse,
        };
        operation.compute(self, cancel, &mut crate::merge::ReplayTrees::default())
    }
}

struct Operation<'a> {
    source: ObjectId,
    original: &'a Commit,
    onto: &'a Commit,
    committer: Signature,
    options: Options,
    reverse: bool,
}

impl Operation<'_> {
    fn compute(
        self,
        objects: &Objects,
        cancel: &AtomicBool,
        memory: &mut crate::merge::ReplayTrees,
    ) -> Result<Replay, Error> {
        let Self {
            source,
            original,
            onto,
            committer,
            options,
            reverse,
        } = self;
        let destination = onto.id();
        let parent = match options.mainline {
            Some(number) => Some(
                *original
                    .parents()
                    .get(number.get() - 1)
                    .ok_or(Error::InvalidMainline)?,
            ),
            None if original.parents().len() > 1 => return Err(Error::MainlineRequired),
            None => original.parents().first().copied(),
        };
        let parent_tree = parent
            .map(|id| objects.read_commit(id).map(|commit| commit.tree()))
            .transpose()?;
        let empty = crate::Tree::new(objects.object_format(), vec![])
            .map_err(crate::merge::TreeError::from)
            .map_err(crate::merge::RenameError::from)?;
        let parent_tree = parent_tree.unwrap_or(empty.id());
        let originally_empty = parent_tree == original.tree();
        let (base, theirs) = if reverse {
            (original.tree(), parent_tree)
        } else {
            (parent_tree, original.tree())
        };
        let result = memory.merge(
            objects,
            [base, onto.tree(), theirs],
            empty,
            options.trees,
            cancel,
        )?;
        let empty_kind = if originally_empty {
            EmptyKind::Original
        } else {
            EmptyKind::Redundant
        };
        let policy = if originally_empty {
            options.originally_empty
        } else {
            options.redundant
        };
        let status = if !result.is_clean() {
            Status::Conflicted
        } else if result.tree.id() != onto.tree() {
            Status::Ready
        } else {
            match policy {
                EmptyPolicy::Keep => Status::KeptEmpty(empty_kind),
                EmptyPolicy::Stop => Status::Empty(empty_kind),
                EmptyPolicy::Skip => Status::Skipped(empty_kind),
            }
        };
        let commit = if matches!(status, Status::Ready | Status::KeptEmpty(_)) {
            let author = if reverse {
                committer.clone()
            } else {
                original.to_fields()?.author
            };
            let message = replay_message(original, source, parent, options.record_origin, reverse);
            Some(
                Commit::builder(result.tree.id(), author, message)
                    .parent(destination)
                    .committer(committer)
                    .build()?,
            )
        } else {
            None
        };
        Ok(Replay {
            source,
            destination,
            result,
            status,
            commit,
        })
    }
}

fn replay_message(
    original: &Commit,
    source: ObjectId,
    parent: Option<ObjectId>,
    origin: bool,
    reverse: bool,
) -> Vec<u8> {
    if reverse {
        let mut message = b"Revert \"".to_vec();
        message.extend_from_slice(&original.subject());
        message.extend_from_slice(b"\"\n\n");
        if original.parents().len() > 1 {
            message.extend_from_slice(
                format!(
                    "This reverts commit {source}, reversing\nchanges made to {}.\n",
                    parent.expect("mainline validated")
                )
                .as_bytes(),
            );
        } else {
            message.extend_from_slice(format!("This reverts commit {source}.\n").as_bytes());
        }
        message
    } else {
        let mut message = original.message().to_vec();
        if origin {
            while message.last() == Some(&b'\n') {
                message.pop();
            }
            message.extend_from_slice(
                format!("\n\n(cherry picked from commit {source})\n").as_bytes(),
            );
        }
        message
    }
}
