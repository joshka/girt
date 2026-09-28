//! Prepared repair of one exactly selected linked checkout without opening repository storage.
use std::fs;
use std::path::{Path, PathBuf};

use super::worktree_admin::{checked_registration, lock, replace};
use super::worktree_create::{gitfile_line, path_line, relative_path};
use super::{RepositoryLocation, WorktreeAdminError, metadata_path};

/// Captured links for moving one linked checkout's gitfile to another directory.
///
/// Created by [`RepositoryLocation::prepare_worktree_repair`]. This handle does not hold a lock
/// between preparation and repair. The caller must exclude checkout relocation and noncooperating
/// administrators throughout that interval and repair. No configuration, HEAD, index, references,
/// object storage or shallow metadata is read. Private and user data is never changed.
#[derive(Debug)]
pub struct WorktreeRepair {
    registration: PathBuf,
    registration_identity: fs::Metadata,
    common: PathBuf,
    common_identity: fs::Metadata,
    forward: CapturedLink,
    back: CapturedLink,
    common_link: CapturedLink,
}

#[derive(Debug)]
struct CapturedLink {
    path: PathBuf,
    bytes: Vec<u8>,
    target: PathBuf,
    identity: fs::Metadata,
}

impl CapturedLink {
    fn read(path: PathBuf, prefix: &[u8]) -> Result<Self, WorktreeAdminError> {
        let identity = regular_file(&path)?;
        let bytes = fs::read(&path).map_err(|_| uncertain(&path))?;
        let value = bytes.strip_prefix(prefix).ok_or_else(|| uncertain(&path))?;
        let target = metadata_path(&path, value).map_err(|_| uncertain(&path))?;
        check_identity(&path, &identity, &regular_file(&path)?)?;
        Ok(Self {
            path,
            bytes,
            target,
            identity,
        })
    }

    fn check_at(&self, path: &Path) -> Result<(), WorktreeAdminError> {
        check_identity(path, &self.identity, &regular_file(path)?)?;
        if fs::read(path).map_err(|_| uncertain(path))? != self.bytes {
            return Err(uncertain(path));
        }
        Ok(())
    }

    fn resolved(&self) -> Result<PathBuf, WorktreeAdminError> {
        let parent = self.path.parent().ok_or_else(|| uncertain(&self.path))?;
        fs::canonicalize(parent.join(&self.target)).map_err(|_| uncertain(&self.path))
    }
}

impl RepositoryLocation {
    /// Captures an exact linked checkout's links for later metadata-only repair.
    ///
    /// Select this location with [`Self::at_git_dir`] on the checkout's `.git` file. The gitfile,
    /// registration backlink and common-directory link must agree. Preparation reads only these
    /// links and filesystem identities. It acquires and releases the registration's administration
    /// lock, but publishes no link changes. Keep the source gitfile until repair finishes.
    ///
    /// # Errors
    ///
    /// Refuses non-linked locations, symlink metadata leaves, malformed or inconsistent links and
    /// a busy registration. Directory aliases are resolved; unrelated repository state is ignored.
    ///
    /// ```no_run
    /// use girt::RepositoryLocation;
    /// let location = RepositoryLocation::at_git_dir("temporary/.git")?;
    /// let repair = location.prepare_worktree_repair()?;
    /// std::fs::hard_link("temporary/.git", "destination/.git")?;
    /// repair.repair("destination")?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn prepare_worktree_repair(&self) -> Result<WorktreeRepair, WorktreeAdminError> {
        let source = self
            .inferred_worktree
            .as_ref()
            .ok_or_else(|| uncertain(&self.git_dir))?;
        let registration = checked_registration(&self.common_dir, &self.git_dir)?;
        let _guard = lock(&registration)?;
        let registration_identity = directory(&registration)?;
        let common_identity = directory(&self.common_dir)?;
        let forward = CapturedLink::read(source.join(".git"), b"gitdir: ")?;
        directory(&source.join(&forward.target))?;
        let back = CapturedLink::read(registration.join("gitdir"), b"")?;
        let common_link = CapturedLink::read(registration.join("commondir"), b"")?;
        if forward.resolved()? != registration
            || back.resolved()? != forward.path
            || common_link.resolved()? != self.common_dir
        {
            return Err(uncertain(&registration));
        }
        Ok(WorktreeRepair {
            registration,
            registration_identity,
            common: self.common_dir.clone(),
            common_identity,
            forward,
            back,
            common_link,
        })
    }
}

