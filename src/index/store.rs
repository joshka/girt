use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::{Entry, Error, Index, Limits};
use crate::{ObjectFormat, Repository};

/// Prepublication policy for an index lock file.
///
/// Defaults preserve ordinary umask behavior and do not synchronize. Synchronization uses macOS
/// full file flush or `File::sync_all` elsewhere; directory entries are not synchronized.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndexCommitOptions {
    /// Permissions applied to the owned lock before publication.
    pub shared_permissions: crate::SharedPermissions,
    /// Synchronize the complete lock file before its final snapshot checks and rename.
    pub sync: bool,
}

/// Explicit storage admission for a caller-selected index edit.
///
/// Defaults retain standalone, regular-file-only admission. These options do not select an index
/// from environment or configuration, authorize directory replacement, or bypass snapshot checks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EditOptions {
    /// Resolve immutable split dependencies beside the selected lexical index path.
    pub resolve_split: bool,
    /// Read through a final symlink, then replace that selected leaf on publication.
    ///
    /// The referent is never written. The original leaf kind, symlink spelling, followed bytes
    /// and presence remain publication preconditions; Unix also checks the captured leaf inode.
    pub follow_symlink: bool,
}

#[derive(Debug)]
struct LeafSnapshot {
    metadata: Option<fs::Metadata>,
    target: Option<PathBuf>,
}

impl LeafSnapshot {
    fn read(path: &Path) -> Result<Self, StorageError> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_error("inspect index leaf", path, error)),
        };
        let target = if metadata.as_ref().is_some_and(|value| value.is_symlink()) {
            Some(fs::read_link(path).map_err(|error| io_error("read index symlink", path, error))?)
        } else {
            None
        };
        Ok(Self { metadata, target })
    }

    fn check(&self, path: &Path) -> Result<(), StorageError> {
        let current = Self::read(path)?;
        let unchanged = match (&self.metadata, &current.metadata) {
            (None, None) => true,
            (Some(before), Some(after)) => {
                let same_kind = before.file_type() == after.file_type();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    same_kind && before.dev() == after.dev() && before.ino() == after.ino()
                }
                #[cfg(not(unix))]
                {
                    same_kind
                }
            }
            _ => false,
        };
        if !unchanged || self.target != current.target {
            return Err(StorageError::Changed(path.into()));
        }
        Ok(())
    }
}

/// An exclusive `index.lock` held from snapshot read through publication or drop.
///
/// Obtain with [`Repository::edit_index`], [`Repository::edit_index_at`] or
/// [`Repository::edit_index_at_with_options`], derive edits from
/// [`Self::index`], and call [`Self::replace_entries`] then [`Self::commit`]. Missing storage
/// starts as an empty index. Dropping without committing abandons the edit and removes only the
/// acquired lock. Existing locks are never stolen. Cleanup is best effort; a filesystem cleanup
/// failure can leave the owned lock for manual removal. [`Self::abort`] explicitly reports cleanup
/// errors; failed reads and commits retain both operation and cleanup causes. Unix
/// cleanup/publication rechecks the acquired lock inode, refusing an observed replacement.
/// Processes must honor Git's lock protocol; arbitrary directory, symlink or lock replacement is
/// outside this contract.
///
/// Publication uses a same-directory rename, with atomic replacement where the host filesystem
/// supports it. Default [`Self::commit`] uses ordinary OS creation modes and umask, without file
/// synchronization. [`Self::commit_with_options`] can adjust the lock's shared permissions and
/// synchronize it before rename, using full file flush on macOS or `File::sync_all` elsewhere.
/// Directory entries are not synchronized; no crash or power-loss durability is promised. No
/// worktree files or objects are written. Index storage does not depend on the reference backend.
#[derive(Debug)]
pub struct IndexEdit {
    destination: PathBuf,
    lock_path: PathBuf,
    file: Option<File>,
    original: Option<Vec<u8>>,
    leaf: Option<LeafSnapshot>,
    shared: Option<(PathBuf, Vec<u8>)>,
    lock_identity: fs::Metadata,
    index: Index,
    limits: Limits,
    published: bool,
}

