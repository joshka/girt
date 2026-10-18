//! Additive recovery files for dangling objects. Stored objects and references are never changed.
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::Report;
use crate::{ObjectKind, Repository};

/// Bounds for exporting dangling objects.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct LostFoundLimits {
    /// Maximum files considered, including already matching recovery files.
    pub max_objects: usize,
    /// Maximum total decoded object payload bytes.
    pub max_bytes: u64,
    /// Per-object decoding bounds.
    pub read: crate::ReadLimits,
}
impl Default for LostFoundLimits {
    fn default() -> Self {
        Self {
            max_objects: 1 << 20,
            max_bytes: 1 << 30,
            read: Default::default(),
        }
    }
}

/// Partial recovery output and the cause that stopped publication. Completed files remain useful.
#[derive(Debug, thiserror::Error)]
#[error("lost-found publication failed")]
#[non_exhaustive]
pub struct LostFoundError {
    /// Files successfully created or confirmed byte-identical before failure.
    pub written: Vec<PathBuf>,
    /// Original typed failure.
    #[source]
    pub cause: LostFoundCause,
}

/// Why recovery-file publication stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LostFoundCause {
    /// Storage lookup or identity verification failed.
    #[error(transparent)]
    Read(#[from] Box<crate::ObjectReadError>),
    /// A previously observed dangling object disappeared.
    #[error("dangling object disappeared: {0}")]
    Missing(crate::ObjectId),
    /// A recovery directory is a symlink/non-directory, or an existing file differs.
    #[error("unsafe or conflicting lost-found path: {0}")]
    Path(PathBuf),
    /// I/O failure at the indicated path.
    #[error("lost-found I/O at {path}")]
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Original failure.
        #[source]
        source: std::io::Error,
    },
    /// Resource exhaustion.
    #[error("lost-found limit exceeded")]
    Limit,
    /// Caller cancellation.
    #[error("lost-found publication cancelled")]
    Cancelled,
}

impl Repository {
    /// Exports dangling objects under the common Git directory's `lost-found` directory.
    ///
    /// Uses default bounds and no cancellation; see [`Self::write_lost_found_with`].
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repository = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("repo"),
    ///     InitKind::Bare,
    /// )?;
    /// let id = repository.loose_objects().write_blob(b"recover this")?;
    /// let report = repository.fsck()?;
    /// let paths = repository.write_lost_found(&report)?;
    ///
    /// assert_eq!(paths.len(), 1);
    /// assert_eq!(std::fs::read(&paths[0])?, b"recover this");
    /// assert!(paths[0].ends_with(id.to_string()));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns partial output with storage, path, I/O or resource failures.
    pub fn write_lost_found(&self, report: &Report) -> Result<Vec<PathBuf>, LostFoundError> {
        self.write_lost_found_with(report, LostFoundLimits::default(), &AtomicBool::new(false))
    }

    /// Writes Git-compatible recovery files: commit IDs in `lost-found/commit`, raw blob payloads
    /// and IDs for other kinds in `lost-found/other`. Existing identical regular files are
    /// accepted; conflicting files and symlinks are refused. No stored objects or refs are
    /// changed.
    ///
    /// The report is advisory: every object is reread and identity-verified before output. Callers
    /// must exclude pruning and hostile recovery-directory replacement during publication. Path
    /// checks reject pre-existing symlinks but cannot prevent concurrent directory replacement.
    /// Files are flushed, not fsynced; success does not promise durability across power loss.
    ///
    /// # Errors
    ///
    /// Returns successfully completed paths with the first failure; never deletes earlier output.
    pub fn write_lost_found_with(
        &self,
        report: &Report,
        limits: LostFoundLimits,
        cancel: &AtomicBool,
    ) -> Result<Vec<PathBuf>, LostFoundError> {
        let mut written = Vec::new();
        let mut operation = || -> Result<(), LostFoundCause> {
            if report.dangling.len() > limits.max_objects {
                return Err(LostFoundCause::Limit);
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(LostFoundCause::Cancelled);
            }
            if report.dangling.is_empty() {
                return Ok(());
            }
            let objects = self
                .objects_controlled(Default::default(), Default::default(), cancel)
                .map_err(Box::new)?;
            let root = self.common_dir().join("lost-found");
            directory(&root)?;
            let mut bytes = 0u64;
            for &id in &report.dangling {
                if cancel.load(Ordering::Relaxed) {
                    return Err(LostFoundCause::Cancelled);
                }
                let object = objects
                    .read_controlled(id, limits.read, cancel)
                    .map_err(Box::new)?
                    .ok_or(LostFoundCause::Missing(id))?;
                bytes = bytes
                    .checked_add(object.data().len() as u64)
                    .ok_or(LostFoundCause::Limit)?;
                if bytes > limits.max_bytes {
                    return Err(LostFoundCause::Limit);
                }
                let category = root.join(if object.kind() == ObjectKind::Commit {
                    "commit"
                } else {
                    "other"
                });
                directory(&category)?;
                let path = category.join(id.to_string());
                let name = format!("{id}\n");
                let content = if object.kind() == ObjectKind::Blob {
                    object.data()
                } else {
                    name.as_bytes()
                };
                publish(&path, content)?;
                written.push(path);
            }
            Ok(())
        };
        match operation() {
            Ok(()) => Ok(written),
            Err(cause) => Err(LostFoundError { written, cause }),
        }
    }
}

fn directory(path: &Path) -> Result<(), LostFoundCause> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(path)
                .map_err(|source| io(path, source))?
                .is_dir()
            {
                Ok(())
            } else {
                Err(LostFoundCause::Path(path.to_owned()))
            }
        }
        Err(source) => Err(io(path, source)),
    }
}

fn publish(path: &Path, content: &[u8]) -> Result<(), LostFoundCause> {
    publish_with(path, content, |file, bytes| file.write_all(bytes))
}

fn publish_with(
    path: &Path,
    content: &[u8],
    write: impl FnOnce(&mut fs::File, &[u8]) -> std::io::Result<()>,
) -> Result<(), LostFoundCause> {
    let parent = path
        .parent()
        .ok_or_else(|| LostFoundCause::Path(path.to_owned()))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| io(path, source))?;
    write(temporary.as_file_mut(), content).map_err(|source| io(path, source))?;
    temporary
        .as_file_mut()
        .flush()
        .map_err(|source| io(path, source))?;
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(path)
                .map_err(|source| io(path, source))?
                .file_type()
                .is_file()
            {
                return Err(LostFoundCause::Path(path.to_owned()));
            }
            let mut existing = Vec::new();
            fs::File::open(path)
                .map_err(|source| io(path, source))?
                .take(content.len().saturating_add(1) as u64)
                .read_to_end(&mut existing)
                .map_err(|source| io(path, source))?;
            if existing == content {
                Ok(())
            } else {
                Err(LostFoundCause::Path(path.to_owned()))
            }
        }
        Err(error) => Err(io(path, error.error)),
    }
}

fn io(path: &Path, source: std::io::Error) -> LostFoundCause {
    LostFoundCause::Io {
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_staging_leaves_no_destination_or_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("object");
        let result = publish_with(&path, b"complete payload", |file, bytes| {
            file.write_all(&bytes[..3])?;
            Err(std::io::Error::other("injected write failure"))
        });

        assert!(result.is_err());
        assert!(!path.exists());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
        publish(&path, b"complete payload").unwrap();
        assert_eq!(fs::read(path).unwrap(), b"complete payload");
    }
}
