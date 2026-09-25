use crate::{ObjectId, TreeChange, TreeCompareLimits, TreeValue, index};

/// Snapshot against which staged entries are compared.
#[derive(Clone, Copy, Debug)]
pub enum Baseline {
    /// Resolve this worktree's HEAD. An unborn branch means an empty tree; detached HEAD works.
    Head,
    /// An explicit tree, or `None` for empty; HEAD is neither read nor validated.
    Tree(Option<ObjectId>),
}

/// Deliberate policy for files absent from the index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Untracked {
    /// Visit only directories needed to find tracked paths; report no untracked names.
    Omit,
    /// List raw leaf paths, including ignored files, without reading their contents.
    /// Empty directories are omitted; nested repositories are reported as boundaries.
    RawFilesWithoutIgnores,
}

/// Bounds for status phases; these are work/allocation ceilings, not exact heap limits.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Applied separately to the initial and final index read.
    pub index: index::Limits,
    /// Applied to flattening the baseline tree, including paths and output leaves.
    pub trees: TreeCompareLimits,
    /// Pack snapshot bounds when opening the reader.
    pub packs: crate::PackLimits,
    /// Per-object reconstruction bounds for HEAD and index blobs.
    pub read: crate::ReadLimits,
    /// Total HEAD and index blob payload bytes, counting repeated identities (default 256 MiB).
    pub max_object_bytes: usize,
    /// Maximum bytes in one working file (default 16 MiB). Symlink targets additionally have a
    /// fixed one-MiB ceiling; reads use one extra byte to detect truncation.
    pub max_file_bytes: usize,
    /// Total bytes hashed from working files/links (default 256 MiB).
    pub max_worktree_bytes: usize,
    /// Directory entries visited, including unrelated names and metadata (default 1,000,000).
    pub max_directory_entries: usize,
    /// Sum of visited paths, index paths and retained ancestor prefixes (default 64 MiB).
    pub max_path_bytes: usize,
    /// Nested directory depth, capped at 128 even if larger (default 64).
    pub max_depth: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            index: index::Limits::default(),
            trees: TreeCompareLimits::default(),
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

/// Explicit interpretation of every successful status result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Comparison {
    /// Literal bytes, owner executable bit and native symlinks; no normalization/config policy.
    RawBytesAndPosixModes,
}

/// A read-only observation, not an atomic snapshot or an authorization to overwrite files.
///
/// Paths are sorted by raw bytes within each list. Empty change lists do not establish Git-default
/// cleanliness: inspect unmerged entries, unchecked gitlinks, boundaries, and untracked policy.
/// Assume-valid entries are verified exactly like other entries. All index blob targets, including
/// conflict stages, are verified; baseline leaf targets and gitlink commit targets are not read.
#[derive(Debug)]
pub struct Report {
    /// Always explicit so raw comparison cannot be confused with normalized Git status.
    pub comparison: Comparison,
    /// Resolved baseline tree, or empty for unborn/explicit empty input.
    pub baseline_tree: Option<ObjectId>,
    /// Whether index storage was absent (treated as empty for staged comparison).
    pub index_missing: bool,
    /// Baseline-to-index leaf changes; conflicted paths appear only in `unmerged`.
    pub staged: Vec<TreeChange>,
    /// Stage-zero index-to-working-tree changes; gitlinks appear only in `unchecked_gitlinks`.
    pub unstaged: Vec<WorktreeChange>,
    /// Exact unresolved entries, including stage, mode, ID and original index metadata.
    pub unmerged: Vec<index::Entry>,
    /// All stage-zero gitlinks. Their presence, identity and dirtiness are uninspected.
    pub unchecked_gitlinks: Vec<index::Entry>,
    /// Policy used; raw untracked paths never claim ignore processing.
    pub untracked_policy: Untracked,
    /// Raw untracked regular files, symlinks and special leaves; empty for `Omit`.
    pub raw_untracked: Vec<Vec<u8>>,
    /// Nested repositories or repository metadata directories excluded from traversal.
    /// The ordinary root `.git` marker is always excluded and omitted from this list.
    pub boundaries: Vec<Vec<u8>>,
}

