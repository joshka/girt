use crate::{ChangeStatus, ObjectId, TreeChange, TreeCompareLimits, TreeValue, index};

/// Snapshot against which staged entries are compared.
///
/// Use an explicit tree when the comparison belongs to a revision selected by your application,
/// rather than this worktree's HEAD. An explicit empty tree does not inspect HEAD at all.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{InitKind, ObjectFormat, Repository};
/// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Worktree,
/// )?;
///
/// use girt::status::{Baseline, Limits, Untracked};
/// let report = repo.raw_status_with(
///     Baseline::Tree(None),
///     Untracked::Omit,
///     Limits::default(),
///     &AtomicBool::new(false),
/// )?;
/// assert!(report.baseline_tree.is_none());
/// assert!(report.index_missing);
/// assert!(report.staged.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Baseline {
    /// Resolve this worktree's HEAD. An unborn branch means an empty tree; detached HEAD works.
    Head,
    /// An explicit tree, or `None` for empty; HEAD is neither read nor validated.
    Tree(Option<ObjectId>),
}

/// Deliberate policy for files absent from the index.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{InitKind, ObjectFormat, Repository};
/// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Worktree,
/// )?;
///
/// use girt::status::{Baseline, Limits, Untracked};
/// std::fs::write(repo.worktree().unwrap().join("notes.tmp"), b"scratch")?;
/// std::fs::write(repo.worktree().unwrap().join(".gitignore"), b"*.tmp\n")?;
/// let report = repo.raw_status_with(
///     Baseline::Head,
///     Untracked::RawFilesWithoutIgnores,
///     Limits::default(),
///     &AtomicBool::new(false),
/// )?;
/// assert!(report.raw_untracked.contains(&b"notes.tmp".to_vec()));
/// // Ignore matching is an explicit separate step, even though .gitignore exists.
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Untracked {
    /// Visit only directories needed to find tracked paths; report no untracked names.
    Omit,
    /// List raw leaf paths, including ignored files, without reading their contents.
    /// Empty directories are omitted; nested repositories are reported as boundaries.
    RawFilesWithoutIgnores,
}

/// Which untracked paths that ignore rules do not exclude [`Repository::status`] lists.
///
/// Git never ignores a tracked path, and an ignored directory hides every untracked path below it
/// even if a rule re-includes one. Only regular files, symlinks and nested repositories are
/// listed; empty directories, sockets and FIFOs are not, as in `git status`.
///
/// ```
/// use girt::status::{Baseline, UntrackedFiles, UntrackedKind};
/// use girt::workflow::Environment;
/// use girt::{InitKind, ObjectFormat, Repository};
/// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("w"),
///     InitKind::Worktree,
/// )?;
/// let worktree = repo.worktree().unwrap();
/// std::fs::create_dir_all(worktree.join("notes/drafts"))?;
/// std::fs::write(worktree.join("notes/drafts/a.md"), b"a")?;
/// std::fs::write(worktree.join("notes/b.md"), b"b")?;
///
/// let rules = repo.ignore_rules(&Environment::default())?;
/// let all = repo.status(Baseline::Head, UntrackedFiles::All, &rules)?;
/// let all: Vec<_> = all.untracked.iter().map(|entry| &entry.path[..]).collect();
/// assert_eq!(all, [&b"notes/b.md"[..], b"notes/drafts/a.md"]);
///
/// let collapsed = repo.status(Baseline::Head, UntrackedFiles::Collapsed, &rules)?;
/// assert_eq!(collapsed.untracked[0].path, b"notes");
/// assert_eq!(collapsed.untracked[0].kind, UntrackedKind::Directory);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// [`Repository::status`]: crate::Repository::status
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UntrackedFiles {
    /// List nothing and skip ignore matching; only tracked paths are inspected.
    Omit,
    /// List every visible untracked file, like `git status --untracked-files=all`.
    All,
    /// List a directory holding no tracked paths once, as [`UntrackedKind::Directory`], when it
    /// contains any visible untracked file. This is `git status`'s default listing. Traversal of
    /// such a directory stops at the first visible file. As in Git, a directory occupying the path
    /// of a tracked file is not listed at all; [`Self::All`] lists its contents.
    Collapsed,
}

/// What an untracked path in [`Report::untracked`] names.
///
/// Git displays directory kinds with a trailing `/`; the path itself never has one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UntrackedKind {
    /// A regular file or symlink.
    File,
    /// A directory without tracked paths, listed in place of its contents by
    /// [`UntrackedFiles::Collapsed`].
    Directory,
    /// A nested repository, which status never enters.
    Repository,
}

