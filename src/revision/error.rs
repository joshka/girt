//! Resolution failures.

use crate::refs::ReferenceError;
use crate::{
    CommitError, HistoryError, ObjectId, ObjectKind, ObjectReadError, PeelError,
    ResolvePrefixError, TreeError,
};

/// A revision that could not be resolved in a repository.
///
/// [`Self::NotFound`], [`Self::NoParent`], [`Self::WrongKind`], [`Self::NoPath`],
/// [`Self::ReflogTooShort`] and [`Self::NoUpstream`] describe the request: the repository is
/// readable but holds no such object. The remaining variants report storage failures or syntax
/// girt does not support yet.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RevisionError {
    /// The text is not a revision or range.
    #[error(transparent)]
    Parse(#[from] super::ParseRevisionError),
    /// Recognised syntax that girt refuses rather than approximates.
    #[error("{0} are not supported yet")]
    Unsupported(&'static str),
    /// No reference or object matches the name.
    #[error("unknown revision `{0}`")]
    NotFound(String),
    /// `~N` or `^N` stepped past a commit without that parent.
    #[error("commit {commit} has no parent {parent}")]
    NoParent {
        /// The commit whose parent was requested.
        commit: ObjectId,
        /// The requested parent, counting from 1.
        parent: usize,
    },
    /// The object cannot be peeled to the requested kind.
    #[error("{id} is a {found}, not a {wanted}", found = .found.as_str(), wanted = .wanted.as_str())]
    WrongKind {
        /// The object reached.
        id: ObjectId,
        /// Its kind.
        found: ObjectKind,
        /// The kind requested.
        wanted: ObjectKind,
    },
    /// The path is absent from the tree, or crosses a non-tree entry.
    #[error("path `{path}` does not exist in {tree}")]
    NoPath {
        /// The tree searched.
        tree: ObjectId,
        /// The requested path.
        path: String,
    },
    /// The reflog has fewer entries than the index requested.
    #[error("log for `{name}` only has {entries} entries")]
    ReflogTooShort {
        /// The reference whose log was read.
        name: String,
        /// Entries present.
        entries: usize,
    },
    /// The reference has no reflog.
    #[error("no reflog for `{0}`")]
    NoReflog(String),
    /// `HEAD` is detached or unborn where a branch is required.
    #[error("HEAD does not point to a branch")]
    DetachedHead,
    /// The branch has no usable upstream or push destination in configuration.
    #[error("no {kind} configured for branch `{branch}`")]
    NoUpstream {
        /// The branch name.
        branch: String,
        /// `upstream` or `push destination`.
        kind: &'static str,
    },
    /// An abbreviated object ID matches several objects, or prefix lookup failed.
    #[error(transparent)]
    Abbreviated(#[from] ResolvePrefixError),
    /// The object is not stored.
    #[error("missing object {0}")]
    MissingObject(ObjectId),
    /// A reference could not be read.
    #[error(transparent)]
    Reference(#[from] ReferenceError),
    /// An object could not be read.
    #[error(transparent)]
    Read(#[from] ObjectReadError),
    /// A tag chain could not be peeled.
    #[error(transparent)]
    Peel(#[from] PeelError),
    /// A commit could not be parsed.
    #[error("parsing commit {id}")]
    Commit {
        /// The commit.
        id: ObjectId,
        /// The parse failure.
        #[source]
        source: CommitError,
    },
    /// A tree could not be parsed.
    #[error("parsing tree {id}")]
    Tree {
        /// The tree.
        id: ObjectId,
        /// The parse failure.
        #[source]
        source: TreeError,
    },
    /// A history walk for a symmetric range failed.
    #[error(transparent)]
    History(#[from] HistoryError),
}
