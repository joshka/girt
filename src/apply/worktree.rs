//! Working-file patch preparation; filesystem publication is a separate explicit phase.
use std::sync::atomic::AtomicBool;

use super::{HunkResult, Limits, Patch, TreeOptions};
use crate::{Repository, filter};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;

/// Bounds for affected working files, retained payloads and repeated path verification.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct WorktreeLimits {
    /// Patch file/hunk and per-file stored/working byte bounds.
    pub content: Limits,
    /// Index snapshot bounds; the index is consulted for filter context and never edited.
    pub index: crate::index::Limits,
    /// Aggregate original, converted preimage and stored/working postimage bytes (default 64 MiB).
    pub max_retained_bytes: usize,
    /// Total directory entries inspected per preparation, preflight or publication pass (default
    /// 1,000,000). Publication admits a conservative scan-cost bound before the first mutation.
    pub max_directory_entries: usize,
    /// Affected ancestor directories, including the root (default 10,000).
    pub max_directories: usize,
    /// Mapped old/new paths, planned prefixes and visited ancestor paths (default 16 MiB).
    pub max_path_bytes: usize,
    /// Components in a mapped path (default 128).
    pub max_depth: usize,
}
impl Default for WorktreeLimits {
    fn default() -> Self {
        Self {
            content: Limits::default(),
            index: crate::index::Limits::default(),
            max_retained_bytes: 64 * 1024 * 1024,
            max_directory_entries: 1_000_000,
            max_directories: 10_000,
            max_path_bytes: 16 * 1024 * 1024,
            max_depth: 128,
        }
    }
}

/// One planned working-file result. Paths are repository-relative bytes, never absolute paths.
#[derive(Debug)]
pub struct WorktreeFile {
    /// Destination, or source for a deletion.
    pub path: Vec<u8>,
    /// Original source, absent for additions.
    pub source_path: Option<Vec<u8>>,
    /// Applied text hunks, in patch order.
    pub hunks: Vec<HunkResult>,
    /// A non-required filter driver failed or an output encoding was skipped, as Git permits.
    pub unfiltered: bool,
    /// Warning produced while converting the working preimage to stored form.
    pub crlf_warning: Option<filter::CrlfWarning>,
    pub(super) content: Option<Vec<u8>>,
    pub(super) executable: bool,
}
impl WorktreeFile {
    /// Whether the result uses Git's executable regular-file mode.
    pub fn executable(&self) -> bool {
        self.executable
    }

    /// Proposed bytes after to-worktree filtering, or `None` for a deletion.
    pub fn content(&self) -> Option<&[u8]> {
        self.content.as_deref()
    }
}

/// Fully computed working-file edits and original snapshot evidence. Computing this value writes
/// no objects, index entries or working files. Filter commands run only under the supplied
/// pipeline's command policy. The caller must exclude other writers and ancestor/root renames
/// through preparation and publication; snapshot checks detect observed changes, not arbitrary
/// races after a final check. Hardlink, mount and ABA attacks are outside this contract.
#[derive(Debug)]
pub struct PreparedWorktree {
    pub(super) files: Vec<WorktreeFile>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    snapshot: unix::Snapshot,
}
impl PreparedWorktree {
    /// Rechecks every affected original leaf and ancestor with default cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeError`] if any snapshot changed. No files are written.
    pub fn verify(&mut self) -> Result<(), WorktreeError> {
        self.verify_with(&AtomicBool::new(false))
    }

    /// Rechecks original identities and bytes with explicit cancellation. This is evidence only
    /// for the duration of the check; publication must repeat it at every mutation boundary.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeError`] on changed paths, I/O failures, cancellation or exhausted work.
    pub fn verify_with(&mut self, cancel: &AtomicBool) -> Result<(), WorktreeError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.snapshot.verify(cancel)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = cancel;
            Err(WorktreeError::UnsupportedPlatform)
        }
    }

    /// Planned final bytes and per-hunk results. Inspecting or dropping the plan changes no files.
    pub fn files(&self) -> &[WorktreeFile] {
        &self.files
    }
}

/// One confirmed filesystem mutation. Replacements report removal and creation separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WorktreeAction {
    /// An original regular file was unlinked.
    Removed,
    /// A new regular leaf was exclusively created.
    Created,
    /// A missing ancestor directory was exclusively created.
    DirectoryCreated,
}

