use std::io::Read;
use std::path::PathBuf;

use super::Modules;
use crate::{EntryMode, ObjectId, Objects, Repository, index};

/// Which repository snapshot supplies `.gitmodules` bytes.
///
/// Selection is explicit: only [`Self::WorktreeOrIndex`] falls back, and never to HEAD.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Source {
    /// Read the worktree file only; absence returns `None`.
    Worktree,
    /// Read the stage-zero index blob only; unresolved conflict stages are refused.
    Index,
    /// Read the root `.gitmodules` blob in this commit, including in a bare repository.
    Commit(ObjectId),
    /// Read the worktree file, or its index blob if the file is absent, as Git init does.
    WorktreeOrIndex,
}

impl Modules {
    /// Reads definitions from an explicitly selected snapshot with a 1 MiB source-byte limit.
    ///
    /// Equivalent to [`Self::read_with`] with `max_bytes = 1024 * 1024`.
    ///
    /// # Errors
    ///
    /// Returns the source, object, syntax and interpretation failures of [`Self::read_with`].
    ///
    /// ```
    /// use girt::submodule::{Modules, Source};
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("parent"),
    ///     InitKind::Worktree,
    /// )?;
    /// std::fs::write(
    ///     repo.worktree().unwrap().join(".gitmodules"),
    ///     b"[submodule \"math\"]\npath = vendor/math\nurl = ../math.git\n",
    /// )?;
    /// let objects = repo.objects()?;
    /// let modules = Modules::read(&repo, &objects, Source::Worktree)?.ok_or("missing metadata")?;
    /// assert_eq!(
    ///     modules.get(b"math").unwrap().path(),
    ///     Some(b"vendor/math".as_slice())
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read(
        repository: &Repository,
        objects: &Objects,
        source: Source,
    ) -> Result<Option<Self>, LoadError> {
        Self::read_with(repository, objects, source, 1024 * 1024)
    }

    /// Reads definitions without changing configuration, objects or the worktree.
    ///
    /// Missing selected files/entries return `None`. Worktree reads refuse symlinks and non-files;
    /// this deliberately exceeds Git init's symlink handling. `WorktreeOrIndex` falls back only on
    /// absence, never on a rejected or unreadable worktree source. Commit/index entries must be
    /// regular or executable blobs. Conflicted index metadata is refused rather than selecting a
    /// side. Includes remain inert; definition names/paths remain unvalidated until `validate`.
    ///
    /// `max_bytes` bounds file/blob payloads before parsing. Index and commit/tree reads retain
    /// their storage APIs' default limits. Oversized worktree files return [`LoadError::Limit`];
    /// oversized blobs retain the typed object-read limit error inside [`LoadError::Blob`].
    /// Reads are synchronous and not an atomic snapshot.
    /// On macOS/Linux, the worktree leaf is opened with no-follow/nonblocking flags. Other hosts
    /// check metadata before opening and require exclusion of concurrent path replacement. The
    /// repository root and metadata retain [`Repository::open`]'s trust assumptions.
    ///
    /// # Errors
    ///
    /// Reports bare worktree requests, symlinks/non-files, conflicting or non-blob metadata,
    /// exceeded byte limits, corrupt/missing referenced objects, I/O and parsing failures.
    pub fn read_with(
        repository: &Repository,
        objects: &Objects,
        source: Source,
        max_bytes: usize,
    ) -> Result<Option<Self>, LoadError> {
        let bytes = match source {
            Source::Worktree => read_worktree(repository, max_bytes)?,
            Source::Index => read_index(repository, objects, max_bytes)?,
            Source::Commit(id) => read_commit(objects, id, max_bytes)?,
            Source::WorktreeOrIndex => match read_worktree(repository, max_bytes)? {
                Some(bytes) => Some(bytes),
                None => read_index(repository, objects, max_bytes)?,
            },
        };
        bytes
            .map(|bytes| Self::parse(&bytes).map_err(LoadError::Parse))
            .transpose()
    }
}

