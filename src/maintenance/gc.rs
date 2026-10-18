use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{LockError, MaintenanceLock};
use crate::Repository;

/// Ownership of `gc.pid`, separate from the maintenance task-runner lock.
///
/// Hold this guard while running explicit gc and retain caller-owned
/// [`crate::retention::MaintenanceIsolation`] for destructive phases. Drop removes only the owned
/// pid file on a best-effort basis; [`Self::release`] reports cleanup failures. A crash can leave
/// a file requiring manual inspection. Girt never steals stale or malformed pid files.
#[derive(Debug)]
#[must_use = "dropping the guard releases the gc pid file"]
pub struct GcLock(MaintenanceLock);

impl GcLock {
    /// Removes the owned pid file, preserving a replaced path.
    ///
    /// # Errors
    ///
    /// Returns ownership-check or cleanup failures from [`MaintenanceLock::release`].
    pub fn release(self) -> Result<(), LockError> {
        self.0.release()
    }
}

/// A previous gc diagnostic that may suppress an automatic detached run.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GcLog {
    /// No nonempty recent log was found, or `gc.autoDetach=false` bypassed the log check.
    NotBlocking,
    /// A nonempty log newer than `gc.logExpiry` exists. Preserve it and report its raw bytes.
    /// The caller should fix its cause before choosing to remove it and retry.
    Suppressed(Vec<u8>),
}

