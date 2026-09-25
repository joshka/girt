use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::{ConfigError, Document};
use crate::Repository;

/// An exclusive configuration lock held from source read through commit or abort.
///
/// Only the selected file is edited: includes, globals and worktree configuration are not
/// implicitly rewritten. The document is a direct-file draft, never an effective snapshot.
/// After commit, re-resolve inputs or reopen the repository before constructing a new remote.
/// Existing repository and remote values remain immutable snapshots.
///
/// Cooperating writers must honor `<path>.lock`. Source bytes/presence are rechecked before
/// publication, detecting observed noncooperating changes but not a race after the final check.
/// Publication renames the owned lock in the same directory; atomic replacement depends on the
/// filesystem. Unix replacements preserve source permission bits; new files use the process
/// umask. Ownership, ACLs and extended attributes are not copied. There is no fsync, crash
/// durability, multi-file transaction or shared-repository permissions policy.
/// Destination symlinks and nonregular files are rejected. Unix lock identity is checked by inode;
/// other platforms require callers to exclude lock/directory replacement. Arbitrary hostile
/// directory replacement is outside this contract on every platform.
///
/// Drop performs best-effort cleanup. Use [`Self::abort`] for observable cleanup; errors retain
/// both primary and cleanup causes. Inspect a residual lock before manual recovery, then acquire
/// a fresh lifecycle and reread sources before retrying.
#[derive(Debug)]
pub struct ConfigEdit {
    destination: PathBuf,
    lock_path: PathBuf,
    file: Option<File>,
    original: Option<Vec<u8>>,
    lock_identity: fs::Metadata,
    document: Document,
    max_bytes: usize,
    published: bool,
}

/// Synchronous config storage failure, retaining path and underlying causes.
#[derive(Debug, Error)]
pub enum EditError {
    /// Input or edited output exceeds the caller byte budget.
    #[error("configuration byte limit exceeded")]
    Limit,
    /// The operation failed and its owned lock could not be removed. Both causes are retained;
    /// the cleanup cause identifies the lock requiring manual recovery.
    #[error("{operation}; additionally, lock cleanup failed: {cleanup}")]
    Cleanup {
        /// Primary failure.
        #[source]
        operation: Box<EditError>,
        /// Failed removal of the acquired lock.
        cleanup: Box<EditError>,
    },
    /// Read, lock write, metadata update or rename failed. Original config bytes are unchanged
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
    #[error("config lock already exists: {0}")]
    Locked(PathBuf),
    /// Original config bytes or presence changed despite the held lock.
    #[error("config changed while locked: {0}")]
    Changed(PathBuf),
    /// The destination is a symlink, directory or other non-regular file.
    #[error("config is not a regular file: {0}")]
    NotRegular(PathBuf),
    /// Format or limit validation failed before publication.
    #[error("invalid config at {path}: {source}")]
    Format {
        /// Configuration destination path.
        path: PathBuf,
        /// Underlying parser/encoder error.
        #[source]
        source: ConfigError,
    },
}

impl Repository {
    /// Locks and reads the common directory's local `config`, independently of this snapshot.
    ///
    /// No global, included or worktree file is changed. Reopen with the original config inputs
    /// after committing to refresh effective settings and repository bootstrap validation.
    ///
    /// # Errors
    ///
    /// Returns the lock, read, syntax and cleanup errors of [`ConfigEdit::open`].
    pub fn edit_config(&self, max_bytes: usize) -> Result<ConfigEdit, EditError> {
        ConfigEdit::open(self.common_dir().join("config"), max_bytes)
    }
}

