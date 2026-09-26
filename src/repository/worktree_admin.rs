//! Conservative repair and pruning of linked-worktree registrations.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use thiserror::Error;

use super::worktree_create::{gitfile_line, path_line, relative_path, validate_path};
use super::{Repository, metadata_path};

/// Outcome of an administration operation, including a recoverable partial mutation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WorktreeAdminError {
    /// The registration, checkout, or its link cannot be safely classified.
    #[error("worktree state is uncertain at {0}")]
    Uncertain(PathBuf),
    /// A live checkout, lock, or nonexpired registration prevents pruning.
    #[error("worktree registration is protected at {0}")]
    Protected(PathBuf),
    /// A concurrent administrator holds this registration.
    #[error("worktree administration is busy at {0}")]
    Busy(PathBuf),
    /// Path cannot be represented in Git's line-based metadata.
    #[error("unsupported worktree metadata path: {0}")]
    Path(PathBuf),
    /// Filesystem failure; written paths, if any, need inspection before retry.
    #[error("worktree administration failed at {path}: {source}")]
    Io {
        /// Failed filesystem path.
        path: PathBuf,
        /// Metadata files successfully replaced before failure.
        written: Vec<PathBuf>,
        /// Original operating-system failure.
        #[source]
        source: io::Error,
    },
}

struct AdminLock(PathBuf);
impl Drop for AdminLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn lock(registration: &Path) -> Result<AdminLock, WorktreeAdminError> {
    let path = registration.join("girt-admin.lock");
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(_) => Ok(AdminLock(path)),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Err(WorktreeAdminError::Busy(path))
        }
        Err(source) => Err(WorktreeAdminError::Io {
            path,
            written: Vec::new(),
            source,
        }),
    }
}

fn checked_registration(repo: &Repository, path: &Path) -> Result<PathBuf, WorktreeAdminError> {
    let root = repo.common_dir.join("worktrees");
    if path.parent() != Some(root.as_path()) {
        return Err(WorktreeAdminError::Uncertain(path.into()));
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|_| WorktreeAdminError::Uncertain(path.into()))?;
    if !metadata.is_dir()
        || fs::canonicalize(path).map_err(|_| WorktreeAdminError::Uncertain(path.into()))? != path
    {
        return Err(WorktreeAdminError::Uncertain(path.into()));
    }
    Ok(path.into())
}

fn read_link(path: &Path, prefix: &[u8]) -> Result<PathBuf, WorktreeAdminError> {
    let bytes = fs::read(path).map_err(|_| WorktreeAdminError::Uncertain(path.into()))?;
    let bytes = bytes
        .strip_prefix(prefix)
        .ok_or_else(|| WorktreeAdminError::Uncertain(path.into()))?;
    metadata_path(path, bytes).map_err(|_| WorktreeAdminError::Uncertain(path.into()))
}

fn resolved(source: &Path, target: &Path) -> PathBuf {
    source.parent().unwrap_or(Path::new(".")).join(target)
}

fn replace(
    path: &Path,
    bytes: &[u8],
    written: &mut Vec<PathBuf>,
) -> Result<(), WorktreeAdminError> {
    let temp = path.with_extension("girt-repair-temp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|source| WorktreeAdminError::Io {
            path: temp.clone(),
            written: written.clone(),
            source,
        })?;
    file.write_all(bytes)
        .map_err(|source| WorktreeAdminError::Io {
            path: temp.clone(),
            written: written.clone(),
            source,
        })?;
    file.sync_all().map_err(|source| WorktreeAdminError::Io {
        path: temp.clone(),
        written: written.clone(),
        source,
    })?;
    fs::rename(&temp, path).map_err(|source| WorktreeAdminError::Io {
        path: path.into(),
        written: written.clone(),
        source,
    })?;
    written.push(path.into());
    Ok(())
}

