use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::{Entry, Error, Index, Limits};
use crate::Repository;

/// An exclusive `index.lock` held from snapshot read through publication or drop.
///
/// Obtain with [`Repository::edit_index`], derive edits from [`Self::index`], and call
/// [`Self::replace_entries`] then [`Self::commit`]. Missing storage starts as an empty index.
/// Dropping without committing abandons the edit and removes only the acquired lock. Existing
/// locks are never stolen. Cleanup is best effort; a filesystem cleanup failure can leave the
/// owned lock for manual removal. [`Self::abort`] explicitly reports cleanup errors; failed reads
/// and commits retain both operation and cleanup causes. Unix cleanup/publication rechecks the
/// acquired lock inode, refusing an observed replacement. Processes must honor Git's lock protocol;
/// arbitrary directory, symlink or lock replacement is outside this contract.
///
/// Publication uses a same-directory rename, with atomic replacement where the host filesystem
/// supports it. No fsync or crash durability is promised. No shared-repository permission policy
/// is implemented; lock creation uses ordinary OS defaults and umask. No worktree files or objects
/// are written. Index storage does not depend on the reference backend.
#[derive(Debug)]
pub struct IndexEdit {
    destination: PathBuf,
    lock_path: PathBuf,
    file: Option<File>,
    original: Option<Vec<u8>>,
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
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "index.read_index",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            let path = self.git_dir().join("index");
            read_bytes(&path, limits)?
                .map(|bytes| {
                    parse_storage(self.object_format(), &path, &bytes, limits)
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
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "index.edit_index",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            let destination = self.git_dir().join("index");
            let lock_path = self.git_dir().join("index.lock");
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
                shared: None,
                index: Index::empty(self.object_format()),
                limits,
                published: false,
            };
            let result = (|| {
                edit.original = read_bytes(&edit.destination, limits)?;
                if let Some(bytes) = &edit.original {
                    (edit.index, edit.shared) =
                        parse_storage(self.object_format(), &edit.destination, bytes, limits)?;
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
    pub fn commit(mut self) -> Result<(), StorageError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "index.commit",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            if let Err(operation) = self.publish() {
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
        self.publish_with_rename(|from, to| fs::rename(from, to))
    }

    fn publish_with_rename(
        &mut self,
        rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
    ) -> Result<(), StorageError> {
        let bytes = self
            .index
            .encode(self.limits)
            .map_err(|source| StorageError::Format {
                path: self.destination.clone(),
                source,
            })?;
        self.check_original()?;
        self.write_lock(&bytes)?;
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
        if read_bytes(&self.destination, self.limits)? != self.original {
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
fn read_bytes(path: &Path, limits: Limits) -> Result<Option<Vec<u8>>, StorageError> {
    let metadata = match fs::symlink_metadata(path) {
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
) -> Result<(Index, SharedFile), StorageError> {
    let contextual = |source| StorageError::Format {
        path: path.into(),
        source,
    };
    let index = Index::parse_file(format, bytes, limits).map_err(contextual)?;
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
