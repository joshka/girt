use std::path::{Path, PathBuf};

use super::Mailmap;
use crate::{Objects, Repository};

/// A mailmap source could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem source could not be read.
    #[error("reading mailmap {path}: {source}")]
    Io {
        /// Selected source path.
        path: PathBuf,
        /// Underlying filesystem failure.
        source: std::io::Error,
    },
    /// Configuration contains a path or revision not representable on this platform.
    #[error("mailmap configuration has unsupported encoding or path expansion")]
    Configuration,
    /// Resolving or reading a blob failed.
    #[error("reading mailmap blob: {0}")]
    Object(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl Mailmap {
    /// Loads repository sources in increasing precedence: worktree `.mailmap`, configured blob,
    /// then configured file. Bare repositories default the blob to `HEAD:.mailmap`.
    ///
    /// Relative file paths use the repository worktree, or Git directory for a bare repository.
    /// Missing files and absent blob revisions are ignored. Other read failures are reported.
    /// This method reads only the repository's captured configuration; it never reads ambient
    /// environment variables. Expand `~` paths before calling, or use [`Self::extend`] with
    /// caller-loaded bytes; home expansion is explicitly refused here.
    ///
    /// # Errors
    ///
    /// Reports unreadable files, corrupt objects and unsupported configuration encodings.
    pub fn load(repository: &Repository, objects: &Objects) -> Result<Self, Error> {
        Self::load_sources(repository, objects, None)
    }

    /// Loads repository sources with an explicitly resolved replacement for `mailmap.file`.
    ///
    /// The supplied path replaces only the configured file source; worktree and blob sources
    /// retain their usual precedence. This lets a CLI expand home paths without making library
    /// loading depend on ambient environment variables.
    ///
    /// # Errors
    ///
    /// Reports the read and blob errors documented on [`Self::load`].
    pub fn load_with_file(
        repository: &Repository,
        objects: &Objects,
        file: &Path,
    ) -> Result<Self, Error> {
        Self::load_sources(repository, objects, Some(file))
    }

    fn load_sources(
        repository: &Repository,
        objects: &Objects,
        file_override: Option<&Path>,
    ) -> Result<Self, Error> {
        let mut map = Self::default();
        if let Some(worktree) = repository.worktree() {
            map.read_file(&worktree.join(".mailmap"))?;
        }
        let blob = repository
            .config()
            .string("mailmap", None, "blob")
            .or_else(|| repository.is_bare().then_some(b"HEAD:.mailmap"));
        if let Some(blob) = blob.filter(|b| !b.is_empty()) {
            let text = std::str::from_utf8(blob).map_err(|_| Error::Configuration)?;
            let revision = text.parse().map_err(|e| Error::Object(Box::new(e)))?;
            let resolver = crate::revision::Resolver::new(repository, objects)
                .map_err(|e| Error::Object(Box::new(e)))?;
            match resolver.resolve(&revision) {
                Ok(id) => {
                    let bytes = objects
                        .read_blob(id)
                        .map_err(|e| Error::Object(Box::new(e)))?;
                    map.extend(&bytes);
                }
                Err(
                    crate::revision::RevisionError::NotFound(_)
                    | crate::revision::RevisionError::NoPath { .. },
                ) => {}
                Err(error) => return Err(Error::Object(Box::new(error))),
            }
        }
        if let Some(file) = file_override {
            map.read_file(file)?;
        } else if let Some(file) = repository
            .config()
            .string("mailmap", None, "file")
            .filter(|b| !b.is_empty())
        {
            if file.starts_with(b"~") {
                return Err(Error::Configuration);
            }
            let path = path(file)?;
            let base = repository.worktree().unwrap_or(repository.git_dir());
            map.read_file(&base.join(path))?;
        }
        Ok(map)
    }

    fn read_file(&mut self, path: &Path) -> Result<(), Error> {
        match std::fs::read(path) {
            Ok(bytes) => self.extend(&bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn path(bytes: &[u8]) -> Result<PathBuf, Error> {
    use std::os::unix::ffi::OsStrExt;
    Ok(std::ffi::OsStr::from_bytes(bytes).into())
}
#[cfg(not(unix))]
fn path(bytes: &[u8]) -> Result<PathBuf, Error> {
    Ok(std::str::from_utf8(bytes)
        .map_err(|_| Error::Configuration)?
        .into())
}
