//! Git sequencer state and explicit recovery plans.
//!
//! Read Git-created state with [`State::read_repository`], compute commits through
//! [`crate::replay`], then
//! prepare a continuation, skip or abort. A plan names the required HEAD/checkout destination and
//! metadata changes separately. The caller owns checkout and reference transactions; metadata
//! publication is explicit and requires exclusive operation ownership. No shell script from Git
//! state, including `author-script`, is evaluated.
//!
//! Am recovery uses [`crate::am::State`] to interpret author/message artifacts and prepare a commit
//! from the staged tree. [`Kind::Am`] admits final-mail continue/skip or session abort.
//! [`State::prepare_next_am`] extracts one queued mail for subsequent explicit staging.
//! Repository-aware observation includes `ORIG_HEAD` from files
//! or the private reftable stack, and metadata publication rechecks that identity. Callers still
//! own checkout, object installation, conditional refs/reflogs and exclusive operation access.
//! Final mail on a still-unborn branch uses [`State::prepare_unborn_am`] and
//! [`UnbornAmRecovery`], whose optional destination explicitly preserves an unborn branch.
//!
//! ```no_run
//! use girt::refs::{Dereference, RefEdit, RefName, Reflog};
//! use girt::sequencer::{Action, Kind, Limits, State};
//! use girt::{Commit, ObjectId, Repository, Signature};
//!
//! // The caller has already resolved/staged the checkout and owns this operation exclusively.
//! fn continue_resolved(
//!     repo: &Repository,
//!     tree: ObjectId,
//!     message: &[u8],
//!     writer: Signature,
//! ) -> Result<(), Box<dyn std::error::Error>> {
//!     let state = State::read(repo.git_dir(), repo.object_format(), Limits::default())?
//!         .ok_or("no operation")?;
//!     let old = repo.head()?.id.ok_or("no HEAD")?;
//!     let source = repo
//!         .objects()?
//!         .read_commit(state.pending().ok_or("no pending source")?)?;
//!     let author = if state.kind() == Kind::Revert {
//!         writer.clone()
//!     } else {
//!         source.to_fields()?.author
//!     };
//!     let commit = Commit::builder(tree, author, message)
//!         .parent(old)
//!         .committer(writer.clone())
//!         .build()?;
//!     let new = repo.loose_objects().write_commit(&commit)?;
//!     let recovery = state.prepare(Action::Continue(new), old)?;
//!     let edit = RefEdit::update(
//!         RefName::HEAD,
//!         old,
//!         new,
//!         Reflog::Append {
//!             committer: writer,
//!             message: recovery.reflog_for(&commit),
//!         },
//!     );
//!     let edit = edit.with_dereference(Dereference::Follow);
//!     repo.references()?.transaction(&[edit])?;
//!     recovery.publish_metadata(repo)?;
//!     // For rebase, Git or the caller's executor can now run the remaining todo.
//!     Ok(())
//! }
//! ```
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use crate::replay::todo::{Todo, TodoLimits};

mod am;
pub use am::UnbornAmRecovery;
mod recovery;
mod state;
use crate::{ObjectFormat, ObjectId};

/// Operation identified by Git's state files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Kind {
    /// One or several cherry-picks.
    CherryPick,
    /// One or several inverse changes.
    Revert,
    /// Merge-backend rebase.
    Rebase,
    /// Patch-mail application in `rebase-apply` (final-mail continue/skip and session abort).
    Am,
}

/// Bounds for reading operation metadata.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Aggregate bytes across state files (default 32 MiB).
    pub max_bytes: usize,
    /// Maximum regular files across state directories (default 128).
    pub max_files: usize,
    /// Todo syntax and count bounds.
    pub todo: TodoLimits,
    /// Reftable admission for repository-aware am original-HEAD reads, per observation.
    pub reftable: crate::refs::reftable::StackLimits,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 32 * 1024 * 1024,
            max_files: 128,
            todo: TodoLimits::default(),
            reftable: crate::refs::reftable::StackLimits::default(),
        }
    }
}