/// An untracked path that the ignore rules do not exclude.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct UntrackedPath {
    /// Repository-relative path bytes, without a trailing slash.
    pub path: Vec<u8>,
    /// What the path names.
    pub kind: UntrackedKind,
}

/// Bounds for status phases; these are work/allocation ceilings, not exact heap limits.
///
/// Choose file and aggregate budgets separately: many small files can exhaust the total even when
/// each file fits. `Error::Limit("file bytes")` identifies the per-file ceiling;
/// `Error::Limit("worktree bytes")` identifies aggregate exhaustion. Each read is capped by the
/// remaining aggregate allowance. A limit failure supplies no partial report and must not become
/// an empty display.
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
/// use girt::index::{Entry, Mode};
/// use girt::status::{Baseline, Error, Limits, Untracked};
/// let id = repo.loose_objects().write_blob(b"12345")?;
/// let mut edit = repo.edit_index(Default::default())?;
/// edit.replace_entries(vec![
///     Entry::new(b"a".to_vec(), Mode::Regular, id),
///     Entry::new(b"b".to_vec(), Mode::Regular, id),
/// ])?;
/// edit.commit()?;
/// std::fs::write(repo.worktree().unwrap().join("a"), b"12345")?;
/// std::fs::write(repo.worktree().unwrap().join("b"), b"12345")?;
/// let result = repo.raw_status_with(
///     Baseline::Head,
///     Untracked::Omit,
///     Limits::default()
///         .with_max_file_bytes(5)
///         .with_max_worktree_bytes(8),
///     &AtomicBool::new(false),
/// );
/// assert!(matches!(result, Err(Error::Limit("worktree bytes"))));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
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

impl Limits {
    /// Sets [`Self::index`].
    #[must_use]
    pub fn with_index(mut self, index: index::Limits) -> Self {
        self.index = index;
        self
    }

