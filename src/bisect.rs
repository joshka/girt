//! Binary search through history for the commit that introduced a change, as `git bisect` does.
//!
//! A bisection starts with a commit that has some property (a *new* commit, `bad` by default)
//! and commits from before it appeared (*old*, `good` by default). Each step checks out the
//! commit that best splits the remaining candidates; the caller tests it and records a verdict
//! with [`Bisect::mark`], or hands a test to [`Bisect::run`].
//!
//! State lives where Git keeps it: `BISECT_START`, `BISECT_TERMS`, `BISECT_NAMES`, `BISECT_LOG`,
//! `BISECT_EXPECTED_REV`, `BISECT_ANCESTORS_OK`, `BISECT_FIRST_PARENT`, the `BISECT_HEAD`
//! pseudo-reference for `--no-checkout`, and `refs/bisect/*`. Git can continue a bisection girt
//! started and the other way round.
//!
//! [`Candidates::select`] reproduces Git's choice of the next commit, including its handling of
//! skipped commits, so both tools test the same commits in the same order. The choice was
//! established by observing `git bisect` and `git rev-list --bisect-all` on generated histories.
//!
//! ```
//! use girt::bisect::{Bisect, Mark, Start, Step};
//! use girt::refs::{RefEdit, RefName, Reflog};
//! use girt::{Commit, InitKind, ObjectFormat, Repository, Signature};
//!
//! # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r"),
//!     InitKind::Worktree,
//! )?;
//! let person = Signature {
//!     name: b"A. Writer".to_vec(),
//!     email: b"writer@example.com".to_vec(),
//!     seconds: 1_700_000_000,
//!     offset_minutes: 0,
//! };
//!
//! // Eight commits in a line; the sixth introduced the change being searched for.
//! let loose = repo.loose_objects();
//! let tree = loose.write_tree(&girt::Tree::new(ObjectFormat::Sha1, vec![])?)?;
//! let mut commits = Vec::new();
//! for n in 0..8 {
//!     let message = format!("commit {n}\n");
//!     let mut builder = Commit::builder(tree, person.clone(), message.as_bytes());
//!     if let Some(&parent) = commits.last() {
//!         builder = builder.parent(parent);
//!     }
//!     commits.push(loose.write_commit(&builder.committer(person.clone()).build()?)?);
//! }
//! let main = RefName::branch("main")?;
//! repo.references()?
//!     .transaction(&[RefEdit::create(main, commits[7], Reflog::Preserve)])?;
//! repo.checkout_tree(None, Some(tree))?;
//!
//! let start = Start::default()
//!     .with_new(commits[7])
//!     .with_olds(vec![commits[0]]);
//! let (mut bisect, mut step) = Bisect::start(&repo, person.clone(), start)?;
//! let found = loop {
//!     step = match step {
//!         Step::Test { commit, .. } => {
//!             let index = commits.iter().position(|&c| c == commit).unwrap();
//!             let mark = if index >= 5 { Mark::New } else { Mark::Old };
//!             bisect.mark(mark, &[commit])?
//!         }
//!         Step::Found(commit) => break commit,
//!         other => panic!("unexpected {other:?}"),
//!     };
//! };
//! assert_eq!(found, commits[5]);
//! bisect.reset(Default::default())?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Limitations
//!
//! Candidate membership is exact reachability; with badly skewed committer dates Git's walk can
//! stop early and keep extra commits that girt leaves out. Shallow boundaries are not handled.
//! Revisions in a replayed log must resolve with girt's revision syntax.

mod select;
mod session;
mod walk;

pub use select::{Candidate, Candidates, CandidatesError, Selection, estimate_steps};
pub use session::{Bisect, Mark, ResetTo, Start, Step, Terms, Verdict};
pub use walk::Scope;

use crate::ObjectId;

/// A bisect operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A bisection is already in progress.
    #[error("a bisection is already in progress")]
    InProgress,
    /// HEAD does not name a commit.
    #[error("HEAD does not name a commit")]
    NoHead,
    /// The repository has no working tree to check out into; use `--no-checkout`.
    #[error("bisecting a repository without a working tree needs --no-checkout")]
    Bare,
    /// The repository uses a feature girt's bisect does not support yet.
    #[error("bisect does not support {0}")]
    Unsupported(&'static str),
    /// A term is not usable.
    #[error("'{0}' is not a valid bisect term")]
    Term(String),
    /// A state file is malformed.
    #[error("malformed {0}")]
    State(&'static str),
    /// A new and an old commit are both needed.
    #[error("bisect needs both a new and an old commit")]
    Waiting,
    /// The new commit is a merge base of an old commit, so the old commits are not its
    /// ancestors.
    #[error("old commits are not ancestors of the new commit {base}")]
    NotAncestors {
        /// The new commit, which is also the merge base.
        base: ObjectId,
    },
    /// No candidate changes the bisected paths.
    #[error("no testable commit found")]
    Untestable,
    /// A replayed log line is not a bisect command (1-based line number; 0 means no start).
    #[error("bisect log line {0} is not a bisect command")]
    Replay(usize),
    /// A run was aborted while testing this commit.
    #[error("bisect run aborted at {0}")]
    Aborted(ObjectId),
    /// The candidates were inconsistent.
    #[error(transparent)]
    Candidates(#[from] CandidatesError),
    /// The recorded pathspec does not parse.
    #[error(transparent)]
    Pathspec(#[from] crate::pathspec::ParseError),
    /// Walking history failed.
    #[error(transparent)]
    History(#[from] crate::HistoryError),
    /// Reading a commit failed.
    #[error(transparent)]
    ReadCommit(#[from] crate::ReadCommitError),
    /// Comparing paths failed.
    #[error(transparent)]
    TreeCompare(#[from] crate::TreeCompareError),
    /// Reading or updating references failed.
    #[error(transparent)]
    Reference(#[from] crate::refs::ReferenceError),
    /// A reference transaction failed.
    #[error(transparent)]
    Transaction(#[from] crate::refs::TransactionError),
    /// Opening the object store failed.
    #[error(transparent)]
    Objects(#[from] crate::ObjectReadError),
    /// Updating the working tree failed.
    #[error(transparent)]
    Checkout(Box<crate::checkout::Failure>),
    /// A state file could not be read or written.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