/// Synchronous index storage failure, retaining path and underlying causes.
#[derive(Debug, Error)]
pub enum StorageError {
    /// The operation failed and its owned lock could not be removed. Both causes are retained;
    /// the cleanup cause identifies the lock requiring manual recovery.
    #[error("{operation}; additionally, lock cleanup failed: {cleanup}")]
    Cleanup {
        /// Primary failure.
        #[source]
        operation: Box<StorageError>,
        /// Failed removal of the acquired lock.
        cleanup: Box<StorageError>,
    },
    /// Read, lock write, metadata update or rename failed. Original index bytes are unchanged
    /// by a failed publication; a concurrent writer's changes are never rolled back.
    #[error("cannot {operation} {path}: {source}")]
    Io {
        /// Operation that failed.
        operation: &'static str,
        /// Affected filesystem path.
        path: PathBuf,
        /// Original OS error.
        #[source]
        source: io::Error,
    },
    /// An existing lock belongs to another writer or requires explicit stale-lock recovery.
    #[error("index lock already exists: {0}")]
    Locked(PathBuf),
    /// Original index bytes or presence changed despite the held lock.
    #[error("index changed while locked: {0}")]
    Changed(PathBuf),
    /// The required immutable shared file is absent. Restore it before retrying.
    #[error("missing shared index: {0}")]
    MissingShared(PathBuf),
    /// The destination is a symlink, directory or other non-regular file.
    #[error("index is not a regular file: {0}")]
    NotRegular(PathBuf),
    /// Format or limit validation failed before publication.
    #[error("invalid index at {path}: {source}")]
    Format {
        /// Index destination path.
        path: PathBuf,
        /// Underlying parser/encoder error.
        #[source]
        source: Error,
    },
}

impl Repository {
    /// Reads this worktree's `git_dir()/index`, returning `None` when missing.
    ///
    /// An existing empty index is `Some(Index)` with zero entries. Bare and separate-gitdir
    /// repositories use their Git directory; linked worktrees use the per-worktree directory,
    /// with split dependencies resolved beside that index. Environment/config index-path overrides
    /// are not consulted. The returned read-only snapshot has no authority to replace a later
    /// index; start an [`Self::edit_index`] lifecycle before deriving changes that will be
    /// published.
    ///
    /// # Errors
    ///
    /// Returns contextual I/O, non-regular-file, format and limit errors. Reads are bounded and
    /// synchronous; nothing is written. Split dependencies share the aggregate byte budget.
    /// Concurrent cooperating writers publish whole files by rename. In-place writes by
    /// noncooperating processes may instead produce a parse error.
    pub fn read_index(&self, limits: Limits) -> Result<Option<Index>, StorageError> {
        self.read_index_path(self.git_dir().join("index"), limits, true)
    }

    /// Reads a caller-selected standalone index, returning `None` when absent.
    ///
    /// Relative paths resolve once against the process current directory, not the Git directory.
    /// No environment or configuration override is read. Symlink ancestors retain their spelling;
    /// the final component must be a regular file. Unlike Git, this API rejects leaf symlinks.
    /// The repository selects the object format. No other index path is read or changed.
    ///
    /// # Errors
    ///
    /// Returns [`Self::read_index`]'s bounded I/O, format and non-regular-file errors. Split `link`
    /// extensions are rejected as [`Error::MandatoryExtension`] without reading a shared file,
    /// even when `path` names the default index. Nothing is written.
    pub fn read_index_at(
        &self,
        path: impl AsRef<Path>,
        limits: Limits,
    ) -> Result<Option<Index>, StorageError> {
        let path = absolute_index_path(path.as_ref())?;
        self.read_index_path(path, limits, false)
    }

