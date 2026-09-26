//! Exclusive registration of an unborn linked worktree.
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use super::Repository;
use crate::refs::{Backend, RefName, Target};

/// A refusal or partial creation failure. A registered directory in an error is retained.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CreateWorktreeError {
    /// The branch is not a full heads reference.
    #[error("invalid orphan branch name")]
    Branch,
    /// The requested orphan branch already has a stored value.
    #[error("branch already exists")]
    ExistingBranch,
    /// The destination exists or lacks a usable filename.
    #[error("worktree destination already exists or has no filename: {0}")]
    Destination(PathBuf),
    /// The path cannot be written unambiguously in Git's line-based metadata.
    #[error("unsupported worktree metadata path: {0}")]
    Path(PathBuf),
    /// No free registration name was found within the caller's bound.
    #[error("worktree registration name limit exceeded")]
    Limit,
    /// Reference inspection failed before creation.
    #[error(transparent)]
    References(#[from] crate::refs::ReferenceError),
    /// Existing repository configuration could not be interpreted.
    #[error(transparent)]
    Configuration(#[from] super::OpenError),
    /// Empty index encoding failed before creation.
    #[error(transparent)]
    Index(#[from] crate::index::Error),
    /// Private reftable encoding failed before creation.
    #[error(transparent)]
    Reftable(#[from] crate::refs::reftable::Error),
    /// Filesystem failure; registration identifies retained partial state, if any.
    #[error("cannot create {path}: {source}")]
    Io {
        /// Failed path.
        path: PathBuf,
        /// Reserved registration, if any.
        registration: Option<PathBuf>,
        /// OS cause.
        #[source]
        source: io::Error,
    },
    /// Written metadata could not be reopened.
    #[error("created worktree metadata failed validation: {source}")]
    Open {
        /// Registration retained for inspection.
        registration: PathBuf,
        /// Opening failure.
        #[source]
        source: Box<super::OpenError>,
    },
}

impl Repository {
    /// Registers an empty linked worktree with an unborn branch and private empty index.
    ///
    /// The branch must be a full heads name without a stored value. The destination must be
    /// absent and its parent must exist. A destination filename supplies the registration name;
    /// numeric suffixes resolve collisions within max_names attempts. Multiple worktrees may
    /// point at the same unborn branch, as Git permits. The configured relativeWorktrees
    /// extension selects relative forward and back links. No checkout files, initial commit,
    /// shared branch value, or jj workspace metadata are created.
    ///
    /// The caller must exclude competing branch/worktree administration and path replacement.
    /// Failures may retain a registration or checkout directory for inspection. There is no
    /// automatic rollback, fsync or crash-durability guarantee. Parent symlinks are followed;
    /// this is not a security boundary for untrusted paths.
    ///
    /// # Errors
    ///
    /// Branch and destination refusals precede creation. Exhausted names can leave a newly
    /// created registrations directory. Later errors retain the failed path and reservation.
    pub fn create_orphan_worktree(
        &self,
        destination: impl AsRef<Path>,
        branch: &RefName,
        max_names: usize,
    ) -> Result<Self, CreateWorktreeError> {
        if max_names == 0 {
            return Err(CreateWorktreeError::Limit);
        }
        if !branch.as_bytes().starts_with(b"refs/heads/") {
            return Err(CreateWorktreeError::Branch);
        }
        if self.references()?.read(branch)?.is_some() {
            return Err(CreateWorktreeError::ExistingBranch);
        }
        let destination = destination.as_ref();
        let name = destination
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| CreateWorktreeError::Destination(destination.into()))?;
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = fs::canonicalize(parent).map_err(|e| io_error(parent, None, e))?;
        let destination = parent.join(name);
        validate_path(&destination)?;
        validate_path(self.common_dir())?;
        match fs::symlink_metadata(&destination) {
            Ok(_) => return Err(CreateWorktreeError::Destination(destination)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(io_error(&destination, None, e)),
        }
        let index = crate::index::Index::empty(self.object_format())
            .encode(crate::index::Limits::default())?;
        let table = if self.reference_backend() == Backend::Reftable {
            Some(
                initial_head_table(self.object_format(), branch)
                    .encode(crate::refs::reftable::Limits::default())?,
            )
        } else {
            None
        };
        let config_path = self.common_dir().join("config");
        let bytes =
            super::read_config(&config_path, crate::config::ResolveLimits::default().bytes)?;
        let config = crate::Config::parse(&bytes).map_err(|source| super::OpenError::Config {
            path: config_path.clone(),
            source,
        })?;
        let relative = super::extension_boolean(&config, &config_path, "relativeworktrees")?;
        let registrations = self.common_dir().join("worktrees");
        fs::create_dir_all(&registrations).map_err(|e| io_error(&registrations, None, e))?;
        let registration = reserve_name(&registrations, name, max_names)?;
        (|| {
            let forward = if relative {
                relative_path(&destination, &registration)
            } else {
                registration.clone()
            };
            let backlink = if relative {
                relative_path(&registration, &destination.join(".git"))
            } else {
                destination.join(".git")
            };
            write_file(&registration.join("commondir"), b"../..\n", &registration)?;
            write_file(
                &registration.join("gitdir"),
                &path_line(&backlink),
                &registration,
            )?;
            if let Some(table) = table {
                create_dir(&registration.join("reftable"), &registration)?;
                write_file(
                    &registration.join("reftable/initial.ref"),
                    &table,
                    &registration,
                )?;
                write_file(
                    &registration.join("reftable/tables.list"),
                    b"initial.ref\n",
                    &registration,
                )?;
                create_dir(&registration.join("refs"), &registration)?;
                write_file(
                    &registration.join("refs/heads"),
                    b"this repository uses the reftable format\n",
                    &registration,
                )?;
                write_file(
                    &registration.join("HEAD"),
                    b"ref: refs/heads/.invalid\n",
                    &registration,
                )?;
            } else {
                let mut head = b"ref: ".to_vec();
                head.extend_from_slice(branch.as_bytes());
                head.push(b'\n');
                write_file(&registration.join("HEAD"), &head, &registration)?;
            }
            write_file(&registration.join("index"), &index, &registration)?;
            create_dir(&destination, &registration)?;
            write_file(
                &destination.join(".git"),
                &gitfile_line(&forward),
                &registration,
            )?;
            Self::open(&destination).map_err(|source| CreateWorktreeError::Open {
                registration: registration.clone(),
                source: Box::new(source),
            })
        })()
    }
}

fn reserve_name(
    root: &Path,
    name: &OsStr,
    max_names: usize,
) -> Result<PathBuf, CreateWorktreeError> {
    for suffix in 0..max_names {
        let mut candidate = OsString::from(name);
        if suffix != 0 {
            candidate.push(suffix.to_string());
        }
        let path = root.join(candidate);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(io_error(&path, None, e)),
        }
    }
    Err(CreateWorktreeError::Limit)
}

fn initial_head_table(
    format: crate::ObjectFormat,
    branch: &RefName,
) -> crate::refs::reftable::Table {
    use crate::refs::reftable::{RefRecord, Table};
    Table {
        format,
        min_update_index: 1,
        max_update_index: 1,
        references: vec![RefRecord {
            name: RefName::new(b"HEAD").expect("fixed name").into(),
            update_index: 1,
            target: Some(Target::Symbolic(branch.clone())),
            peeled: None,
        }],
        logs: Vec::new(),
    }
}

pub(super) fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let absolute = to.to_path_buf();
    let from: Vec<Component<'_>> = from.components().collect();
    let to: Vec<Component<'_>> = to.components().collect();
    if from.first() != to.first() {
        return absolute;
    }
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut result = PathBuf::new();
    for _ in shared..from.len() {
        result.push("..");
    }
    for component in &to[shared..] {
        result.push(component.as_os_str());
    }
    result
}

pub(super) fn validate_path(path: &Path) -> Result<(), CreateWorktreeError> {
    #[cfg(unix)]
    let invalid = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().contains(&b'\n') || path.as_os_str().as_bytes().contains(&b'\r')
    };
    #[cfg(not(unix))]
    let invalid = path.to_str().is_none()
        || path.to_string_lossy().as_bytes().contains(&b'\n')
        || path.to_string_lossy().as_bytes().contains(&b'\r');
    if invalid {
        Err(CreateWorktreeError::Path(path.into()))
    } else {
        Ok(())
    }
}

