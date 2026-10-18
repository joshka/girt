//! Reading the rule files Git consults for a repository: ignore and attribute files.
//!
//! In-tree files are read without following symlinks, component by component where the platform
//! allows; configured files and files in the Git directory follow symlinks, as Git does.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// A rule file could not be read; each consumer maps this to its own error.
#[derive(Debug)]
pub(crate) enum FileError {
    /// The operating system refused a read.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file exceeds the remaining byte budget.
    TooLarge { path: PathBuf },
    /// A configured file is not a regular file, such as a FIFO, which is never waited on.
    NotFile,
}

/// Converts path bytes to a native path; `None` for non-Unicode bytes where paths are Unicode.
pub(crate) fn os_path(bytes: &[u8]) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes).ok().map(PathBuf::from)
    }
}

/// Converts a validated slash-separated directory to a native path below `root`.
pub(crate) fn native(root: &Path, directory: &[u8]) -> Option<PathBuf> {
    let mut path = root.to_owned();
    if directory.is_empty() {
        return Some(path);
    }
    for component in directory.split(|byte| *byte == b'/') {
        if component.is_empty() || component == b"." || component == b".." {
            return None;
        }
        path.push(os_path(component)?);
    }
    Some(path)
}

fn io(path: &Path, source: impl Into<std::io::Error>) -> FileError {
    FileError::Io {
        path: path.to_owned(),
        source: source.into(),
    }
}

/// Reads at most `remaining` bytes, so an oversized source fails before it is buffered.
fn read_bounded(
    file: impl std::io::Read,
    path: &Path,
    remaining: usize,
) -> Result<Vec<u8>, FileError> {
    use std::io::Read as _;
    let mut content = Vec::new();
    let limit = u64::try_from(remaining)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    file.take(limit)
        .read_to_end(&mut content)
        .map_err(|source| io(path, source))?;
    if content.len() > remaining {
        return Err(FileError::TooLarge {
            path: path.to_owned(),
        });
    }
    Ok(content)
}

/// Reads a configured file or one in the Git directory, following symlinks as Git does.
///
/// The file is opened without blocking, so a FIFO is refused rather than waited on.
pub(crate) fn read_optional(path: &Path, remaining: usize) -> Result<Option<Vec<u8>>, FileError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits().cast_signed());
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io(path, source)),
    };
    if !file
        .metadata()
        .map_err(|source| io(path, source))?
        .is_file()
    {
        return Err(FileError::NotFile);
    }
    read_bounded(file, path, remaining).map(Some)
}

/// Worktree directories opened component by component without following symlinks.
///
/// [`Self::enter`] reuses the directories a previous call opened for a common prefix, so a value
/// kept across sorted queries opens each directory about once. A reused directory is the one
/// opened earlier even if its path has since been replaced, so nothing is read through a symlink.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug)]
pub(crate) struct Ancestors {
    /// Open directories from the root down; `names[i]` was opened as `stack[i + 1]`.
    stack: Vec<rustix::fd::OwnedFd>,
    names: Vec<Vec<u8>>,
    /// The last [`Self::enter`] failed, so [`Self::read`] finds nothing.
    blocked: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Ancestors {
    pub(crate) fn open(root: &Path) -> Result<Self, FileError> {
        let directory = crate::status::worktree::open_root(root).map_err(|_| {
            io(
                root,
                std::io::Error::other("cannot open the working tree without following symlinks"),
            )
        })?;
        Ok(Self {
            stack: vec![directory],
            names: Vec::new(),
            blocked: false,
        })
    }

    /// Opens `directory` below the root; false when a component is missing, a symlink or not a
    /// directory, so no file at or below it applies.
    pub(crate) fn enter(&mut self, root: &Path, directory: &[u8]) -> Result<bool, FileError> {
        use std::os::unix::ffi::OsStrExt;
        let components: Vec<&[u8]> = directory
            .split(|byte| *byte == b'/')
            .filter(|c| !c.is_empty())
            .collect();
        self.blocked = true;
        if components
            .iter()
            .any(|name| *name == b"." || *name == b"..")
        {
            return Ok(false);
        }
        let common = self
            .names
            .iter()
            .zip(&components)
            .take_while(|(open, wanted)| open.as_slice() == **wanted)
            .count();
        self.names.truncate(common);
        self.stack.truncate(common + 1);
        for name in components.get(common..).unwrap_or_default() {
            let Some(current) = self.stack.last() else {
                return Ok(false);
            };
            match rustix::fs::openat(
                current,
                std::ffi::OsStr::from_bytes(name),
                crate::status::worktree::directory_flags(),
                rustix::fs::Mode::empty(),
            ) {
                Ok(next) => {
                    self.stack.push(next);
                    self.names.push(name.to_vec());
                }
                Err(
                    rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP,
                ) => return Ok(false),
                Err(source) => {
                    return Err(io(&native(root, directory).unwrap_or_default(), source));
                }
            }
        }
        self.blocked = false;
        Ok(true)
    }

    /// Reads `name` in the entered directory, skipping symlinks and non-files as Git does.
    pub(crate) fn read(
        &self,
        name: &str,
        path: &Path,
        remaining: usize,
    ) -> Result<Option<Vec<u8>>, FileError> {
        use rustix::fs::{FileType, Mode, OFlags};
        let Some(directory) = self.stack.last().filter(|_| !self.blocked) else {
            return Ok(None);
        };
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let file = match rustix::fs::openat(directory, name, flags, Mode::empty()) {
            Ok(file) => file,
            Err(rustix::io::Errno::NOENT | rustix::io::Errno::LOOP) => return Ok(None),
            Err(source) => return Err(io(path, source)),
        };
        let stat = rustix::fs::fstat(&file).map_err(|source| io(path, source))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Ok(None);
        }
        read_bounded(std::fs::File::from(file), path, remaining).map(Some)
    }
}

/// Path-based fallback where descriptor-relative traversal is unavailable: every call rechecks
/// each component with `symlink_metadata`, which cannot exclude concurrent substitution. Holding no
/// descriptors, it reuses nothing from earlier calls.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[derive(Debug)]
pub(crate) struct Ancestors {
    blocked: bool,
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Ancestors {
    pub(crate) fn open(_root: &Path) -> Result<Self, FileError> {
        Ok(Self { blocked: false })
    }

    pub(crate) fn enter(&mut self, root: &Path, directory: &[u8]) -> Result<bool, FileError> {
        let components: Vec<&[u8]> = directory
            .split(|byte| *byte == b'/')
            .filter(|c| !c.is_empty())
            .collect();
        self.blocked = !(1..=components.len()).all(|end| {
            let prefix = components.get(..end).unwrap_or_default().join(&b'/');
            native(root, &prefix)
                .and_then(|folder| std::fs::symlink_metadata(folder).ok())
                .is_some_and(|metadata| metadata.is_dir())
        });
        Ok(!self.blocked)
    }

    pub(crate) fn read(
        &self,
        _name: &str,
        path: &Path,
        remaining: usize,
    ) -> Result<Option<Vec<u8>>, FileError> {
        if self.blocked {
            return Ok(None);
        }
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => {
                let file = std::fs::File::open(path).map_err(|source| io(path, source))?;
                read_bounded(file, path, remaining).map(Some)
            }
            Ok(_) => Ok(None),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(source) => Err(io(path, source)),
        }
    }
}