/// One mutation in execution order, including a potentially incomplete created file.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct WorktreeWrite {
    /// Repository-relative byte path.
    pub path: Vec<u8>,
    /// Mutation that succeeded at this path.
    pub action: WorktreeAction,
    /// For created files, true only after all bytes and executable mode were installed.
    /// False means installation was not confirmed; the retained file can be empty, partial or
    /// fully written. Inspect its actual state before recovery.
    pub complete: bool,
}

/// Exact confirmed mutations. No automatic rollback or index publication occurs.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct WorktreeReport {
    /// Mutations in execution order. On failure, inspect these paths before recovery; earlier
    /// writes remain, and a created leaf with `complete == false` is retained explicitly.
    pub writes: Vec<WorktreeWrite>,
}

/// Publication stopped. Preparation/hunk errors never reach this phase; late I/O, cancellation or
/// snapshot failures retain the exact mutations performed before stopping.
#[derive(Debug, thiserror::Error)]
#[error("working-file patch publication failed")]
#[non_exhaustive]
pub struct WorktreeFailure {
    /// Cause of stopping.
    #[source]
    pub cause: Box<WorktreeError>,
    /// Mutations already performed; never discard this when choosing recovery.
    pub report: WorktreeReport,
}

impl PreparedWorktree {
    /// Publishes with no cancellation, after preflighting all original snapshots again.
    ///
    /// Wraps [`Self::publish_with`]. All hunk, filter and path preparation is complete before
    /// this phase. Index, refs and object storage remain unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeFailure`] with every confirmed mutation if I/O or a final snapshot check
    /// fails. Earlier writes are retained; no unsafe automatic rollback is attempted.
    ///
    /// ```
    /// use girt::apply::Patch;
    /// use girt::filter::Commands;
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
    ///
    /// let temporary = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     temporary.path().join("r"),
    ///     InitKind::Worktree,
    /// )?;
    /// let path = repo.worktree().unwrap().join("file");
    /// std::fs::write(&path, b"old\n")?;
    /// let mut filters = repo.filters(&Default::default(), Commands::Refuse)?;
    /// let patch = Patch::parse(b"--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n")?;
    /// let plan = patch.prepare_worktree(&repo, &mut filters)?;
    /// assert_eq!(std::fs::read(&path)?, b"old\n");
    ///
    /// let report = plan.publish()?;
    /// assert_eq!(std::fs::read(&path)?, b"new\n");
    /// assert!(report.writes.iter().all(|write| write.complete));
    /// assert!(repo.read_index(Default::default())?.is_none());
    /// # Ok::<(), girt::Error>(())
    /// ```
    pub fn publish(self) -> Result<WorktreeReport, WorktreeFailure> {
        self.publish_with(&AtomicBool::new(false))
    }

    /// Preflights snapshots and the publication scan budget before any mutation, then rechecks
    /// each affected file immediately before mutation.
    ///
    /// Missing directories are created exclusively, original leaves are removed only after
    /// identity/content checks, and new regular files are exclusively created without following
    /// symlinks. A copy's unchanged source is rechecked immediately before destination creation.
    /// Every created file is reported before bytes are written. Failed or cancelled writes retain
    /// the incomplete leaf with `complete == false`; report entries distinguish that from a fully
    /// installed file. Final verification reopens every installed target and compares its bytes.
    ///
    /// There is no batch atomicity or crash durability for publication failures. Parse, hunk,
    /// conversion and preparation failures write nothing; a publication failure reports the exact
    /// confirmed mutations, including directories and partial leaves. Existing empty directories
    /// are retained after deletions. File permissions use Git modes 0644/0755; ACLs, ownership and
    /// other metadata are not preserved. The caller exclusion contract on [`PreparedWorktree`]
    /// applies through this entire call. Verification cannot exclude an arbitrary writer racing
    /// the final inode check and unlink/write syscall.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeFailure`] on snapshot changes, obstructions, I/O, cancellation or bounds.
    /// An initial preflight failure has an empty report. Inspect the report and actual paths
    /// before retrying or attempting recovery; there is no implicit index or reference update.
    pub fn publish_with(self, cancel: &AtomicBool) -> Result<WorktreeReport, WorktreeFailure> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            unix::publish(self, cancel, &mut |_, _| Ok(()))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = cancel;
            Err(WorktreeFailure {
                cause: Box::new(WorktreeError::UnsupportedPlatform),
                report: WorktreeReport::default(),
            })
        }
    }
}