    fn read_index_path(
        &self,
        path: PathBuf,
        limits: Limits,
        resolve_split: bool,
    ) -> Result<Option<Index>, StorageError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "index.read_index",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            read_bytes(&path, limits)?
                .map(|bytes| {
                    parse_storage(self.object_format(), &path, &bytes, limits, resolve_split)
                        .map(|(index, _)| index)
                })
                .transpose()
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| crate::trace::index(error, &span));

        result
    }

    /// Exclusively locks the per-worktree index, then reads and validates its current bytes.
    ///
    /// The repository selects SHA-1 or SHA-256; existing bytes never override its format.
    /// Derive drafts from the returned guard rather than a previously read snapshot. Absence
    /// becomes an empty editable index, but does not create `index` until commit. Existing locks
    /// are preserved. Parse/read failure releases the newly acquired lock.
    ///
    /// # Errors
    ///
    /// Returns lock contention, I/O, unsupported/malformed index or resource errors. No existing
    /// index bytes are modified. See [`IndexEdit`] for filesystem and cleanup assumptions.
    pub fn edit_index(&self, limits: Limits) -> Result<IndexEdit, StorageError> {
        self.edit_index_path(
            self.git_dir().join("index"),
            limits,
            EditOptions {
                resolve_split: true,
                ..EditOptions::default()
            },
        )
    }

    /// Locks and edits a caller-selected standalone index.
    ///
    /// Uses [`Self::read_index_at`]'s path, format and symlink policy. Relative paths are made
    /// absolute before acquiring the lock and remain fixed throughout the edit. The adjacent lock
    /// name appends `.lock` to the complete filename, including any extension. Parent directories
    /// must already exist. Missing storage starts empty and is created only by explicit commit.
    /// Existing locks are never stolen. Publication uses [`IndexEdit`]'s unchanged snapshot and
    /// lock-identity checks; it never retries through another index path.
    ///
    /// # Errors
    ///
    /// Returns [`Self::edit_index`]'s lock, I/O, format and resource failures. Split `link`
    /// extensions are rejected without reading shared dependencies. Failure releases only the
    /// acquired lock; cleanup failures are retained. No other index path is read or changed.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use girt::{Repository, index::Limits};
    /// # let repository = Repository::open("project")?;
    /// let mut edit = repository.edit_index_at("temporary.index", Limits::default())?;
    /// edit.replace_entries(Vec::new())?;
    /// edit.commit()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn edit_index_at(
        &self,
        path: impl AsRef<Path>,
        limits: Limits,
    ) -> Result<IndexEdit, StorageError> {
        self.edit_index_at_with_options(path, limits, EditOptions::default())
    }

    /// Locks a selected index with explicit split and symlink admission.
    ///
    /// Paths are fixed lexically before locking. Split dependencies are read beside that path,
    /// including when its final leaf is a symlink; the referent's parent is never used for lookup.
    /// A followed missing target starts an empty draft. Publication replaces the selected symlink,
    /// never its referent. Leaf identity/spelling and followed bytes are rechecked before rename.
    /// See [`IndexEdit`] for cooperating-writer, cleanup and durability requirements.
    ///
    /// # Errors
    ///
    /// Returns the lock, bounded-read, format and cleanup errors of [`Self::edit_index_at`]. A
    /// required absent shared file is [`StorageError::MissingShared`], never an empty replacement.
    ///
    /// ```no_run
    /// # use girt::{Repository, index::{EditOptions, Limits}};
    /// # let repository = Repository::open("project")?;
    /// let options = EditOptions {
    ///     resolve_split: true,
    ///     follow_symlink: true,
    /// };
    /// let mut edit =
    ///     repository.edit_index_at_with_options("alternate.index", Limits::default(), options)?;
    /// edit.discard_optional_extensions(&[*b"REUC"])?;
    /// edit.commit()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn edit_index_at_with_options(
        &self,
        path: impl AsRef<Path>,
        limits: Limits,
        options: EditOptions,
    ) -> Result<IndexEdit, StorageError> {
        self.edit_index_path(absolute_index_path(path.as_ref())?, limits, options)
    }

    fn edit_index_path(
        &self,
        destination: PathBuf,
        limits: Limits,
        options: EditOptions,
    ) -> Result<IndexEdit, StorageError> {
        IndexEdit::acquire(self.object_format(), destination, limits, options)
    }
}

