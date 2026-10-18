use std::io;
use std::path::PathBuf;

use crate::Repository;

/// An exclusive lock file shared with `git maintenance run`.
///
/// Dropping the guard removes its owned `objects/maintenance.lock` file on a best-effort basis.
/// Use [`Self::release`] when cleanup errors must be reported. A crash can leave a stale file;
/// inspect ownership before removing it manually. Existing files are never stolen.
/// This coordinates maintenance runners only. It does not exclude ordinary Git writers, readers,
/// or alternate-store dependents and cannot replace [`crate::retention::MaintenanceIsolation`].
/// Callers must exclude hostile replacement of the lock path and its parent directory while held.
/// The lock participates in [`crate::locks::cleanup`]; girt installs no signal handlers.
#[must_use = "dropping the guard releases the maintenance lock"]
pub struct MaintenanceLock {
    path: PathBuf,
    file: Option<crate::locks::SharedFile>,
    registration: crate::locks::Guard,
}

impl std::fmt::Debug for MaintenanceLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaintenanceLock")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl MaintenanceLock {
    /// Removes the owned lock file, reporting cleanup failures.
    ///
    /// # Errors
    ///
    /// Returns [`LockError::Io`] if caller cleanup invalidated the guard, the path changed, or
    /// removal failed. A replaced path is preserved. A failed removal can leave a stale lock.
    pub fn release(mut self) -> Result<(), LockError> {
        let _operation = crate::locks::operation();
        self.registration.check_path().map_err(LockError::Io)?;
        drop(self.file.take());
        std::fs::remove_file(&self.path).map_err(LockError::Io)
    }
}

impl Drop for MaintenanceLock {
    fn drop(&mut self) {
        let _operation = crate::locks::operation();
        if self.file.is_some() && self.registration.check_path().is_ok() {
            drop(self.file.take());
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Why a maintenance runner could not acquire its coordination lock.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LockError {
    /// Another cooperating process holds the lock. An automatic caller can report a skipped run.
    #[error("another process is running maintenance")]
    Busy,
    /// Creating, inspecting or releasing the lock failed.
    #[error("cannot lock repository maintenance")]
    Io(#[source] io::Error),
}

impl Repository {
    /// Tries once to create Git's exclusive `objects/maintenance.lock` file.
    ///
    /// This never waits, steals an existing lock or launches work. Any existing file means
    /// contention, including a stale file left by a crashed process. Hold the returned guard
    /// through the complete maintenance batch, including failure handling. The lock uses the
    /// primary object directory, which linked worktrees share.
    ///
    /// `gc.pid` is separate coordination for `git gc`, and this method does not acquire it. Direct
    /// gc and destructive work still require their own coordination and caller-owned isolation.
    ///
    /// # Errors
    ///
    /// Returns [`LockError::Busy`] for contention, or [`LockError::Io`] for filesystem/OS failures.
    /// Identity-capture failure can leave a file requiring manual inspection; it is not removed
    /// without evidence of ownership.
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let guard = repo.try_lock_maintenance()?;
    /// assert!(matches!(
    ///     repo.try_lock_maintenance(),
    ///     Err(girt::maintenance::LockError::Busy)
    /// ));
    /// drop(guard);
    /// let next_batch = repo.try_lock_maintenance()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn try_lock_maintenance(&self) -> Result<MaintenanceLock, LockError> {
        let _operation = crate::locks::operation();
        let path = self.object_dir().join("maintenance.lock");
        let file = crate::locks::unreplaceable_lock_options()
            .open(&path)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    LockError::Busy
                } else {
                    LockError::Io(error)
                }
            })?;
        let file = crate::locks::SharedFile::from(file);
        let identity = crate::locks::capture_identity(&path, &file).map_err(LockError::Io)?;
        let registration = crate::locks::Guard::new(&path, &file, identity);
        Ok(MaintenanceLock {
            path,
            file: Some(file),
            registration,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Observe Git's task regions rather than its exit status: an automatic lock skip succeeds.
    #[test]
    fn git_skips_task_while_girt_holds_lock() {
        let directory = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            crate::ObjectFormat::Sha1,
            directory.path().join("r"),
            crate::InitKind::Bare,
        )
        .unwrap();
        let trace = directory.path().join("trace.json");
        let mut guard = Some(repo.try_lock_maintenance().unwrap());
        for held in [true, false] {
            let mut command = std::process::Command::new("git");
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("GIT_") {
                    command.env_remove(key);
                }
            }
            let output = command
                .current_dir(repo.git_dir())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", directory.path().join("absent-config"))
                .env("GIT_AUTHOR_NAME", "Maintenance Fixture")
                .env("GIT_AUTHOR_EMAIL", "maintenance@example.com")
                .env("GIT_COMMITTER_NAME", "Maintenance Fixture")
                .env("GIT_COMMITTER_EMAIL", "maintenance@example.com")
                .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
                .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
                .env("GIT_TRACE2_EVENT", &trace)
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "maintenance",
                    "run",
                    "--task=commit-graph",
                ])
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let events = std::fs::read_to_string(&trace).unwrap();
            assert_eq!(
                events.contains("\"label\":\"commit-graph\""),
                !held,
                "{events}"
            );
            std::fs::remove_file(&trace).unwrap();
            // The second run must be free to enter the task after guard cleanup.
            if held {
                drop(guard.take());
            }
        }
    }

    #[test]
    fn existing_file_refuses_and_owned_file_is_removed() {
        let directory = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            crate::ObjectFormat::Sha1,
            directory.path().join("r"),
            crate::InitKind::Bare,
        )
        .unwrap();
        let path = repo.object_dir().join("maintenance.lock");
        std::fs::write(&path, b"another owner").unwrap();
        assert!(matches!(repo.try_lock_maintenance(), Err(LockError::Busy)));
        assert_eq!(std::fs::read(&path).unwrap(), b"another owner");
        std::fs::remove_file(&path).unwrap();
        let guard = repo.try_lock_maintenance().unwrap();
        assert!(matches!(repo.try_lock_maintenance(), Err(LockError::Busy)));
        drop(guard);
        assert!(!path.exists());
        repo.try_lock_maintenance().unwrap().release().unwrap();
        assert!(!path.exists());
    }
}
