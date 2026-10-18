use crate::{ObjectId, index};

/// Work and allocation ceilings for synchronous checkout, not an exact heap budget.
///
/// Trees, index entries, expected paths, operation reports and unique blob payloads are retained
/// eagerly. File verification uses one additional bounded buffer. Cancellation is checked between
/// entries and 64-KiB I/O chunks; one syscall/object decode cannot be interrupted. The caller owns
/// blocking-worker scheduling. Directory names are enumerated again at mutation boundaries, so
/// wide directories can require quadratic work in the number of selected paths. Blob decode
/// ceilings are reported through `Error::Object` and its storage source; traversal and aggregate
/// accounting can instead return `Error::Limit`. Nested tree budgets return `Error::Tree`; index
/// budgets return `Error::Index` (or `Error::IndexFormat` during replacement validation). Inspect
/// each source chain for the actual bound.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{InitKind, ObjectFormat, Repository};
/// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("w"),
///     InitKind::Worktree,
/// )?;
///
/// use girt::checkout::Limits;
/// use girt::{EntryMode, Tree, TreeEntry};
/// let id = repo.loose_objects().write_blob(b"target")?;
/// let tree = repo.loose_objects().write_tree(&Tree::new(
///     ObjectFormat::Sha1,
///     vec![TreeEntry {
///         name: b"file".to_vec(),
///         mode: EntryMode::Blob,
///         id,
///     }],
/// )?)?;
///
/// use girt::checkout::Error;
/// let failure = repo
///     .checkout_tree_with(
///         None,
///         Some(tree),
///         Limits::default().with_max_file_bytes(4),
///         &AtomicBool::new(false),
///     )
///     .unwrap_err();
/// assert!(matches!(
///     failure.cause.as_ref(),
///     Error::Object(girt::ObjectReadError::Loose(girt::loose::Error::TooLarge))
/// ));
/// assert_eq!(failure.report.stage, girt::checkout::Stage::Preparation);
/// assert!(failure.report.applied.is_empty());
/// assert!(!repo.worktree().unwrap().join("file").exists());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Index parsing and replacement encoding bounds.
    pub index: index::Limits,
    /// Applied separately to baseline and target tree flattening.
    pub trees: crate::TreeCompareLimits,
    /// Pack snapshot bounds when opening object storage.
    pub packs: crate::PackLimits,
    /// Per-object decode bounds, also capped by remaining payload and file budgets.
    pub read: crate::ReadLimits,
    /// Retained unique baseline/target blob payload bytes (default 256 MiB).
    pub max_object_bytes: usize,
    /// Bytes in one blob/working file (default 16 MiB); links have a 1024-byte ceiling.
    pub max_file_bytes: usize,
    /// Total bytes per full baseline/target verification pass (default 256 MiB). Each removal
    /// recheck independently uses the same ceiling.
    pub max_worktree_bytes: usize,
    /// Entries per directory enumeration (default 1,000,000); includes unrelated names.
    /// Enumeration retains names of at most 255 bytes each. Repeated checks get separate budgets.
    pub max_directory_entries: usize,
    /// Sum of distinct baseline/target path prefixes (default 64 MiB). Several bounded maps and
    /// the operation report retain copies; this does not limit their combined heap bytes.
    pub max_path_bytes: usize,
    /// Components in a leaf path (default 64, always capped at 128).
    pub max_depth: usize,
}

impl Limits {
    /// Sets [`Self::index`].
    #[must_use]
    pub fn with_index(mut self, index: index::Limits) -> Self {
        self.index = index;
        self
    }

    /// Sets [`Self::trees`].
    #[must_use]
    pub fn with_trees(mut self, trees: crate::TreeCompareLimits) -> Self {
        self.trees = trees;
        self
    }

    /// Sets [`Self::packs`].
    #[must_use]
    pub fn with_packs(mut self, packs: crate::PackLimits) -> Self {
        self.packs = packs;
        self
    }

