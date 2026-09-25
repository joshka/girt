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
/// owned lock for manual removal. Processes must honor Git's lock protocol; malicious directory,
/// symlink or lock replacement is outside this contract.
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
    index: Index,
    limits: Limits,
    published: bool,
}

/// Synchronous index storage failure, retaining path and underlying causes.
#[derive(Debug, Error)]
pub enum StorageError {
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
    /// never the shared index. Environment/config index-path overrides are not consulted.
    /// The returned read-only snapshot has no authority to replace a later index; start an
    /// [`Self::edit_index`] lifecycle before deriving changes that will be published.
    ///
    /// # Errors
    ///
    /// Returns contextual I/O, non-regular-file, format and limit errors. Reads are bounded and
    /// synchronous; nothing is written. Concurrent cooperating writers publish whole files by
    /// rename. In-place writes by noncooperating processes may instead produce a parse error.
    pub fn read_index(&self, limits: Limits) -> Result<Option<Index>, StorageError> {
        let path = self.git_dir().join("index");
        read_bytes(&path, limits)?
            .map(|bytes| parse(&path, &bytes, limits))
            .transpose()
    }

    /// Exclusively locks the per-worktree index, then reads and validates its current bytes.
    ///
    /// Derive drafts from the returned guard rather than a previously read snapshot. Absence
    /// becomes an empty editable index, but does not create `index` until commit. Existing locks
    /// are preserved. Parse/read failure releases the newly acquired lock.
    ///
    /// # Errors
    ///
    /// Returns lock contention, I/O, unsupported/malformed index or resource errors. No existing
    /// index bytes are modified. See [`IndexEdit`] for filesystem and cleanup assumptions.
    pub fn edit_index(&self, limits: Limits) -> Result<IndexEdit, StorageError> {
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
        let mut edit = IndexEdit {
            destination,
            lock_path,
            file: Some(file),
            original: None,
            index: Index::default(),
            limits,
            published: false,
        };
        edit.original = read_bytes(&edit.destination, limits)?;
        if let Some(bytes) = &edit.original {
            edit.index = parse(&edit.destination, bytes, limits)?;
        }
        Ok(edit)
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
        // Closing before rename also permits replacement on hosts that restrict open-file moves.
        drop(self.file.take());
        fs::rename(&self.lock_path, &self.destination)
            .map_err(|source| io_error("replace index", &self.destination, source))?;
        self.published = true;
        Ok(())
    }

    fn check_original(&self) -> Result<(), StorageError> {
        if read_bytes(&self.destination, self.limits)? != self.original {
            return Err(StorageError::Changed(self.destination.clone()));
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
        drop(self.file.take());
        if !self.published {
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
fn parse(path: &Path, bytes: &[u8], limits: Limits) -> Result<Index, StorageError> {
    Index::parse(bytes, limits).map_err(|source| StorageError::Format {
        path: path.into(),
        source,
    })
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
