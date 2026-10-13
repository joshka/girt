//! Caller-controlled release of this process's transient girt lock files.
//!
//! Call [`cleanup`] during shutdown in normal thread context, after requesting cancellation and
//! preventing new work. A caller-owned signal callback thread can do this; a raw POSIX signal
//! trampoline cannot. Girt installs no signal handlers. Cleanup waits for active lock operations,
//! invalidates live guards and removes only files whose registered identity still matches.
//!
//! Completed publication is never rolled back. Surviving guards refuse publication after cleanup;
//! another acquisition after cleanup is allowed. The caller must exclude hostile lock-path and
//! parent-directory replacement races. Identity checks cannot make check/unlink atomic against a
//! non-cooperating writer. SIGKILL, process crashes and power loss cannot run this API.
//!
//! Unix uses device/inode identities. Other platforms preserve registered locks and report an
//! unsupported cleanup failure. Persistent worktree `locked` markers and pack `.keep` protections
//! are not transient locks and are never registered.
use std::cell::Cell;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, Weak};

static GATE: RwLock<()> = RwLock::new(());
static REGISTRY: std::sync::LazyLock<Mutex<HashMap<usize, Weak<Registration>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
thread_local! { static DEPTH: Cell<usize> = const { Cell::new(0) }; }

/// Effects of one cleanup attempt; failures do not prevent attempts on other owned locks.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct CleanupReport {
    /// Owned locks removed, or already absent, after their guards were invalidated.
    pub removed: Vec<PathBuf>,
    /// Replaced or non-regular paths preserved after invalidating the old guard.
    pub preserved: Vec<PathBuf>,
    /// Locks whose inspection or removal failed. Inspect these before manual recovery.
    pub failures: Vec<CleanupFailure>,
}

/// One cleanup refusal or filesystem failure, retaining its path and underlying cause.
#[derive(Debug, thiserror::Error)]
#[error("lock cleanup at {path:?}")]
#[non_exhaustive]
pub struct CleanupFailure {
    /// Absolute registered lock path.
    pub path: PathBuf,
    /// Identity inspection, unsupported platform, or unlink failure.
    #[source]
    pub source: io::Error,
}

/// Invalidates all live registered guards and releases their identity-matched transient locks.
///
/// This process-wide operation includes index/config edits, refs/reflogs/reftable, shallow
/// metadata, clone config, credentials and worktree administration mutexes. It does not scan for
/// stale locks. Active acquisition/publication/drop operations finish before cleanup; a blocked
/// lock wait must first be cancelled by the caller. Retained edits may still own open file handles,
/// but cannot subsequently publish. Partial ref/worktree effects completed before cleanup remain
/// completed.
///
/// # Errors
///
/// Returns `WouldBlock` if invoked reentrantly from an active girt lock operation, avoiding
/// self-deadlock. Per-file failures are returned in [`CleanupReport::failures`]. No raw signal
/// handler may call this function: it locks, allocates and performs filesystem I/O.
///
/// ```no_run
/// # use std::sync::atomic::{AtomicBool, Ordering};
/// # let cancel = AtomicBool::new(false);
/// // In a normal shutdown callback, first stop admitting work and wake/join writers.
/// cancel.store(true, Ordering::Relaxed);
/// let report = girt::locks::cleanup()?;
/// for failure in report.failures {
///     eprintln!(
///         "lock cleanup failed at {:?}: {}",
///         failure.path, failure.source
///     );
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn cleanup() -> io::Result<CleanupReport> {
    cleanup_matching(|_| true)
}

#[cfg(test)]
pub(crate) fn cleanup_under(root: &std::path::Path) -> io::Result<CleanupReport> {
    cleanup_matching(|path| path.starts_with(root))
}

fn cleanup_matching(select: impl Fn(&std::path::Path) -> bool) -> io::Result<CleanupReport> {
    if DEPTH.with(|depth| depth.get() != 0) {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "cleanup inside a lock operation",
        ));
    }
    let _exclusive = GATE.write().unwrap_or_else(|poison| poison.into_inner());
    let registrations = {
        let registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
        registry
            .values()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>()
    };
    let mut report = CleanupReport::default();
    for registration in registrations {
        if !select(&registration.path) || !registration.active.swap(false, Ordering::Relaxed) {
            continue;
        }
        match registration.remove_owned() {
            Ok(true) => report.removed.push(registration.path.clone()),
            Ok(false) => report.preserved.push(registration.path.clone()),
            Err(source) => report.failures.push(CleanupFailure {
                path: registration.path.clone(),
                source,
            }),
        }
    }
    Ok(report)
}