impl IndexEdit {
    pub(crate) fn acquire(
        format: ObjectFormat,
        destination: PathBuf,
        limits: Limits,
        options: EditOptions,
    ) -> Result<IndexEdit, StorageError> {
        let destination = absolute_index_path(&destination)?;
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "index.edit_index",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            let mut lock_name = destination.as_os_str().to_os_string();
            lock_name.push(".lock");
            let lock_path = PathBuf::from(lock_name);
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
                .map_err(|source| {
                    if source.kind() == io::ErrorKind::AlreadyExists {
                        StorageError::Locked(lock_path.clone())
                    } else {
                        io_error("create lock", &lock_path, source)
                    }
                })?;
            let lock_identity = file.metadata().map_err(|source| StorageError::Cleanup {
                operation: Box::new(io_error("inspect acquired lock", &lock_path, source)),
                cleanup: Box::new(io_error(
                    "identify lock for cleanup",
                    &lock_path,
                    io::Error::other("cannot safely remove lock without its file identity"),
                )),
            })?;
            let mut edit = IndexEdit {
                destination,
                lock_path,
                lock_identity,
                file: Some(file),
                original: None,
                leaf: None,
                shared: None,
                index: Index::empty(format),
                limits,
                published: false,
            };
            let result = (|| {
                if options.follow_symlink {
                    edit.leaf = Some(LeafSnapshot::read(&edit.destination)?);
                }
                edit.original =
                    read_selected_bytes(&edit.destination, limits, options.follow_symlink)?;
                if let Some(bytes) = &edit.original {
                    (edit.index, edit.shared) = parse_storage(
                        format,
                        &edit.destination,
                        bytes,
                        limits,
                        options.resolve_split,
                    )?;
                }
                if let Some(leaf) = &edit.leaf {
                    leaf.check(&edit.destination)?;
                }
                Ok(())
            })();
            if let Err(operation) = result {
                return Err(with_cleanup(operation, edit.abort()));
            }
            Ok(edit)
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| crate::trace::index(error, &span));

        result
    }
}

impl IndexEdit {
    /// Borrows the snapshot read after acquiring the lock, including accepted edits.
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Validates and replaces drafts under the guard's resource and extension policy.
    ///
    /// # Errors
    ///
    /// Propagates [`Index::replace_entries`] errors and preserves the previous in-memory index.
    /// No bytes are written; the guard continues to own the lock after failure.
    pub fn replace_entries(&mut self, entries: Vec<Entry>) -> Result<(), Error> {
        self.index.replace_entries(entries, self.limits)
    }

    /// Replaces the complete draft with a caller-supplied standalone index.
    ///
    /// The held object format and limits are validated before mutation. Original primary/shared
    /// snapshots and lock ownership remain unchanged; only explicit commit publishes the draft.
    ///
    /// # Errors
    ///
    /// Foreign formats, limit failures and any `link` extension leave the previous draft intact.
    /// A replacement cannot introduce a shared dependency that this guard has not captured.
    pub fn replace_index(&mut self, index: Index) -> Result<(), Error> {
        crate::ObjectId::null(index.object_format()).require_format(self.index.object_format())?;
        if index
            .extensions()
            .iter()
            .any(|extension| extension.signature() == *b"link")
        {
            return Err(Error::ExtensionPreventsEdit(*b"link"));
        }
        index.encoded_len(self.limits)?;
        self.index = index;
        Ok(())
    }

