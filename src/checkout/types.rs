use crate::{ObjectId, index};

/// Work and allocation ceilings for synchronous checkout, not an exact heap budget.
///
/// Trees, index entries, expected paths, operation reports and unique blob payloads are retained
/// eagerly. File verification uses one additional bounded buffer. Cancellation is checked between
/// entries and 64-KiB I/O chunks; one syscall/object decode cannot be interrupted. The caller owns
/// blocking-worker scheduling. Directory names are enumerated again at mutation boundaries, so
/// wide directories can require quadratic work in the number of selected paths.
#[derive(Clone, Copy, Debug)]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
pub struct Applied {
    /// Worktree-relative path bytes.
    pub path: Vec<u8>,
    /// Namespace change that succeeded.
    pub action: Action,
}

/// Outcome of completed namespace operations, excluding changes by independent writers.
#[derive(Debug, Default)]
pub struct Report {
    /// Last phase entered.
    pub stage: Stage,
    /// Operations confirmed successful, in execution order. Unlisted paths were not changed by
    /// checkout, except owned temporary artifacts listed in a failure's cleanup errors.
    pub applied: Vec<Applied>,
    /// True only after successful index publication. On failure the prior index remains, apart
    /// from independent writers; completed working-tree changes are not rolled back.
    pub index_published: bool,
}

/// Checkout failure with exact completed operations and independently reported cleanup failures.
#[derive(Debug, thiserror::Error)]
#[error("checkout failed during {stage:?}: {cause}", stage = .report.stage)]
pub struct Failure {
    /// Cause that stopped progress.
    #[source]
    pub cause: Box<Error>,
    /// Partial worktree outcome. No automatic rollback is attempted.
    pub report: Report,
    /// Cleanup failures identify owned temporary paths/locks that may remain. Never remove a
    /// reported artifact without verifying ownership and excluding concurrent writers first.
    pub cleanup: Vec<Error>,
}

/// Validation, storage, cancellation or filesystem failure.
#[derive(Debug, thiserror::Error)]
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
    #[error("checkout I/O at {path:?}: {source}")]
    Io {
        /// Relative path; cleanup failures contain the owned temporary name.
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