impl ConfigEdit {
    /// Locks and reads an explicitly selected file; missing files start empty.
    ///
    /// # Errors
    ///
    /// Returns contention, read, parse and size errors, retaining cleanup failures.
    pub fn open(path: impl AsRef<Path>, max_bytes: usize) -> Result<Self, EditError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "config.edit_config",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            let destination = path.as_ref().to_path_buf();
            let mut lock_name = destination.as_os_str().to_os_string();
            lock_name.push(".lock");
            let lock_path = PathBuf::from(lock_name);
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
                .map_err(|source| {
                    if source.kind() == io::ErrorKind::AlreadyExists {
                        EditError::Locked(lock_path.clone())
                    } else {
                        io_error("create lock", &lock_path, source)
                    }
                })?;
            let lock_identity = file.metadata().map_err(|source| EditError::Cleanup {
                operation: Box::new(io_error("inspect acquired lock", &lock_path, source)),
                cleanup: Box::new(io_error(
                    "identify lock for cleanup",
                    &lock_path,
                    io::Error::other("cannot safely remove lock without its file identity"),
                )),
            })?;
            let mut edit = ConfigEdit {
                destination,
                lock_path,
                lock_identity,
                file: Some(file),
                original: None,
                document: Document::parse(b"").expect("empty configuration"),
                max_bytes,
                published: false,
            };
            let result = (|| {
                edit.original = read_bytes(&edit.destination, max_bytes)?;
                if let Some(bytes) = &edit.original {
                    edit.document = Document::parse(bytes).map_err(|source| EditError::Format {
                        path: edit.destination.clone(),
                        source,
                    })?;
                    #[cfg(unix)]
                    {
                        let permissions = fs::symlink_metadata(&edit.destination)
                            .map_err(|source| {
                                io_error("read source permissions", &edit.destination, source)
                            })?
                            .permissions();
                        edit.file
                            .as_ref()
                            .expect("acquired lock")
                            .set_permissions(permissions)
                            .map_err(|source| {
                                io_error("preserve source permissions", &edit.lock_path, source)
                            })?;
                    }
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
        crate::trace::finish(&span, &result, |error| trace_error(error, &span));

        result
    }
}

impl ConfigEdit {
    /// Current direct-file draft, without include expansion.
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// Edits the validated in-memory document while retaining the owned lock.
    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.document
    }

    /// Requires the exact originally read bytes/presence before using an earlier snapshot.
    ///
    /// # Errors
    ///
    /// Returns `Changed` for a stale snapshot; the draft and held lock remain available.
    pub fn require_source(&self, expected: Option<&[u8]>) -> Result<(), EditError> {
        if self.original.as_deref() != expected {
            return Err(EditError::Changed(self.destination.clone()));
        }
        Ok(())
    }

    /// Rechecks source bytes, writes the draft to the owned lock and publishes by rename.
    ///
    /// Consumes the guard on either outcome. No destination write occurs during preparation.
    ///
    /// # Errors
    ///
    /// Limit/precondition failures precede publication; `Io` identifies write/flush/rename
    /// failures. Failed preparation or rename preserves original bytes except independent writes.
    /// Cleanup errors retain both causes; inspect state before retrying. Success is publication,
    /// not crash durability or confirmation of the resulting effective configuration.
    pub fn commit(mut self) -> Result<(), EditError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "config.commit",
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
        crate::trace::finish(&span, &result, |error| trace_error(error, &span));

        result
    }

    /// Releases the owned lock without publishing and reports cleanup failure.
    ///
    /// # Errors
    ///
    /// Returns the lock path and I/O cause when removal fails. The lock may remain for manual
    /// recovery; no config or working-tree content is changed.
    pub fn abort(mut self) -> Result<(), EditError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "config.abort",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || self.cleanup(|path| fs::remove_file(path));
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| trace_error(error, &span));

        result
    }

    fn cleanup(&mut self, remove: impl FnOnce(&Path) -> io::Result<()>) -> Result<(), EditError> {
        self.published = true; // Do not silently retry an explicitly reported cleanup failure.
        self.check_lock_identity()?;
        drop(self.file.take());
        remove(&self.lock_path)
            .map_err(|source| io_error("remove owned config lock", &self.lock_path, source))
    }

    fn publish(&mut self) -> Result<(), EditError> {
        self.publish_with_rename(|from, to| fs::rename(from, to))
    }

    fn publish_with_rename(
        &mut self,
        rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
    ) -> Result<(), EditError> {
        let bytes = self.document.as_bytes().to_vec();
        if bytes.len() > self.max_bytes {
            return Err(EditError::Limit);
        }
        self.check_original()?;
        self.write_lock(&bytes)?;
        self.check_original()?;
        self.check_lock_identity()?;
        drop(self.file.take());
        rename(&self.lock_path, &self.destination)
            .map_err(|source| io_error("replace config", &self.destination, source))?;
        self.published = true;
        Ok(())
    }

    fn check_lock_identity(&self) -> Result<(), EditError> {
        let current = fs::symlink_metadata(&self.lock_path)
            .map_err(|source| io_error("inspect owned config lock", &self.lock_path, source))?;
        if !current.is_file() {
            return Err(EditError::Changed(self.lock_path.clone()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owned = &self.lock_identity;
            if current.dev() != owned.dev() || current.ino() != owned.ino() {
                return Err(EditError::Changed(self.lock_path.clone()));
            }
        }
        #[cfg(not(unix))]
        let _ = &self.lock_identity;
        Ok(())
    }

    fn check_original(&self) -> Result<(), EditError> {
        if read_bytes(&self.destination, self.max_bytes)? != self.original {
            return Err(EditError::Changed(self.destination.clone()));
        }
        Ok(())
    }

    fn write_lock(&mut self, bytes: &[u8]) -> Result<(), EditError> {
        let file = self.file.as_mut().expect("unpublished guard owns its file");
        file.write_all(bytes)
            .map_err(|source| io_error("write lock", &self.lock_path, source))?;
        file.flush()
            .map_err(|source| io_error("flush lock", &self.lock_path, source))?;
        Ok(())
    }
}
impl Drop for ConfigEdit {
    fn drop(&mut self) {
        if !self.published && self.check_lock_identity().is_ok() {
            drop(self.file.take());
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}
fn read_bytes(path: &Path, max_bytes: usize) -> Result<Option<Vec<u8>>, EditError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error("inspect config", path, source)),
    };
    if !metadata.is_file() {
        return Err(EditError::NotRegular(path.into()));
    }
    let file = File::open(path).map_err(|source| io_error("open config", path, source))?;
    let mut bytes = Vec::new();
    let limit = (max_bytes as u64).saturating_add(1);
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error("read config", path, source))?;
    if bytes.len() > max_bytes {
        return Err(EditError::Limit);
    }
    Ok(Some(bytes))
}
fn io_error(operation: &'static str, path: &Path, source: io::Error) -> EditError {
    EditError::Io {
        operation,
        path: path.into(),
        source,
    }
}