    /// Explicitly discards selected optional extensions from the held draft.
    ///
    /// Only uppercase-leading signatures are accepted. An actual removal also drops `EOIE` and
    /// `IEOT`, whose offsets can change during encoding. Split drafts become standalone at the
    /// same version, dropping `link` and derived caches while retaining unselected `REUC` and
    /// the original shared-file publication check. Standalone sparse markers are retained.
    ///
    /// # Errors
    ///
    /// Mandatory signatures, retained unknown extensions during split conversion and resource
    /// failures leave the entire draft unchanged. No matching extension is a byte-preserving no-op.
    pub fn discard_optional_extensions(&mut self, signatures: &[[u8; 4]]) -> Result<(), Error> {
        self.index
            .discard_optional_extensions(signatures, self.limits)
    }

    /// Expands sparse directories in the held draft, leaving publication to `commit`.
    ///
    /// # Errors
    ///
    /// See [`Index::expand_sparse`]. Failure preserves both draft and storage; the lock stays held.
    pub fn expand_sparse(
        &mut self,
        objects: &crate::Objects,
        limits: super::SparseLimits,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<(), super::SparseError> {
        self.index
            .expand_sparse(objects, self.limits, limits, cancelled)
    }

    /// Reuses locked cached stat words for unchanged draft entries, then validates replacement.
    ///
    /// Matches path, stage, mode, object ID and all flags. Changed/new entries retain the caller's
    /// supplied stat words; no filesystem verification occurs. The caller still owns staging and
    /// skip-worktree/intent-to-add choices. Derived cache extensions are invalidated exactly as in
    /// [`Self::replace_entries`]. Publication retains the conservative racy-stat timestamp policy.
    ///
    /// # Errors
    ///
    /// Returns validation/extension errors without changing the draft index or storage.
    pub fn replace_entries_reusing_stat(&mut self, mut entries: Vec<Entry>) -> Result<(), Error> {
        let old = self.index.entries();
        for entry in &mut entries {
            let found = old.binary_search_by(|candidate| {
                candidate
                    .path
                    .cmp(&entry.path)
                    .then(candidate.stage.cmp(&entry.stage))
            });
            if let Ok(position) = found {
                let previous = &old[position];
                if previous.id == entry.id
                    && previous.mode == entry.mode
                    && previous.assume_valid == entry.assume_valid
                    && previous.intent_to_add == entry.intent_to_add
                    && previous.skip_worktree == entry.skip_worktree
                {
                    entry.stat = previous.stat;
                }
            }
        }
        self.replace_entries(entries)
    }

    /// Selects framing under the guard's extension and resource policy.
    ///
    /// # Errors
    ///
    /// See [`Index::set_version`]; failures leave the snapshot and storage unchanged.
    pub fn set_version(&mut self, version: super::Version) -> Result<(), Error> {
        self.index.set_version(version, self.limits)
    }

    /// Converts a resolved split index to standalone storage without changing its entries or
    /// version.
    ///
    /// Preserves paths, IDs, stages, flags and stat words, even when the logical entries and
    /// framing version are unchanged. Removes `link` and the derived `TREE`, `UNTR`, `FSMN`,
    /// `EOIE` and `IEOT` caches. Resolve-undo (`REUC`) bytes are retained. An already
    /// standalone supported index retains its exact encoding, including caches.
    ///
    /// This only changes the held draft. The shared file is never written or removed; its original
    /// bytes remain a publication precondition alongside the primary index. Call [`Self::commit`]
    /// to publish, or drop the guard to discard the draft. The existing lock stays held throughout.
    ///
    /// # Errors
    ///
    /// Sparse (`sdir`) and unknown extensions are rejected, including on standalone input. Resource
    /// or extension errors preserve the draft and storage. Shared-file resolution errors are
    /// reported earlier by [`Repository::edit_index`]. Explicit alternate indexes with split
    /// dependencies remain unsupported by [`Repository::edit_index_at`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let repository = girt::Repository::open("project")?;
    /// let mut edit = repository.edit_index(girt::index::Limits::default())?;
    /// edit.make_standalone()?;
    /// edit.commit()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn make_standalone(&mut self) -> Result<(), Error> {
        self.index.make_standalone(self.limits)
    }