pub(super) fn path_line(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    let mut bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(windows)]
    let mut bytes = {
        let spelling = path.to_str().expect("validated metadata path");
        // Git treats the verbatim prefix returned by Windows canonicalization as nonlocal.
        let spelling = if let Some(unc) = spelling.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            spelling
                .strip_prefix(r"\\?\")
                .unwrap_or(spelling)
                .to_owned()
        };
        spelling.into_bytes()
    };
    #[cfg(not(any(unix, windows)))]
    let mut bytes = path
        .to_str()
        .expect("validated metadata path")
        .as_bytes()
        .to_vec();
    bytes.push(b'\n');
    bytes
}

pub(super) fn gitfile_line(path: &Path) -> Vec<u8> {
    let mut bytes = b"gitdir: ".to_vec();
    bytes.extend_from_slice(&path_line(path));
    bytes
}

fn create_dir(path: &Path, registration: &Path) -> Result<(), CreateWorktreeError> {
    fs::create_dir(path).map_err(|e| io_error(path, Some(registration), e))
}

fn write_file(path: &Path, bytes: &[u8], registration: &Path) -> Result<(), CreateWorktreeError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| io_error(path, Some(registration), e))?;
    file.write_all(bytes)
        .map_err(|e| io_error(path, Some(registration), e))
}

fn io_error(path: &Path, registration: Option<&Path>, source: io::Error) -> CreateWorktreeError {
    CreateWorktreeError::Io {
        path: path.into(),
        registration: registration.map(Path::to_path_buf),
        source,
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn gitfile_paths_drop_windows_verbatim_prefixes() {
        assert_eq!(
            path_line(Path::new(r"\\?\C:\repo\worktrees\one")),
            b"C:\\repo\\worktrees\\one\n"
        );
        assert_eq!(
            path_line(Path::new(r"\\?\UNC\server\share\repo")),
            b"\\\\server\\share\\repo\n"
        );
    }
}