fn read_worktree(repository: &Repository, max_bytes: usize) -> Result<Option<Vec<u8>>, LoadError> {
    let root = repository.worktree().ok_or(LoadError::NoWorktree)?;
    let path = root.join(".gitmodules");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() => return Err(LoadError::NotRegular),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(LoadError::Io { path, source }),
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
        options.custom_flags(flags.bits().cast_signed());
    }
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(LoadError::Io { path, source }),
    };
    if !file
        .metadata()
        .map_err(|source| LoadError::Io {
            path: path.clone(),
            source,
        })?
        .is_file()
    {
        return Err(LoadError::NotRegular);
    }
    let mut bytes = Vec::new();
    file.take(
        u64::try_from(max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
    )
    .read_to_end(&mut bytes)
    .map_err(|source| LoadError::Io { path, source })?;
    if bytes.len() > max_bytes {
        return Err(LoadError::Limit);
    }
    Ok(Some(bytes))
}

fn read_index(
    repository: &Repository,
    objects: &Objects,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, LoadError> {
    let Some(index) = repository.read_index(index::Limits::default())? else {
        return Ok(None);
    };
    let mut entries = index
        .entries()
        .iter()
        .filter(|entry| entry.path == b".gitmodules");
    let Some(entry) = entries.next() else {
        return Ok(None);
    };
    if entry.stage != index::Stage::Normal {
        return Err(LoadError::Conflict);
    }
    if !matches!(entry.mode, index::Mode::Regular | index::Mode::Executable) {
        return Err(LoadError::NotRegular);
    }
    read_blob(objects, entry.id, max_bytes).map(Some)
}

fn read_commit(
    objects: &Objects,
    id: ObjectId,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, LoadError> {
    let commit = objects.read_commit(id)?;
    let tree = objects.read_tree(commit.tree())?;
    let Some(entry) = tree
        .entries()
        .iter()
        .find(|entry| entry.name == b".gitmodules")
    else {
        return Ok(None);
    };
    if !matches!(entry.mode, EntryMode::Blob | EntryMode::Executable) {
        return Err(LoadError::NotRegular);
    }
    read_blob(objects, entry.id, max_bytes).map(Some)
}

fn read_blob(objects: &Objects, id: ObjectId, max_bytes: usize) -> Result<Vec<u8>, LoadError> {
    let limits = crate::ReadLimits::default().with_max_object_bytes(max_bytes);
    Ok(objects.read_blob_with(id, limits)?)
}

/// A selected `.gitmodules` snapshot could not be read; no mutation occurred.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// Worktree selection requires a non-bare repository.
    #[error("submodule source requires a worktree")]
    NoWorktree,
    /// A filesystem or snapshot entry is a symlink, directory or other non-regular file.
    #[error(".gitmodules must be a regular file")]
    NotRegular,
    /// The index has unresolved `.gitmodules` stages.
    #[error(".gitmodules has unresolved index conflicts")]
    Conflict,
    /// The worktree file exceeded the caller's byte limit.
    #[error(".gitmodules exceeds the source byte limit")]
    Limit,
    /// Reading the selected worktree path failed.
    #[error("reading {path}: {source}")]
    Io {
        /// Selected worktree metadata path.
        path: PathBuf,
        /// Underlying filesystem failure.
        source: std::io::Error,
    },
    /// The index could not be read.
    #[error(transparent)]
    Index(#[from] index::StorageError),
    /// The selected commit could not be read.
    #[error(transparent)]
    Commit(#[from] crate::ReadCommitError),
    /// The selected root tree could not be read.
    #[error(transparent)]
    Tree(#[from] crate::ReadTreeError),
    /// A referenced blob is absent, corrupt, wrong-kind or exceeds object-read limits.
    #[error(transparent)]
    Blob(#[from] crate::ReadBlobError),
    /// Source bytes are malformed or have invalid settings.
    #[error(transparent)]
    Parse(#[from] super::Error),
}