impl WorktreeRepair {
    /// Repairs the published gitfile and registration backlink to an existing destination.
    ///
    /// The destination `.git` must contain the captured source gitfile's bytes; on Unix it must
    /// also be the same file (for example, published with `hard_link`). Source and metadata links
    /// are revalidated under the administration lock before replacement. Unix identity checks use
    /// device and inode; other platforms check file kind and bytes. These checks do not replace the
    /// caller's exclusion of noncooperating replacements. Directory symlink aliases are accepted,
    /// but gitfile and registration leaves must not be symlinks.
    ///
    /// Each link retains its captured absolute or relative path form independently. The common
    /// link is unchanged, and no repository-format extension is read or enabled. Relative paths
    /// use the existing path helper, which retains an absolute path across different roots.
    ///
    /// # Errors
    ///
    /// Changed identities or bytes, conflicting links, invalid paths and busy registrations fail
    /// before replacement. Later I/O errors report paths already written; a temporary repair file
    /// may remain. Preserve the source checkout and registration for recovery on failure.
    pub fn repair(self, checkout: impl AsRef<Path>) -> Result<(), WorktreeAdminError> {
        let checkout =
            fs::canonicalize(checkout.as_ref()).map_err(|_| uncertain(checkout.as_ref()))?;
        directory(&checkout)?;
        checked_registration(&self.common, &self.registration)?;
        let _guard = lock(&self.registration)?;
        self.check_captured()?;
        let gitfile = checkout.join(".git");
        self.forward.check_at(&gitfile)?;
        let forward = if self.forward.target.is_absolute() {
            self.registration.clone()
        } else {
            relative_path(&checkout, &self.registration)
        };
        let back = if self.back.target.is_absolute() {
            gitfile.clone()
        } else {
            relative_path(&self.registration, &gitfile)
        };
        validate_repair_path(&forward)?;
        validate_repair_path(&back)?;
        let mut written = Vec::new();
        let forward_bytes = gitfile_line(&forward);
        replace(&gitfile, &forward_bytes, &mut written)?;
        // Report the published forward link if revalidation fails before the second replacement.
        let recheck = || -> Result<(), WorktreeAdminError> {
            self.check_captured()?;
            let published = CapturedLink::read(gitfile.clone(), b"gitdir: ")?;
            if published.bytes != forward_bytes || published.resolved()? != self.registration {
                return Err(uncertain(&gitfile));
            }
            Ok(())
        };
        recheck().map_err(|source| WorktreeAdminError::Io {
            path: gitfile.clone(),
            written: written.clone(),
            source: std::io::Error::other(source),
        })?;
        replace(&self.back.path, &path_line(&back), &mut written)?;
        Ok(())
    }
    fn check_captured(&self) -> Result<(), WorktreeAdminError> {
        check_identity(
            &self.registration,
            &self.registration_identity,
            &directory(&self.registration)?,
        )?;
        check_identity(
            &self.common,
            &self.common_identity,
            &directory(&self.common)?,
        )?;
        self.forward.check_at(&self.forward.path)?;
        directory(
            &self
                .forward
                .path
                .parent()
                .unwrap()
                .join(&self.forward.target),
        )?;
        self.back.check_at(&self.back.path)?;
        self.common_link.check_at(&self.common_link.path)?;
        if self.forward.resolved()? != self.registration
            || self.back.resolved()? != self.forward.path
            || self.common_link.resolved()? != self.common
        {
            return Err(uncertain(&self.registration));
        }
        Ok(())
    }
}

fn uncertain(path: &Path) -> WorktreeAdminError {
    WorktreeAdminError::Uncertain(path.into())
}

fn regular_file(path: &Path) -> Result<fs::Metadata, WorktreeAdminError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| uncertain(path))?;
    if !metadata.is_file() {
        return Err(uncertain(path));
    }
    Ok(metadata)
}

fn directory(path: &Path) -> Result<fs::Metadata, WorktreeAdminError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| uncertain(path))?;
    if !metadata.is_dir() {
        return Err(uncertain(path));
    }
    Ok(metadata)
}

fn check_identity(
    path: &Path,
    before: &fs::Metadata,
    after: &fs::Metadata,
) -> Result<(), WorktreeAdminError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(uncertain(path));
        }
    }
    #[cfg(not(unix))]
    let _ = (path, before, after);
    Ok(())
}

// Unlike creation, repair retains Git-created paths with interior CR/LF components.
fn validate_repair_path(path: &Path) -> Result<(), WorktreeAdminError> {
    #[cfg(not(unix))]
    if path.to_str().is_none() {
        return Err(WorktreeAdminError::Path(path.into()));
    }
    let line = path_line(path);
    let bytes = &line[..line.len() - 1];
    if bytes.is_empty() || bytes.contains(&0) || matches!(bytes.last(), Some(b'\r' | b'\n')) {
        return Err(WorktreeAdminError::Path(path.into()));
    }
    Ok(())
}