    /// Sets [`Self::trees`].
    #[must_use]
    pub fn with_trees(mut self, trees: TreeCompareLimits) -> Self {
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
#[non_exhaustive]
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
///
/// # Inspect unresolved entries before declaring a worktree clean
///
/// Conflict stages are excluded from both change lists. Preserve their identities when presenting a
/// conflict; resolving it requires an explicit index edit, not a status refresh.
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
/// use girt::index::{Entry, Mode, Stage};
/// use girt::status::{Baseline, Limits, Untracked};
/// let id = repo.loose_objects().write_blob(b"ours")?;
/// let mut edit = repo.edit_index(Default::default())?;
/// edit.replace_entries(vec![
///     Entry::new(b"conflict".to_vec(), Mode::Regular, id).with_stage(Stage::Ours),
/// ])?;
/// edit.commit()?;
///
/// let report = repo.raw_status_with(
///     Baseline::Head,
///     Untracked::Omit,
///     Limits::default(),
///     &AtomicBool::new(false),
/// )?;
///
/// assert!(report.staged.is_empty());
/// assert!(report.unstaged.is_empty());
/// assert_eq!(report.unmerged[0].stage, Stage::Ours);
/// assert_eq!(report.unmerged[0].id, id);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
#[non_exhaustive]
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
    /// Raw listing policy; [`Untracked::Omit`] for [`Repository::status`], which lists into
    /// [`Self::untracked`] instead.
    ///
    /// [`Repository::status`]: crate::Repository::status
    pub untracked_policy: Untracked,
    /// Raw untracked regular files, symlinks and special leaves; empty for `Omit` and for
    /// ignore-aware status.
    pub raw_untracked: Vec<Vec<u8>>,
    /// Ignore-aware listing, or `None` for raw status.
    pub untracked_files: Option<UntrackedFiles>,
    /// Untracked paths the ignore rules do not exclude, sorted by path bytes. Empty for raw status
    /// and for [`UntrackedFiles::Omit`].
    pub untracked: Vec<UntrackedPath>,
    /// Nested repositories or repository metadata directories excluded from traversal.
    /// The ordinary root `.git` marker is always excluded and omitted from this list. Ignore-aware
    /// status does not visit ignored directories, so their nested repositories are not listed.
    pub boundaries: Vec<Vec<u8>>,
}

/// An index leaf whose observed worktree value differs or could not be inspected as a leaf.
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
/// use girt::index::{Entry, Mode};
/// use girt::status::{Baseline, Change, Limits, Untracked};
/// let id = repo.loose_objects().write_blob(b"tracked")?;
/// let mut edit = repo.edit_index(Default::default())?;
/// edit.replace_entries(vec![Entry::new(b"missing".to_vec(), Mode::Regular, id)])?;
/// edit.commit()?;
///
/// let report = repo.raw_status_with(
///     Baseline::Head,
///     Untracked::Omit,
///     Limits::default(),
///     &AtomicBool::new(false),
/// )?;
///
/// let change = &report.unstaged[0];
/// assert_eq!(change.path, b"missing");
/// assert_eq!(change.index.id, id);
/// assert!(matches!(change.change, Change::Deleted));
/// // The index still retains the staged blob; status does not stage this deletion.
/// assert_eq!(
///     repo.read_index(Default::default())?.unwrap().entries()[0].id,
///     id
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct WorktreeChange {
    /// Original index path bytes.
    pub path: Vec<u8>,
    /// Stage-zero index value.
    pub index: TreeValue,
    /// Observed difference, preserving obstructions rather than treating them as clean.
    pub change: Change,
}

impl WorktreeChange {
    /// Classifies this change as the second column of `git status --porcelain` does, when the
    /// report alone determines it.
    ///
    /// A deleted path is [`ChangeStatus::Deleted`]; a modified one is compared with
    /// [`index`](Self::index) by [`ChangeStatus::between`]. Returns `None` for
    /// [`Change::Obstructed`]: Git reports `D` when a directory occupies the leaf or an ancestor is
    /// blocked, but `M` when a special file such as a FIFO occupies the leaf, and the report does
    /// not record which. Inspect the obstructing path to decide. Staged changes in
    /// [`Report::staged`] use [`TreeChange::status`].
    #[must_use]
    pub fn status(&self) -> Option<ChangeStatus> {
        match &self.change {
            Change::Deleted => Some(ChangeStatus::Deleted),
            Change::Modified(new) => Some(ChangeStatus::between(Some(self.index), Some(*new))),
            Change::Obstructed { .. } => None,
        }
    }
}

/// Literal worktree difference from one index leaf.
///
/// An obstruction differs from deletion: the tracked path is blocked by a different filesystem
/// kind or a repository boundary, at the leaf or an ancestor. Keep the first blocking path in
/// diagnostics; it can differ from the tracked entry path.
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
/// use girt::index::{Entry, Mode};
/// use girt::status::{Baseline, Change, Limits, Untracked};
/// let id = repo.loose_objects().write_blob(b"tracked")?;
/// let mut edit = repo.edit_index(Default::default())?;
/// edit.replace_entries(vec![Entry::new(b"dir/file".to_vec(), Mode::Regular, id)])?;
/// edit.commit()?;
/// std::fs::write(repo.worktree().unwrap().join("dir"), b"untracked obstruction")?;
///
/// let report = repo.raw_status_with(
///     Baseline::Head,
///     Untracked::Omit,
///     Limits::default(),
///     &AtomicBool::new(false),
/// )?;
///
/// assert_eq!(report.unstaged[0].path, b"dir/file");
/// assert!(matches!(
///     &report.unstaged[0].change,
///     Change::Obstructed { path } if path == b"dir"
/// ));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
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
///
/// [`Error::Changed`] means observations raced: retry for display, but obtain exclusive ownership
/// before using observations to change files. [`Error::Unsupported`] needs an application policy
/// or another implementation; it does not mean clean. Preserve the source chain of object/index
/// errors so the failing storage can be diagnosed.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::status::{Baseline, Error, Limits, Untracked};
/// use girt::{InitKind, ObjectFormat, Repository};
/// let directory = tempfile::tempdir()?;
/// let repository = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("repo"),
///     InitKind::Worktree,
/// )?;
/// let result = repository.raw_status_with(
///     Baseline::Head,
///     Untracked::Omit,
///     Limits::default(),
///     &AtomicBool::new(true),
/// );
/// assert!(matches!(result, Err(Error::Cancelled)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Platform, bare repository, filename, or filesystem feature outside the supported scope.
    #[error("unsupported status observation at {path:?}: {reason}")]
    Unsupported {
        /// Relative byte path; empty for a repository-wide failure.
        path: Vec<u8>,
        /// Unsupported condition.
        reason: &'static str,
    },
    /// An ignore rule could not be loaded or applied at this path.
    #[error("status ignore rules at {path:?}")]
    Ignore {
        /// The `.gitignore` or queried path.
        path: Vec<u8>,
        /// Limit or path cause.
        #[source]
        source: crate::ignore::Error,
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
    #[error("status I/O at {path:?}")]
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
    #[error("cannot open status object reader")]
    Objects(#[source] crate::ObjectReadError),
    /// Reading a HEAD commit or an index blob failed.
    #[error("reading status object {id} at {path:?}")]
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
    #[error("invalid HEAD commit")]
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
            index::Mode::SparseDirectory => crate::EntryMode::Tree,
            index::Mode::Gitlink => crate::EntryMode::Gitlink,
        },
    }
}