    /// Sets [`Self::read`].
    #[must_use]
    pub fn with_read(mut self, read: crate::ReadLimits) -> Self {
        self.read = read;
        self
    }

    /// Sets [`Self::max_object_bytes`].
    #[must_use]
    pub fn with_max_object_bytes(mut self, max_object_bytes: usize) -> Self {
        self.max_object_bytes = max_object_bytes;
        self
    }

    /// Sets [`Self::max_file_bytes`].
    #[must_use]
    pub fn with_max_file_bytes(mut self, max_file_bytes: usize) -> Self {
        self.max_file_bytes = max_file_bytes;
        self
    }

    /// Sets [`Self::max_worktree_bytes`].
    #[must_use]
    pub fn with_max_worktree_bytes(mut self, max_worktree_bytes: usize) -> Self {
        self.max_worktree_bytes = max_worktree_bytes;
        self
    }

    /// Sets [`Self::max_directory_entries`].
    #[must_use]
    pub fn with_max_directory_entries(mut self, max_directory_entries: usize) -> Self {
        self.max_directory_entries = max_directory_entries;
        self
    }

    /// Sets [`Self::max_path_bytes`].
    #[must_use]
    pub fn with_max_path_bytes(mut self, max_path_bytes: usize) -> Self {
        self.max_path_bytes = max_path_bytes;
        self
    }

    /// Sets [`Self::max_depth`].
    #[must_use]
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            index: index::Limits::default(),
            trees: crate::TreeCompareLimits::default(),
            packs: crate::PackLimits::default(),
            read: crate::ReadLimits::default(),
            max_object_bytes: 256 * 1024 * 1024,
            max_file_bytes: 16 * 1024 * 1024,
            max_worktree_bytes: 256 * 1024 * 1024,
            max_directory_entries: 1_000_000,
            max_path_bytes: 64 * 1024 * 1024,
            max_depth: 64,
        }
    }
}

/// Last phase entered by checkout.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Stage {
    /// Locking, validation and planning; no worktree mutation has begun.
    #[default]
    Preparation,
    /// Worktree operations, possibly partially applied.
    Worktree,
    /// Final verification and index publication; worktree operations have completed.
    Publication,
}

/// A namespace operation completed by this call.
///
/// One path can participate in several operations during a file-to-directory replacement. Use the
/// ordered operations for progress or recovery diagnostics, rather than treating them as one final
/// change per path.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::checkout::{Action, Limits};
/// use girt::{EntryMode, InitKind, ObjectFormat, Repository, Tree, TreeEntry};
/// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("w"),
///     InitKind::Worktree,
/// )?;
/// let loose = repo.loose_objects();
/// let id = loose.write_blob(b"data")?;
/// let entry = |name: &[u8], mode, id| TreeEntry {
///     name: name.to_vec(),
///     mode,
///     id,
/// };
/// let old = loose.write_tree(&Tree::new(
///     ObjectFormat::Sha1,
///     vec![entry(b"path", EntryMode::Blob, id)],
/// )?)?;
/// let subtree = loose.write_tree(&Tree::new(
///     ObjectFormat::Sha1,
///     vec![entry(b"child", EntryMode::Blob, id)],
/// )?)?;
/// let new = loose.write_tree(&Tree::new(
///     ObjectFormat::Sha1,
///     vec![entry(b"path", EntryMode::Tree, subtree)],
/// )?)?;
/// let cancel = AtomicBool::new(false);
/// repo.checkout_tree_with(None, Some(old), Limits::default(), &cancel)?;
/// let report = repo.checkout_tree_with(Some(old), Some(new), Limits::default(), &cancel)?;
/// let at_path: Vec<_> = report
///     .applied
///     .iter()
///     .filter(|op| op.path == b"path")
///     .map(|op| op.action)
///     .collect();
/// assert_eq!(at_path, [Action::Remove, Action::CreateDirectory]);
/// assert!(
///     report
///         .applied
///         .iter()
///         .any(|op| op.path == b"path/child" && op.action == Action::Install)
/// );
/// assert!(report.index_published);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Action {
    /// Removed a verified tracked leaf.
    Remove,
    /// Removed a known directory after it became empty.
    RemoveDirectory,
    /// Created a missing directory.
    CreateDirectory,
    /// Installed a regular file or symlink from a verified blob.
    Install,
}