fn with_cleanup(operation: EditError, cleanup: Result<(), EditError>) -> EditError {
    match cleanup {
        Ok(()) => operation,
        Err(cleanup) => EditError::Cleanup {
            operation: Box::new(operation),
            cleanup: Box::new(cleanup),
        },
    }
}

#[cfg(feature = "tracing")]
fn trace_error(error: &EditError, span: &tracing::Span) -> &'static str {
    match error {
        EditError::Cleanup { operation, .. } => {
            span.record("effects", "cleanup_failed");
            trace_error(operation, span)
        }
        EditError::Limit => "limit",
        EditError::Locked(_) | EditError::Changed(_) => "precondition",
        EditError::NotRegular(_) => "unsupported",
        EditError::Format { .. } => "corrupt",
        EditError::Io { .. } => "io",
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn write_optional(path: &Path, bytes: Option<&[u8]>) {
        if let Some(bytes) = bytes {
            fs::write(path, bytes).unwrap();
        }
    }

    #[rstest]
    #[case::missing(None)]
    #[case::empty(Some(b"".as_slice()))]
    #[case::existing(Some(b"[core]\nx=old\n".as_slice()))]
    fn lifecycle_and_contention(#[case] original: Option<&[u8]>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        write_optional(&path, original);
        let mut edit = ConfigEdit::open(&path, 1024).unwrap();
        edit.require_source(original).unwrap();
        assert!(matches!(
            ConfigEdit::open(&path, 1024),
            Err(EditError::Locked(_))
        ));
        edit.document_mut()
            .append("core", None, "x", b"new")
            .unwrap();
        edit.commit().unwrap();
        let next = ConfigEdit::open(&path, 1024).unwrap();
        assert_eq!(
            next.document().config().value("core", None, "x"),
            Some(Some(b"new".as_slice()))
        );
        next.abort().unwrap();
        assert!(!dir.path().join("config.lock").exists());
    }
    #[test]
    fn stale_snapshot_and_competing_noncooperating_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(&path, b"[x]\na=first\n").unwrap();
        let edit = ConfigEdit::open(&path, 1024).unwrap();
        assert!(matches!(
            edit.require_source(None),
            Err(EditError::Changed(_))
        ));
        fs::write(&path, b"[x]\na=other\n").unwrap();
        assert!(matches!(edit.commit(), Err(EditError::Changed(_))));
        assert_eq!(fs::read(&path).unwrap(), b"[x]\na=other\n");
        assert!(!dir.path().join("config.lock").exists());
    }
    #[test]
    fn malformed_source_releases_only_owned_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(&path, b"[broken").unwrap();
        assert!(matches!(
            ConfigEdit::open(&path, 1024),
            Err(EditError::Format { .. })
        ));
        assert_eq!(fs::read(&path).unwrap(), b"[broken");
        assert!(!dir.path().join("config.lock").exists());
        fs::write(dir.path().join("config.lock"), b"foreign").unwrap();
        assert!(matches!(
            ConfigEdit::open(&path, 1024),
            Err(EditError::Locked(_))
        ));
        assert_eq!(
            fs::read(dir.path().join("config.lock")).unwrap(),
            b"foreign"
        );
    }
    #[test]
    fn failed_rename_preserves_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(&path, b"#original").unwrap();
        let mut edit = ConfigEdit::open(&path, 1024).unwrap();
        edit.document_mut().append("x", None, "a", b"new").unwrap();
        assert!(matches!(
            edit.publish_with_rename(|_, _| Err(io::Error::other("injected rename failure"))),
            Err(EditError::Io {
                operation: "replace config",
                ..
            })
        ));
        edit.abort().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"#original");
    }
    #[test]
    fn failed_descriptor_write_preserves_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(&path, b"#original").unwrap();
        let mut edit = ConfigEdit::open(&path, 1024).unwrap();
        edit.document_mut().append("x", None, "a", b"new").unwrap();
        edit.file = Some(File::open(&edit.lock_path).unwrap());
        assert!(matches!(
            edit.commit(),
            Err(EditError::Io {
                operation: "write lock",
                ..
            })
        ));
        assert_eq!(fs::read(&path).unwrap(), b"#original");
        assert!(!dir.path().join("config.lock").exists());
    }
    #[test]
    fn changed_lock_reports_cleanup_failure_without_removing_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let mut edit = ConfigEdit::open(&path, 1024).unwrap();
        edit.max_bytes = 0;
        edit.document_mut().append("x", None, "a", b"new").unwrap();
        drop(edit.file.take());
        fs::remove_file(&edit.lock_path).unwrap();
        fs::create_dir(&edit.lock_path).unwrap();
        let error = edit.commit().unwrap_err();
        assert!(matches!(error, EditError::Cleanup { .. }));
        assert!(dir.path().join("config.lock").is_dir());
        assert!(!path.exists());
    }
    #[test]
    fn limit_rejection_and_drop_preserve_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(&path, b"#original").unwrap();
        assert!(matches!(ConfigEdit::open(&path, 2), Err(EditError::Limit)));
        let mut edit = ConfigEdit::open(&path, 10).unwrap();
        edit.document_mut().append("x", None, "a", b"new").unwrap();
        assert!(matches!(edit.commit(), Err(EditError::Limit)));
        drop(ConfigEdit::open(&path, 1024).unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"#original");
        assert!(!dir.path().join("config.lock").exists());
    }
    #[test]
    fn injected_cleanup_fault_retains_both_causes_and_owned_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let mut edit = ConfigEdit::open(&path, 1024).unwrap();
        let cleanup = edit.cleanup(|_| Err(io::Error::other("injected removal failure")));
        let error = with_cleanup(EditError::Changed(path.clone()), cleanup);
        assert!(matches!(error, EditError::Cleanup { operation, cleanup }
            if matches!(*operation, EditError::Changed(_)) && matches!(*cleanup, EditError::Io { operation: "remove owned config lock", .. })));
        drop(edit);
        assert!(dir.path().join("config.lock").is_file());
        assert!(!path.exists());
    }

    #[test]
    fn deletion_after_acquisition_is_not_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(&path, b"#original").unwrap();
        let edit = ConfigEdit::open(&path, 1024).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(matches!(edit.commit(), Err(EditError::Changed(_))));
        assert!(!path.exists());
        assert!(!dir.path().join("config.lock").exists());
    }

    #[test]
    fn creation_after_acquisition_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let edit = ConfigEdit::open(&path, 1024).unwrap();
        fs::write(&path, b"#other").unwrap();
        assert!(matches!(edit.commit(), Err(EditError::Changed(_))));
        assert_eq!(fs::read(&path).unwrap(), b"#other");
    }

    #[cfg(unix)]
    #[test]
    fn replacement_preserves_restrictive_source_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(&path, b"[x]\na=old\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut edit = ConfigEdit::open(&path, 1024).unwrap();
        edit.document_mut().set_value(0, b"new").unwrap();
        edit.commit().unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_destination_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(dir.path().join("target"), b"#untouched").unwrap();
        std::os::unix::fs::symlink("target", &path).unwrap();
        assert!(matches!(
            ConfigEdit::open(&path, 1024),
            Err(EditError::NotRegular(_))
        ));
        assert_eq!(fs::read(dir.path().join("target")).unwrap(), b"#untouched");
    }
}