/// Serializes acquisition, publication and destruction against caller cleanup. Nested library
/// phases on one thread share the outer read guard, so a waiting cleanup writer cannot deadlock
/// a nested read acquisition. This guard is deliberately not Send.
pub(crate) struct Operation {
    _guard: Option<RwLockReadGuard<'static, ()>>,
}

pub(crate) fn operation() -> Operation {
    let guard = DEPTH.with(|depth| {
        let guard = if depth.get() == 0 {
            Some(GATE.read().unwrap_or_else(|poison| poison.into_inner()))
        } else {
            None
        };
        depth.set(depth.get() + 1);
        guard
    });
    Operation { _guard: guard }
}

impl Drop for Operation {
    fn drop(&mut self) {
        DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

#[derive(Debug)]
pub(crate) struct Guard(Arc<Registration>);

#[derive(Debug)]
struct Registration {
    path: PathBuf,
    identity: fs::Metadata,
    #[cfg(unix)]
    _pinned: File,
    active: AtomicBool,
}

impl Guard {
    /// The caller holds an operation guard across exclusive creation and registration.
    pub(crate) fn new(path: &std::path::Path, file: &File) -> io::Result<Self> {
        let registration = Arc::new(Registration {
            path: std::path::absolute(path)?,
            identity: file.metadata()?,
            #[cfg(unix)]
            _pinned: file.try_clone()?,
            active: AtomicBool::new(true),
        });
        let mut registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
        registry.insert(
            Arc::as_ptr(&registration) as usize,
            Arc::downgrade(&registration),
        );
        Ok(Self(registration))
    }

    pub(crate) fn check_path(&self) -> io::Result<()> {
        self.check()?;
        self.0.check_identity()
    }

    pub(crate) fn check(&self) -> io::Result<()> {
        if self.0.active.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "lock invalidated by caller cleanup",
            ))
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        REGISTRY
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&(Arc::as_ptr(&self.0) as usize));
    }
}

impl Registration {
    fn check_identity(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let current = fs::symlink_metadata(&self.path)?;
            if !current.is_file()
                || current.dev() != self.identity.dev()
                || current.ino() != self.identity.ino()
            {
                return Err(io::Error::other("registered lock path replaced"));
            }
        }
        Ok(())
    }
    #[cfg(unix)]
    fn remove_owned(&self) -> io::Result<bool> {
        use std::os::unix::fs::MetadataExt;
        let current = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error),
        };
        if !current.is_file()
            || current.dev() != self.identity.dev()
            || current.ino() != self.identity.ino()
        {
            return Ok(false);
        }
        fs::remove_file(&self.path)?;
        Ok(true)
    }

    #[cfg(not(unix))]
    fn remove_owned(&self) -> io::Result<bool> {
        let _ = &self.identity;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned lock cleanup identity on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn cleanup_waits_for_registration_and_nested_operation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned.lock");
        let (created, observe) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let (cleaned, done) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let path_ref = &path;
            let creator = scope.spawn(move || {
                let _operation = operation();
                let file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path_ref)
                    .unwrap();
                created.send(()).unwrap();
                wait.recv().unwrap();
                let _nested = operation();
                let guard = Guard::new(path_ref, &file).unwrap();
                // Keep the registration alive after the operation releases the acquisition gate.
                guard
            });
            observe.recv().unwrap();
            let worker = scope.spawn(|| {
                let report = cleanup_under(root.path()).unwrap();
                cleaned.send(()).unwrap();
                report
            });
            assert!(
                done.recv_timeout(std::time::Duration::from_millis(50))
                    .is_err()
            );
            release.send(()).unwrap();
            let report = worker.join().unwrap();
            assert!(report.failures.is_empty());
            // The creator's returned guard is retained by its JoinHandle until scope completion.
            assert_eq!(report.removed, vec![path.clone()]);
            let guard = creator.join().unwrap();
            assert!(guard.check().is_err());
        });
        assert!(!path.exists());
    }

    #[test]
    fn cleanup_from_a_lock_callback_refuses_without_deadlock() {
        let _operation = operation();
        assert_eq!(cleanup().unwrap_err().kind(), io::ErrorKind::WouldBlock);
    }
}

#[cfg(all(test, unix))]
mod failure_tests {
    use super::*;

    #[test]
    fn inspection_failure_preserves_the_old_file_and_reports_its_path() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("owned.lock");
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let guard = {
            let _operation = operation();
            Guard::new(&path, &file).unwrap()
        };
        fs::rename(&parent, root.path().join("saved-parent")).unwrap();
        fs::write(&parent, b"foreign parent replacement").unwrap();
        let report = cleanup_under(root.path()).unwrap();
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].path, path);
        assert!(guard.check().is_err());
        assert!(root.path().join("saved-parent/owned.lock").is_file());
        assert_eq!(fs::read(parent).unwrap(), b"foreign parent replacement");
    }
}