/// An index leaf whose observed worktree value differs or could not be inspected as a leaf.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeChange {
    /// Original index path bytes.
    pub path: Vec<u8>,
    /// Stage-zero index value.
    pub index: TreeValue,
    /// Observed difference, preserving obstructions rather than treating them as clean.
    pub change: Change,
}

/// Literal worktree difference from one index leaf.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Change {
    /// The exact byte name or one of its ancestor directories was absent.
    Deleted,
    /// Content identity and/or leaf mode differ; compare against the index value.
    Modified(TreeValue),
    /// A directory or special file occupies this leaf, or a non-directory/boundary blocks an
    /// ancestor. No descendant behind the obstruction was opened.
    Obstructed {
        /// First obstructing path in raw relative bytes.
        path: Vec<u8>,
    },
}

/// Status failed without returning a partial report or changing repository data.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Platform, bare repository, filename, or filesystem feature outside the supported scope.
    #[error("unsupported status observation at {path:?}: {reason}")]
    Unsupported {
        /// Relative byte path; empty for a repository-wide failure.
        path: Vec<u8>,
        /// Unsupported condition.
        reason: &'static str,
    },
    /// A configured work or allocation bound was exhausted.
    #[error("status limit exceeded: {0}")]
    Limit(&'static str),
    /// Cooperative cancellation was observed.
    #[error("status cancelled")]
    Cancelled,
    /// A file/directory changed during inspection, or index/HEAD differed on the final reread.
    #[error("concurrent status change at {0:?}")]
    Changed(Vec<u8>),
    /// Contextual worktree I/O failure, including disappearance after enumeration.
    #[error("status I/O at {path:?}: {source}")]
    Io {
        /// Raw relative path (empty for root).
        path: Vec<u8>,
        /// Operating-system cause.
        #[source]
        source: std::io::Error,
    },
    /// Index framing, storage or independent index bounds failed.
    #[error(transparent)]
    Index(#[from] index::StorageError),
    /// HEAD resolution failed.
    #[error(transparent)]
    Reference(#[from] crate::refs::ReferenceError),
    /// Baseline tree traversal failed with tree identity/path context.
    #[error(transparent)]
    Tree(#[from] crate::TreeCompareError),
    /// Opening the pack snapshot failed.
    #[error("cannot open status object reader: {0}")]
    Objects(#[source] crate::ObjectReadError),
    /// Reading a HEAD commit or an index blob failed.
    #[error("reading status object {id} at {path:?}: {source}")]
    Object {
        /// Object identity requested.
        id: ObjectId,
        /// Entry path, or `HEAD` for the baseline commit.
        path: Vec<u8>,
        /// Storage/verification cause. Boxed to keep contextual errors compact.
        #[source]
        source: Box<crate::ObjectReadError>,
    },
    /// Referenced commit/blob is missing or has the wrong kind.
    #[error("invalid status object {id} at {path:?}: {reason}")]
    InvalidObject {
        /// Object identity requested.
        id: ObjectId,
        /// Entry path or `HEAD`.
        path: Vec<u8>,
        /// Missing or wrong-kind explanation.
        reason: &'static str,
    },
    /// HEAD resolved to an invalid commit payload.
    #[error("invalid HEAD commit: {0}")]
    Commit(#[from] crate::CommitError),
}

pub(super) fn check(cancel: &std::sync::atomic::AtomicBool) -> Result<(), Error> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn charge(
    remaining: &mut usize,
    count: usize,
    label: &'static str,
) -> Result<(), Error> {
    *remaining = remaining.checked_sub(count).ok_or(Error::Limit(label))?;
    Ok(())
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn value(entry: &index::Entry) -> TreeValue {
    TreeValue {
        id: entry.id,
        mode: match entry.mode {
            index::Mode::Regular => crate::EntryMode::Blob,
            index::Mode::Executable => crate::EntryMode::Executable,
            index::Mode::Symlink => crate::EntryMode::Symlink,
            index::Mode::Gitlink => crate::EntryMode::Gitlink,
        },
    }
}