/// Why gc coordination or log observation could not complete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GcCoordinationError {
    /// A pid-file writer or existing gc owner prevented acquisition.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// The supplied host is empty, longer than 255 bytes, non-ASCII or contains whitespace.
    #[error("gc hostname must be a nonempty ASCII name without whitespace (at most 255 bytes)")]
    Hostname,
    /// A pid/log file could not be created, published, read or inspected.
    #[error("cannot coordinate gc")]
    Io(#[from] io::Error),
    /// A consulted gc configuration value is malformed or uses an unsupported date expression.
    #[error("invalid gc coordination configuration")]
    Config(#[from] crate::config::InvalidValue),
    /// The supplied clock or expiry cutoff cannot be represented.
    #[error("gc log expiry clock cannot be represented")]
    Clock,
    /// A recent log exceeded the caller's byte budget; its contents remain untouched.
    #[error("gc log byte limit exceeded")]
    LogLimit,
}

impl Repository {
    /// Publishes this process's pid and the supplied local hostname under Git's gc writer lock.
    ///
    /// Supply the actual local hostname used by Git so it can check the pid's liveness. Girt
    /// never discovers the hostname by spawning a process. Publication holds `gc.pid.lock` and
    /// installs complete `pid hostname` bytes without replacing any existing `gc.pid`. Existing
    /// files, including stale/malformed files, return [`LockError::Busy`]; inspect them manually.
    /// Git may reclaim sufficiently old pid files, so the caller must keep this work bounded and
    /// exclude non-cooperating writers. The lock alone never establishes reader isolation.
    ///
    /// # Errors
    ///
    /// Returns [`GcCoordinationError`] for invalid hostname, contention, or filesystem failure.
    /// A failure after publication can leave `gc.pid` requiring manual inspection. No object,
    /// reference, pack or reflog changes occur.
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// // The fixture never runs Git concurrently. A real caller supplies its local hostname.
    /// let guard = repo.try_lock_gc("fixture-host")?;
    /// assert!(repo.common_dir().join("gc.pid").is_file());
    /// guard.release()?;
    /// assert!(!repo.common_dir().join("gc.pid").exists());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn try_lock_gc(&self, hostname: &str) -> Result<GcLock, GcCoordinationError> {
        if hostname.is_empty()
            || hostname.len() > 255
            || !hostname.is_ascii()
            || hostname
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte == 0)
        {
            return Err(GcCoordinationError::Hostname);
        }
        let _operation = crate::locks::operation();
        let _writer = MaintenanceLock::acquire(self.common_dir().join("gc.pid.lock"))?;
        let path = self.common_dir().join("gc.pid");
        match fs::symlink_metadata(&path) {
            Ok(_) => return Err(LockError::Busy.into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut builder = tempfile::Builder::new();
        builder.prefix(".girt-gc-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Use ordinary lock-file permissions, restricted by the process umask, so another
            // user in a shared repository can read the pid rather than ignore an unreadable lock.
            builder.permissions(fs::Permissions::from_mode(0o666));
        }
        let mut temporary = builder.tempfile_in(self.common_dir())?;
        writeln!(temporary, "{} {hostname}", std::process::id())?;
        temporary.as_file().sync_all()?;
        let file = temporary.persist_noclobber(&path).map_err(|error| {
            if error.error.kind() == io::ErrorKind::AlreadyExists {
                GcCoordinationError::Lock(LockError::Busy)
            } else {
                GcCoordinationError::Io(error.error)
            }
        })?;
        Ok(GcLock(MaintenanceLock::from_file(path, file)?))
    }

    /// Reads a previous gc log with a caller byte budget, without changing it.
    ///
    /// As observed with Git 2.56, `gc.autoDetach=false` bypasses this suppression check. Otherwise
    /// a nonempty `gc.log` newer than `gc.logExpiry` (one day by default) suppresses an automatic
    /// detached run. Empty or expired logs do not block. `never` keeps a nonempty log blocking
    /// regardless of age; `now`/`all` disable age-based suppression. Date syntax is the supported
    /// subset documented by [`Self::configured_gc_policy`]. Nothing is detached or printed.
    ///
    /// This observation can become stale. Execution must recheck it under gc coordination.
    /// Errors and suppression do not authorize deleting the log; retain its diagnostic bytes.
    ///
    /// # Errors
    ///
    /// Returns [`GcCoordinationError`] for invalid config/clock, filesystem errors or a recent
    /// log exceeding `max_bytes`. Expired logs are not read into memory.
    ///
    /// ```
    /// use std::time::SystemTime;
    ///
    /// use girt::maintenance::GcLog;
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// assert_eq!(
    ///     repo.check_auto_gc_log(SystemTime::now(), 4096)?,
    ///     GcLog::NotBlocking
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn check_auto_gc_log(
        &self,
        now: SystemTime,
        max_bytes: usize,
    ) -> Result<GcLog, GcCoordinationError> {
        if !self
            .config()
            .boolean("gc", None, "autoDetach")?
            .unwrap_or(true)
        {
            return Ok(GcLog::NotBlocking);
        }
        let seconds = now
            .duration_since(UNIX_EPOCH)
            .map_err(|_| GcCoordinationError::Clock)?
            .as_secs();
        let value = match self.config().value("gc", None, "logExpiry") {
            None => Some(&b"1 day ago"[..]),
            Some(value) => value,
        };
        let cutoff = value
            .and_then(|value| crate::retention::parse_expiry_date(value, i128::from(seconds)))
            .ok_or_else(|| crate::config::InvalidValue::new("gc", "logExpiry", value))?;
        let path = self.common_dir().join("gc.log");
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(GcLog::NotBlocking),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if metadata.len() == 0 || cutoff == Some(i128::from(u64::MAX)) {
            return Ok(GcLog::NotBlocking);
        }
        if let Some(cutoff) = cutoff {
            let cutoff = u64::try_from(cutoff).map_err(|_| GcCoordinationError::Clock)?;
            let cutoff = UNIX_EPOCH
                .checked_add(Duration::from_secs(cutoff))
                .ok_or(GcCoordinationError::Clock)?;
            if metadata.modified()? <= cutoff {
                return Ok(GcLog::NotBlocking);
            }
        }
        if metadata.len() > max_bytes as u64 {
            return Err(GcCoordinationError::LogLimit);
        }
        let mut bytes = Vec::new();
        file.take((max_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > max_bytes {
            return Err(GcCoordinationError::LogLimit);
        }
        Ok(if bytes.is_empty() {
            GcLog::NotBlocking
        } else {
            GcLog::Suppressed(bytes)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InitKind, ObjectFormat};

    fn fixture(config: &str) -> (tempfile::TempDir, Repository) {
        let root = tempfile::tempdir().unwrap();
        let repo =
            Repository::init(ObjectFormat::Sha1, root.path().join("r"), InitKind::Bare).unwrap();
        let path = repo.common_dir().join("config");
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(config.as_bytes());
        fs::write(path, bytes).unwrap();
        let repo = Repository::open(repo.git_dir()).unwrap();
        (root, repo)
    }

    #[test]
    fn pid_is_complete_and_other_owners_are_preserved() {
        let (_root, repo) = fixture("");
        let guard = repo.try_lock_gc("fixture-host").unwrap();
        let pid = repo.common_dir().join("gc.pid");
        assert_eq!(
            fs::read_to_string(&pid).unwrap(),
            format!("{} fixture-host\n", std::process::id())
        );
        assert!(!repo.common_dir().join("gc.pid.lock").exists());
        assert!(matches!(
            repo.try_lock_gc("fixture-host"),
            Err(GcCoordinationError::Lock(LockError::Busy))
        ));
        guard.release().unwrap();
        assert!(!pid.exists());
        fs::write(&pid, b"stale or malformed owner").unwrap();
        assert!(matches!(
            repo.try_lock_gc("fixture-host"),
            Err(GcCoordinationError::Lock(LockError::Busy))
        ));
        assert_eq!(fs::read(&pid).unwrap(), b"stale or malformed owner");
    }

    #[cfg(unix)]
    #[test]
    fn pid_permissions_match_ordinary_lock_files() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, repo) = fixture("");
        let sibling = repo.common_dir().join("sibling.lock");
        let file = crate::locks::lock_options().open(&sibling).unwrap();
        let expected = file.metadata().unwrap().permissions().mode() & 0o777;
        let _guard = repo.try_lock_gc("fixture-host").unwrap();
        let actual = fs::metadata(repo.common_dir().join("gc.pid"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(actual, expected);
    }

    #[test]
    fn invalid_hostname_does_not_create_locks() {
        let (_root, repo) = fixture("");
        for hostname in ["", "two hosts", "a\nb", "a\0b", "nonascii-é"] {
            assert!(matches!(
                repo.try_lock_gc(hostname),
                Err(GcCoordinationError::Hostname)
            ));
        }
        assert!(!repo.common_dir().join("gc.pid.lock").exists());
        assert!(!repo.common_dir().join("gc.pid").exists());
    }

    #[test]
    fn recent_logs_are_bounded_preserved_and_expire() {
        let (_root, repo) = fixture("");
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let path = repo.common_dir().join("gc.log");
        assert_eq!(repo.check_auto_gc_log(now, 10).unwrap(), GcLog::NotBlocking);
        fs::write(&path, b"failure").unwrap();
        let file = File::options().read(true).write(true).open(&path).unwrap();
        file.set_modified(now).unwrap();
        assert_eq!(
            repo.check_auto_gc_log(now, 7).unwrap(),
            GcLog::Suppressed(b"failure".to_vec())
        );
        assert!(matches!(
            repo.check_auto_gc_log(now, 6),
            Err(GcCoordinationError::LogLimit)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"failure");
        file.set_modified(now - Duration::from_secs(2 * 86400))
            .unwrap();
        assert_eq!(repo.check_auto_gc_log(now, 0).unwrap(), GcLog::NotBlocking);
    }

    #[test]
    fn foreground_bypasses_log_suppression() {
        let (_root, repo) = fixture("\n[gc]\nautoDetach=false\n");
        fs::write(repo.common_dir().join("gc.log"), b"failure").unwrap();
        assert_eq!(
            repo.check_auto_gc_log(SystemTime::now(), 0).unwrap(),
            GcLog::NotBlocking
        );
    }

    #[test]
    fn git_respects_published_pid_then_runs_after_release() {
        let (root, repo) = fixture("\n[gc]\nauto=256\nautoDetach=false\n");
        let sample = repo.object_dir().join("17");
        fs::create_dir(&sample).unwrap();
        for number in 1..=2 {
            fs::write(
                sample.join(format!("{number:038x}")),
                b"invalid selection fixture",
            )
            .unwrap();
        }
        let mut guard = Some(repo.try_lock_gc("girt-fixture-host").unwrap());
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
                .env("GIT_CONFIG_GLOBAL", root.path().join("absent-config"))
                .env("GIT_AUTHOR_NAME", "Maintenance Fixture")
                .env("GIT_AUTHOR_EMAIL", "maintenance@example.com")
                .env("GIT_COMMITTER_NAME", "Maintenance Fixture")
                .env("GIT_COMMITTER_EMAIL", "maintenance@example.com")
                .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
                .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
                .args(["-c", "commit.gpgsign=false", "gc", "--auto"])
                .output()
                .unwrap();
            assert_eq!(output.status.success(), held, "{output:?}");
            if held {
                drop(guard.take());
            }
        }
    }
}