    /// Discards standalone entry-offset accelerators (`EOIE` and `IEOT`) under the held lock.
    ///
    /// Preserves entries, stat words, flags, framing version and opaque `TREE`/`REUC`/`sdir`
    /// payloads. Both offset caches must be removed: canonical re-encoding can change entry byte
    /// boundaries even when version and logical entries are unchanged. With neither cache present,
    /// the original encoding is retained. Publication remains explicit through [`Self::commit`].
    ///
    /// # Errors
    ///
    /// Other extensions, including split-index `link`, prevent this bounded rewrite. Extension or
    /// output-limit failures leave the snapshot and storage unchanged. Cache payloads are not
    /// interpreted or repaired; they are discarded.
    pub fn invalidate_entry_offsets(&mut self) -> Result<(), Error> {
        self.index.invalidate_entry_offsets(self.limits)
    }

    /// Discards a standalone index's resolve-undo (`REUC`) information under the held lock.
    ///
    /// Preserves entries, stat words, flags, version and `TREE` payloads. Resolve-undo payloads
    /// are discarded opaquely, without interpreting or repairing them. Use this only when the
    /// caller's operation intentionally forgets previous conflict resolutions. An absent `REUC`
    /// retains the original encoding. Publication remains explicit through [`Self::commit`].
    ///
    /// # Errors
    ///
    /// Any extension other than `TREE` or `REUC`, including split `link`, prevents this edit.
    /// Call [`Self::invalidate_entry_offsets`] first if `EOIE` or `IEOT` is present. Extension
    /// and output-limit errors preserve the draft and storage; the lock remains held. This does
    /// not relax the extension policies of entry replacement or tree-cache invalidation.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use girt::{Repository, index::Limits};
    /// # let repository = Repository::open("project")?;
    /// let mut edit = repository.edit_index(Limits::default())?;
    /// edit.invalidate_entry_offsets()?;
    /// edit.discard_resolve_undo()?;
    /// edit.commit()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn discard_resolve_undo(&mut self) -> Result<(), Error> {
        self.index.discard_resolve_undo(self.limits)
    }

    /// Discards a standalone index's `TREE` cache under the held lock.
    ///
    /// Entry replacement already discards `TREE` when entries change. Use this method when the
    /// cached tree must be removed even though the entries are unchanged. No bytes are written
    /// until [`Self::commit`]. An index without extensions is accepted as a no-op.
    ///
    /// # Errors
    ///
    /// Any extension other than `TREE`, including a split-index `link`, prevents this edit.
    /// Resource-limit failure also leaves the draft and stored index unchanged. The lock remains
    /// held after either failure.
    pub fn invalidate_tree_cache(&mut self) -> Result<(), Error> {
        self.index.invalidate_tree_cache(self.limits)
    }

    /// Encodes, rechecks the original bytes, writes the owned lock and renames it over `index`.
    ///
    /// Consumes the guard on success or failure. A final exact-byte/presence comparison detects
    /// noncooperating changes observed before rename, but cannot exclude a noncooperating writer
    /// racing after that comparison. Cooperating writers remain excluded for the entire lifecycle.
    ///
    /// The lock's modification time is set to one second after the Unix epoch before publication,
    /// conservatively keeping nonzero cached entry mtimes racy in Git until Git refreshes them.
    /// This avoids making a previously racy entry appear clean merely by rewriting the index
    /// later. Entry stat words remain exact; filesystem support for setting that timestamp is
    /// required. Callers supplying new stat data still own its correctness and any future
    /// worktree-comparison policy.
    ///
    /// # Errors
    ///
    /// Encoding, precondition, write, timestamp and rename failures preserve the destination's
    /// bytes, apart from independent concurrent changes. The acquired lock is cleaned on failure
    /// where possible. Successful return reports publication, not crash durability.
    pub fn commit(self) -> Result<(), StorageError> {
        self.commit_with_options(IndexCommitOptions::default())
    }

    /// Publishes with explicit lock-file permissions and optional file synchronization.
    ///
    /// Permission and synchronization failures occur before rename and preserve the selected
    /// destination. Existing snapshot/lock checks and cleanup behavior remain in force. Successful
    /// file synchronization does not promise directory-entry or power-loss durability.
    ///
    /// # Errors
    ///
    /// Returns [`Self::commit`]'s errors plus invalid/unsupported mode and file-sync failures.
    /// Exact modes are validated before writing the lock. No synchronization failure is ignored.
    pub fn commit_with_options(mut self, options: IndexCommitOptions) -> Result<(), StorageError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "index.commit",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            if let Err(operation) = self.publish_with_options(options) {
                return Err(with_cleanup(operation, self.abort()));
            }
            Ok(())
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| crate::trace::index(error, &span));

        result
    }

    /// Releases the owned lock without publishing and reports cleanup failure.
    ///
    /// # Errors
    ///
    /// Returns the lock path and I/O cause when removal fails. The lock may remain for manual
    /// recovery; no index or working-tree content is changed.
    pub fn abort(mut self) -> Result<(), StorageError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "index.abort",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            self.published = true; // Do not silently retry an explicitly reported cleanup failure.
            self.check_lock_identity()?;
            drop(self.file.take());
            fs::remove_file(&self.lock_path)
                .map_err(|source| io_error("remove owned index lock", &self.lock_path, source))
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| crate::trace::index(error, &span));

        result
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn discard_tree_cache(&mut self) {
        self.index.discard_tree_cache();
    }

    pub(crate) fn publish(&mut self) -> Result<(), StorageError> {
        self.publish_with_options(IndexCommitOptions::default())
    }

    fn publish_with_options(&mut self, options: IndexCommitOptions) -> Result<(), StorageError> {
        self.publish_with_policy(options, crate::file_policy::sync_file, |from, to| {
            fs::rename(from, to)
        })
    }

    #[cfg(test)]
    fn publish_with_rename(
        &mut self,
        rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
    ) -> Result<(), StorageError> {
        self.publish_with_policy(
            IndexCommitOptions::default(),
            crate::file_policy::sync_file,
            rename,
        )
    }

    fn publish_with_policy(
        &mut self,
        options: IndexCommitOptions,
        sync: impl FnOnce(&File) -> io::Result<()>,
        rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
    ) -> Result<(), StorageError> {
        options
            .shared_permissions
            .validate()
            .map_err(|source| io_error("validate index permissions", &self.lock_path, source))?;
        let bytes = self
            .index
            .encode(self.limits)
            .map_err(|source| StorageError::Format {
                path: self.destination.clone(),
                source,
            })?;
        self.check_original()?;
        self.write_lock(&bytes)?;
        let file = self.file.as_ref().expect("unpublished guard owns its file");
        options
            .shared_permissions
            .apply_file(file)
            .map_err(|source| io_error("set index permissions", &self.lock_path, source))?;
        if options.sync {
            sync(file)
                .map_err(|source| io_error("synchronize index lock", &self.lock_path, source))?;
        }
        self.check_original()?;
        self.check_lock_identity()?;
        drop(self.file.take());
        rename(&self.lock_path, &self.destination)
            .map_err(|source| io_error("replace index", &self.destination, source))?;
        self.published = true;
        Ok(())
    }

    fn check_lock_identity(&self) -> Result<(), StorageError> {
        let current = fs::symlink_metadata(&self.lock_path)
            .map_err(|source| io_error("inspect owned index lock", &self.lock_path, source))?;
        if !current.is_file() {
            return Err(StorageError::Changed(self.lock_path.clone()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owned = &self.lock_identity;
            if current.dev() != owned.dev() || current.ino() != owned.ino() {
                return Err(StorageError::Changed(self.lock_path.clone()));
            }
        }
        #[cfg(not(unix))]
        let _ = &self.lock_identity;
        Ok(())
    }

    fn check_original(&self) -> Result<(), StorageError> {
        if let Some(leaf) = &self.leaf {
            leaf.check(&self.destination)?;
        }
        if read_selected_bytes(&self.destination, self.limits, self.leaf.is_some())?
            != self.original
        {
            return Err(StorageError::Changed(self.destination.clone()));
        }
        if let Some((path, bytes)) = &self.shared
            && read_bytes(path, self.limits)?.as_ref() != Some(bytes)
        {
            return Err(StorageError::Changed(path.clone()));
        }
        Ok(())
    }

    fn write_lock(&mut self, bytes: &[u8]) -> Result<(), StorageError> {
        let file = self.file.as_mut().expect("unpublished guard owns its file");
        file.write_all(bytes)
            .map_err(|source| io_error("write lock", &self.lock_path, source))?;
        file.flush()
            .map_err(|source| io_error("flush lock", &self.lock_path, source))?;
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .map_err(|source| io_error("set lock timestamp", &self.lock_path, source))?;
        Ok(())
    }
}
impl Drop for IndexEdit {
    fn drop(&mut self) {
        if !self.published && self.check_lock_identity().is_ok() {
            drop(self.file.take());
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}
fn absolute_index_path(path: &Path) -> Result<PathBuf, StorageError> {
    std::path::absolute(path).map_err(|source| io_error("resolve index path", path, source))
}

fn read_bytes(path: &Path, limits: Limits) -> Result<Option<Vec<u8>>, StorageError> {
    read_selected_bytes(path, limits, false)
}

fn read_selected_bytes(
    path: &Path,
    limits: Limits,
    follow_symlink: bool,
) -> Result<Option<Vec<u8>>, StorageError> {
    let metadata = match if follow_symlink {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    } {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error("inspect index", path, source)),
    };
    if !metadata.is_file() {
        return Err(StorageError::NotRegular(path.into()));
    }
    let file = File::open(path).map_err(|source| io_error("open index", path, source))?;
    let mut bytes = Vec::new();
    let limit = (limits.max_bytes as u64).saturating_add(1);
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error("read index", path, source))?;
    if bytes.len() > limits.max_bytes {
        return Err(StorageError::Format {
            path: path.into(),
            source: Error::Limit("bytes"),
        });
    }
    Ok(Some(bytes))
}
type SharedFile = Option<(PathBuf, Vec<u8>)>;
fn parse_storage(
    format: crate::ObjectFormat,
    path: &Path,
    bytes: &[u8],
    limits: Limits,
    resolve_split: bool,
) -> Result<(Index, SharedFile), StorageError> {
    let contextual = |source| StorageError::Format {
        path: path.into(),
        source,
    };
    let index = Index::parse_file(format, bytes, limits).map_err(contextual)?;
    if !resolve_split
        && index
            .extensions()
            .iter()
            .any(|extension| extension.signature() == *b"link")
    {
        return Err(contextual(Error::MandatoryExtension(*b"link")));
    }
    let shared = if let Some(id) = index.shared_index_id() {
        let shared_path = path.with_file_name(format!("sharedindex.{id}"));
        let remaining = Limits {
            max_bytes: limits.max_bytes.saturating_sub(bytes.len()),
            ..limits
        };
        let shared_bytes = read_bytes(&shared_path, remaining)?
            .ok_or_else(|| StorageError::MissingShared(shared_path.clone()))?;
        Some((shared_path, shared_bytes))
    } else {
        None
    };
    let index = index
        .resolve_shared(shared.as_ref().map(|(_, bytes)| bytes.as_slice()), limits)
        .map_err(contextual)?;
    Ok((index, shared))
}
fn io_error(operation: &'static str, path: &Path, source: io::Error) -> StorageError {
    StorageError::Io {
        operation,
        path: path.into(),
        source,
    }
}

#[cfg(test)]
mod tests;

fn with_cleanup(operation: StorageError, cleanup: Result<(), StorageError>) -> StorageError {
    match cleanup {
        Ok(()) => operation,
        Err(cleanup) => StorageError::Cleanup {
            operation: Box::new(operation),
            cleanup: Box::new(cleanup),
        },
    }
}

#[cfg(test)]
mod options_tests;