/// One completed operation; multiple records may refer to the same path.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Applied {
    /// Worktree-relative path bytes.
    pub path: Vec<u8>,
    /// Namespace change that succeeded.
    pub action: Action,
}

/// Outcome of completed namespace operations, excluding changes by independent writers.
///
/// A successful report confirms publication and lets a UI describe completed filesystem operations.
/// HEAD is unchanged; a checkout report does not establish that a branch was switched.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{InitKind, ObjectFormat, Repository};
/// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("w"),
///     InitKind::Worktree,
/// )?;
///
/// use girt::checkout::Limits;
/// use girt::{EntryMode, Tree, TreeEntry};
/// let id = repo.loose_objects().write_blob(b"target")?;
/// let tree = repo.loose_objects().write_tree(&Tree::new(
///     ObjectFormat::Sha1,
///     vec![TreeEntry {
///         name: b"file".to_vec(),
///         mode: EntryMode::Blob,
///         id,
///     }],
/// )?)?;
///
/// use girt::checkout::{Action, Stage};
/// let report =
///     repo.checkout_tree_with(None, Some(tree), Limits::default(), &AtomicBool::new(false))?;
/// assert_eq!(report.stage, Stage::Publication);
/// assert!(report.index_published);
/// assert!(
///     report
///         .applied
///         .iter()
///         .any(|entry| entry.path == b"file" && entry.action == Action::Install)
/// );
/// assert_eq!(
///     std::fs::read(repo.worktree().unwrap().join("file"))?,
///     b"target"
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Report {
    /// Last phase entered.
    pub stage: Stage,
    /// Operations confirmed successful, in execution order. Unlisted paths were not changed by
    /// checkout, except incomplete leaves listed in a failure's cleanup errors.
    pub applied: Vec<Applied>,
    /// True only after successful index publication. On failure the prior index remains, apart
    /// from independent writers; completed working-tree changes are not rolled back.
    pub index_published: bool,
}

/// Checkout failure with exact completed operations and independently reported cleanup failures.
///
/// A failed call is not permission to retry the original baseline. Inspect `report.applied`,
/// `report.index_published`, `cause` and every `cleanup` entry; reconcile the actual files under
/// exclusive ownership before making another plan. Even cancellation may follow completed edits.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::checkout::{Error, Limits, Stage};
/// use girt::{InitKind, ObjectFormat, Repository};
/// let directory = tempfile::tempdir()?;
/// let repository = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("repo"),
///     InitKind::Worktree,
/// )?;
/// let failure = repository
///     .checkout_tree_with(None, None, Limits::default(), &AtomicBool::new(true))
///     .unwrap_err();
/// assert!(matches!(failure.cause.as_ref(), Error::Cancelled));
/// assert_eq!(failure.report.stage, Stage::Preparation);
/// assert!(failure.report.applied.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[error("checkout failed during {stage:?}", stage = .report.stage)]
#[non_exhaustive]
pub struct Failure {
    /// Cause that stopped progress.
    #[source]
    pub cause: Box<Error>,
    /// Partial worktree outcome. No automatic rollback is attempted.
    pub report: Report,
    /// Cleanup failures identify incomplete leaves or locks that may remain. Never remove a
    /// reported artifact without verifying ownership and excluding concurrent writers first.
    pub cleanup: Vec<Error>,
}