impl Repository {
    /// Repairs a registration's links to an explicitly identified existing checkout.
    ///
    /// The caller must exclude checkout relocation and noncooperating administrators. This method
    /// never changes checkout files or jj workspace metadata. A failed replacement reports paths
    /// already written; inspect the registration and checkout before retrying. The checkout's
    /// existing `.git` must point to this registration or to an absent old location.
    ///
    /// # Errors
    ///
    /// Refuses ambiguous or inaccessible metadata before mutation. Later I/O errors report partial
    /// writes; a temporary repair file may remain for manual recovery.
    pub fn repair_worktree(
        &self,
        registration: impl AsRef<Path>,
        checkout: impl AsRef<Path>,
    ) -> Result<(), WorktreeAdminError> {
        let registration = checked_registration(self, registration.as_ref())?;
        let checkout = fs::canonicalize(checkout.as_ref())
            .map_err(|_| WorktreeAdminError::Uncertain(checkout.as_ref().into()))?;
        if !checkout.is_dir() {
            return Err(WorktreeAdminError::Uncertain(checkout));
        }
        let gitfile = checkout.join(".git");
        let _guard = lock(&registration)?;
        let old_forward = read_link(&gitfile, b"gitdir: ")?;
        let old_target = resolved(&gitfile, &old_forward);
        if old_target.file_name() != registration.file_name()
            || old_target.parent().and_then(Path::file_name)
                != Some(std::ffi::OsStr::new("worktrees"))
        {
            return Err(WorktreeAdminError::Protected(gitfile));
        }
        match fs::symlink_metadata(&old_target) {
            Ok(_) => {
                let old_canonical = fs::canonicalize(&old_target)
                    .map_err(|_| WorktreeAdminError::Uncertain(old_target.clone()))?;
                if old_canonical != registration {
                    return Err(WorktreeAdminError::Protected(gitfile));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(_) => return Err(WorktreeAdminError::Uncertain(old_target)),
        }
        let backlink = registration.join("gitdir");
        let _ = read_link(&backlink, b"")?;
        let commondir = registration.join("commondir");
        let _ = read_link(&commondir, b"")?;
        let relative = self.common_dir.join("config");
        let config =
            fs::read(&relative).map_err(|_| WorktreeAdminError::Uncertain(relative.clone()))?;
        let config = crate::Config::parse(&config)
            .map_err(|_| WorktreeAdminError::Uncertain(relative.clone()))?;
        let use_relative = super::extension_boolean(&config, &relative, "relativeworktrees")
            .map_err(|_| WorktreeAdminError::Uncertain(relative))?;
        let forward = if use_relative {
            relative_path(&checkout, &registration)
        } else {
            registration.clone()
        };
        let back = if use_relative {
            relative_path(&registration, &gitfile)
        } else {
            gitfile.clone()
        };
        validate_path(&forward).map_err(|_| WorktreeAdminError::Path(forward.clone()))?;
        validate_path(&back).map_err(|_| WorktreeAdminError::Path(back.clone()))?;
        let mut written = Vec::new();
        replace(&commondir, b"../..\n", &mut written)?;
        replace(&gitfile, &gitfile_line(&forward), &mut written)?;
        replace(&backlink, &path_line(&back), &mut written)?;
        Ok(())
    }

    /// Protects a registration from pruning with Git-compatible `locked` metadata.
    ///
    /// # Errors
    ///
    /// Existing locks are preserved; I/O failures retain their source.
    pub fn lock_worktree(
        &self,
        registration: impl AsRef<Path>,
        reason: &str,
    ) -> Result<(), WorktreeAdminError> {
        let registration = checked_registration(self, registration.as_ref())?;
        if reason.contains(['\n', '\r']) {
            return Err(WorktreeAdminError::Uncertain(registration));
        }
        let _guard = lock(&registration)?;
        let path = registration.join("locked");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| WorktreeAdminError::Io {
                path: path.clone(),
                written: Vec::new(),
                source,
            })?;
        file.write_all(reason.as_bytes())
            .map_err(|source| WorktreeAdminError::Io {
                path,
                written: Vec::new(),
                source,
            })
    }

    /// Removes a lock only when its exact reason matches the caller's observation.
    ///
    /// # Errors
    ///
    /// Refuses changed or unreadable locks. Other administrators must cooperate with this API.
    pub fn unlock_worktree(
        &self,
        registration: impl AsRef<Path>,
        reason: &str,
    ) -> Result<(), WorktreeAdminError> {
        let registration = checked_registration(self, registration.as_ref())?;
        let _guard = lock(&registration)?;
        let path = registration.join("locked");
        if fs::read(&path).map_err(|_| WorktreeAdminError::Uncertain(path.clone()))?
            != reason.as_bytes()
        {
            return Err(WorktreeAdminError::Protected(path));
        }
        fs::remove_file(&path).map_err(|source| WorktreeAdminError::Io {
            path,
            written: Vec::new(),
            source,
        })
    }

    /// Prunes one missing registration only when its gitdir link predates `expire_before`.
    ///
    /// A live or locked registration is never removed. Inaccessible targets are uncertain and
    /// require caller inspection. The caller must exclude noncooperating moves and Git maintenance.
    ///
    /// # Errors
    ///
    /// Refusals have no effects. Removal may fail partially; retained files need inspection.
    pub fn prune_worktree(
        &self,
        registration: impl AsRef<Path>,
        expire_before: SystemTime,
    ) -> Result<(), WorktreeAdminError> {
        let registration = checked_registration(self, registration.as_ref())?;
        let _guard = lock(&registration)?;
        let locked = registration.join("locked");
        match fs::symlink_metadata(&locked) {
            Ok(_) => return Err(WorktreeAdminError::Protected(locked)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(_) => return Err(WorktreeAdminError::Uncertain(locked)),
        }
        let gitdir = registration.join("gitdir");
        let target = resolved(&gitdir, &read_link(&gitdir, b"")?);
        let age = fs::metadata(&gitdir)
            .map_err(|_| WorktreeAdminError::Uncertain(gitdir.clone()))?
            .modified()
            .map_err(|_| WorktreeAdminError::Uncertain(gitdir.clone()))?;
        if age >= expire_before {
            return Err(WorktreeAdminError::Protected(registration));
        }
        match fs::symlink_metadata(&target) {
            Ok(_) => return Err(WorktreeAdminError::Protected(target)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(_) => return Err(WorktreeAdminError::Uncertain(target)),
        }
        fs::remove_dir_all(&registration).map_err(|source| WorktreeAdminError::Io {
            path: registration,
            written: Vec::new(),
            source,
        })
    }
}