/// Working-file preparation or snapshot verification failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorktreeError {
    /// Patch mapping or content validation failed.
    #[error("preparing working-file patch")]
    Patch(#[from] super::TreeError),
    /// A no-follow filesystem or snapshot check failed.
    #[error("checking working-file snapshot")]
    Status(#[from] crate::status::Error),
    /// Reading a bounded existing file failed.
    #[error("reading working-file preimage")]
    Checkout(#[from] crate::checkout::Error),
    /// The supplied filter pipeline refused or failed conversion.
    #[error("converting patch content")]
    Filter(#[from] filter::Error),
    /// Reading the optional index for staged filter context failed.
    #[error("reading patch filter context")]
    Index(#[from] crate::index::StorageError),
    /// Reading a bounded staged blob for filter context failed.
    #[error("reading staged patch filter context")]
    ReadBlob(#[from] crate::ReadBlobError),
    /// Opening objects for staged filter context failed.
    #[error("opening patch filter objects")]
    Objects(#[from] crate::ObjectReadError),
    /// A filesystem operation failed at a repository-relative path.
    #[error("working-file patch I/O failed")]
    Io {
        /// Repository-relative path; empty for the root.
        path: Vec<u8>,
        /// Underlying operating-system error.
        #[source]
        source: std::io::Error,
    },
    /// Path safety, mode, presence or snapshot requirements were not satisfied.
    #[error("working-file patch refused: {reason}")]
    Refused {
        /// Repository-relative byte path, empty for repository-wide settings.
        path: Vec<u8>,
        /// Specific refusal, containing no file content.
        reason: &'static str,
    },
    /// A work or allocation ceiling was exceeded.
    #[error("working-file patch limit exceeded: {0}")]
    Limit(&'static str),
    /// Cancellation was observed before the next bounded operation.
    #[error("working-file patch cancelled")]
    Cancelled,
    /// Guarded working-file operations currently require Linux or macOS.
    #[error("guarded working-file patches require Linux or macOS")]
    UnsupportedPlatform,
}

impl Patch {
    /// Computes working-file edits with default mapping/bounds and no cancellation.
    ///
    /// Wraps [`Self::prepare_worktree_with`]. The caller supplies the repository's filters, with
    /// an explicit command policy, for both stored preimages and working postimages.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeError`] without publishing any edits when a file, hunk, conversion or
    /// path cannot be admitted. Unsupported platforms return an explicit error.
    pub fn prepare_worktree(
        &self,
        repo: &Repository,
        filters: &mut filter::Pipeline,
    ) -> Result<PreparedWorktree, WorktreeError> {
        self.prepare_worktree_with(
            repo,
            filters,
            TreeOptions::default(),
            WorktreeLimits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Reads all affected original files, validates hunks, and computes filtered postimages.
    ///
    /// The index is read only to supply `text=auto` staged context. Dirty and untracked regular
    /// files can be patch sources; index and refs remain untouched. Rename cycles read original
    /// sources and copies retain theirs. New directories are planned without creating them.
    /// `core.fileMode=false` is refused until a mode policy is supplied; regular-file executable
    /// mode follows the owner execute bit, as checkout/status do. Symlink leaves, symlink
    /// ancestors, special files, directory/file transitions, nested
    /// repositories and aliases sharing a volume name are refused. Path mapping uses the same
    /// stripping and directory-prefix rules as tree application. Existing regular-file modes are
    /// preserved unless declared by the patch; additions use 0644 or declared executable mode.
    ///
    /// Conversion uses the supplied pipeline's fixed attribute view. Patches changing attribute
    /// files do not reload that view midway. Optional conversion failures and CRLF warnings are
    /// reported on [`WorktreeFile`]. Bounds apply to every retained snapshot and converted result;
    /// driver execution has the pipeline's own output bounds. Individual syscalls/conversions
    /// cannot be interrupted. A successful return provides check-only use without publication.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeError`] with no filesystem or index mutation on computation failure.
    /// Caller-authorized filter commands can have their own external effects.
    pub fn prepare_worktree_with(
        &self,
        repo: &Repository,
        filters: &mut filter::Pipeline,
        options: TreeOptions,
        limits: WorktreeLimits,
        cancel: &AtomicBool,
    ) -> Result<PreparedWorktree, WorktreeError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            unix::prepare(self, repo, filters, options, limits, cancel)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (repo, filters, options, limits, cancel);
            Err(WorktreeError::UnsupportedPlatform)
        }
    }
}