/// Validation, storage, cancellation or filesystem failure.
///
/// Read this through [`Failure`], which also records completed effects. Refusals identify input
/// or worktree conditions requiring a new policy/baseline. Limits and cancellation can occur after
/// mutation; an I/O error does not imply that all earlier paths are unchanged. Nested
/// [`Error::Verification`] and [`Error::Object`] preserve their diagnostic source chains.
///
/// # Preserve an untracked obstruction
///
/// A refusal during preparation leaves the obstructing file intact. Ask the user to move it or
/// abandon checkout; removing it automatically would turn a diagnostic into data loss.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{InitKind, ObjectFormat, Repository};
/// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("w"),
///     InitKind::Worktree,
/// )?;
///
/// use girt::checkout::Limits;
/// use girt::{EntryMode, Tree, TreeEntry};
/// let id = repo.loose_objects().write_blob(b"target")?;
/// let tree = repo.loose_objects().write_tree(&Tree::new(
///     ObjectFormat::Sha1,
///     vec![TreeEntry {
///         name: b"file".to_vec(),
///         mode: EntryMode::Blob,
///         id,
///     }],
/// )?)?;
///
/// use girt::checkout::{Error, Stage};
/// let path = repo.worktree().unwrap().join("file");
/// std::fs::write(&path, b"local notes")?;
/// let failure = repo
///     .checkout_tree_with(None, Some(tree), Limits::default(), &AtomicBool::new(false))
///     .unwrap_err();
/// assert!(matches!(
///     failure.cause.as_ref(),
///     Error::Refused { path, reason } if path == b"file" && !reason.is_empty()
/// ));
/// assert_eq!(failure.report.stage, Stage::Preparation);
/// assert!(failure.report.applied.is_empty());
/// assert!(!failure.report.index_published);
/// assert_eq!(std::fs::read(path)?, b"local notes");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A supplied baseline or target uses an unsupported object format.
    #[error(transparent)]
    ObjectFormat(#[from] crate::ObjectFormatError),
    /// Input or worktree state outside the conservative checkout contract.
    #[error("checkout refused at {path:?}: {reason}")]
    Refused {
        /// Relative bytes, or empty for repository-wide conditions.
        path: Vec<u8>,
        /// Reason for refusal.
        reason: &'static str,
    },
    /// Cooperative cancellation; inspect the enclosing failure report for prior mutations.
    #[error("checkout cancelled")]
    Cancelled,
    /// A resource bound was exhausted.
    #[error("checkout limit exceeded: {0}")]
    Limit(&'static str),
    /// No-follow path/identity validation failed.
    #[error(transparent)]
    Verification(#[from] Box<crate::status::Error>),
    /// Index locking, parsing or publication failed.
    #[error(transparent)]
    Index(#[from] index::StorageError),
    /// Replacement index validation failed.
    #[error(transparent)]
    IndexFormat(#[from] index::Error),
    /// Tree flattening failed.
    #[error(transparent)]
    Tree(#[from] crate::TreeCompareError),
    /// Object storage failed verification or could not be opened.
    #[error(transparent)]
    Object(#[from] crate::ObjectReadError),
    /// A tree leaf did not identify an available blob.
    #[error("missing or non-blob checkout object {0}")]
    InvalidBlob(ObjectId),
    /// Descriptor-relative filesystem operation failed.
    #[error("checkout I/O at {path:?}")]
    Io {
        /// Relative path; cleanup failures name the incomplete leaf.
        path: Vec<u8>,
        /// Operating-system cause.
        #[source]
        source: std::io::Error,
    },
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
// Keep the nested verification cause off every checkout Result's stack footprint.
impl From<crate::status::Error> for Error {
    fn from(source: crate::status::Error) -> Self {
        Self::Verification(Box::new(source))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn check(cancel: &std::sync::atomic::AtomicBool) -> Result<(), Error> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

pub(super) fn refused(path: &[u8], reason: &'static str) -> Error {
    Error::Refused {
        path: path.to_vec(),
        reason,
    }
}