impl Limits {
    /// Sets the reftable budget for each repository-aware original-HEAD observation.
    #[must_use]
    pub fn with_reftable(mut self, limits: crate::refs::reftable::StackLimits) -> Self {
        self.reftable = limits;
        self
    }
}

/// Observed operation metadata with an exact byte snapshot for stale-plan detection.
#[derive(Clone, Debug)]
pub struct State {
    git_dir: PathBuf,
    format: ObjectFormat,
    limits: Limits,
    files: BTreeMap<PathBuf, Vec<u8>>,
    kind: Kind,
    directory: Option<PathBuf>,
    pending: Option<ObjectId>,
    original_head: Option<ObjectId>,
    onto: Option<ObjectId>,
    branch: Option<crate::refs::RefName>,
    todo: Option<Todo>,
}

/// One conditional branch update recorded by Git's rebase update-refs state.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RefUpdate {
    /// Full branch name.
    pub name: crate::refs::RefName,
    /// Original branch tip expected by a caller-owned ref transaction.
    pub old: ObjectId,
    /// Rewritten destination, or `None` while its todo update-ref has not executed.
    pub new: Option<ObjectId>,
}

/// Caller decision after computing or discarding the current unresolved change.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Action {
    /// A caller-created resolved commit is ready for HEAD publication.
    Continue(ObjectId),
    /// Discard the unresolved change, keeping the supplied current HEAD.
    Skip,
    /// Restore the recorded original HEAD and discard operation metadata.
    Abort,
    /// Finish a rebase after its todo is exhausted and no source remains unresolved.
    Finish,
}

/// Explicit checkout/ref intent and subsequent metadata publication.
#[derive(Clone, Debug)]
pub struct Recovery {
    state: State,
    after: BTreeMap<PathBuf, Vec<u8>>,
    /// Destination commit for caller-owned HEAD/checkout publication.
    pub destination: ObjectId,
    /// Original branch to restore on abort or finish; `None` means keep/detach HEAD as
    /// appropriate.
    pub restore_branch: Option<crate::refs::RefName>,
    /// Original branch tip expected before a finish transaction updates it to `destination`.
    pub expected_branch_tip: Option<ObjectId>,
    /// Completed update-refs intents that must be published before finishing metadata.
    pub reference_updates: Vec<RefUpdate>,
    /// Suggested HEAD reflog message. The caller supplies the reflog identity.
    pub reflog_message: Vec<u8>,
}

/// Invalid, unsupported or changed operation metadata.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// File access failed with its original path and cause.
    #[error("accessing sequencer state at {path}")]
    Io {
        /// State path involved in the operation.
        path: PathBuf,
        /// Original I/O failure.
        #[source]
        source: io::Error,
    },
    /// Reference observation failed.
    #[error("reading sequencer HEAD")]
    Reference(#[from] crate::refs::ReferenceError),
    /// A state file is malformed or the layout is not supported.
    #[error("unsupported or invalid sequencer state at {0}")]
    Unsupported(PathBuf),
    /// State observation reached a configured bound.
    #[error("sequencer state exceeds configured bound")]
    Limit,
    /// Todo syntax is invalid.
    #[error("parsing sequencer todo")]
    Todo(#[from] crate::replay::todo::TodoError),
    /// Observed metadata differs from the plan's original snapshot.
    #[error("sequencer state changed since observation")]
    Changed,
    /// Head differs from Git's sequencer abort-safety checkpoint.
    #[error("HEAD differs from sequencer abort-safety checkpoint")]
    HeadChanged,
    /// Metadata publication partially completed. Re-read state before choosing recovery.
    #[error("sequencer metadata publication partially completed")]
    Partial {
        /// State paths already replaced/removed and any retired-directory destination.
        changed: Vec<PathBuf>,
        /// Failure after the listed changes.
        #[source]
        source: Box<Error>,
    },
    /// Extracting the next numbered patch mail failed.
    #[error("extracting queued am mail")]
    Mail(#[from] crate::mailinfo::Error),
}
