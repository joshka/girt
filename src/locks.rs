//! Caller-controlled release of this process's transient girt lock files.
//!
//! Call [`cleanup`] during shutdown in normal thread context, after requesting cancellation and
//! preventing new work. Cleanup also cancels pending reference lock waits. A caller-owned signal
//! callback thread can do this; a raw POSIX signal trampoline cannot. Girt installs no signal
//! handlers. Cleanup waits for active lock operations, invalidates live guards and removes only
//! files whose registered identity still matches.
//!
//! Completed publication is never rolled back. Surviving guards refuse publication after cleanup;
//! another acquisition after cleanup is allowed. The caller must exclude hostile lock-path and
//! parent-directory replacement races. Identity checks cannot make check/unlink atomic against a
//! non-cooperating writer. SIGKILL, process crashes and power loss cannot run this API.
//!
//! Unix uses device/inode identities. Windows compares the volume and 128-bit file ID at the path
//! with the registered file and only then acts through a handle to that file. While a retained edit
//! still holds its handle, cleanup renames the file through that handle to a unique
//! `<name>.girt-released-*.lock` name beside it, freeing the registered name at once, and marks it
//! for deletion when the handle closes, which also happens at process exit. Once the writer has
//! closed, cleanup deletes the file directly. A Windows failure keeps the guard valid for ordinary
//! Drop/abort, except a failed deletion mark after the rename: the name is already free, so the
//! guard is invalidated and the failure names the moved file. Other platforms preserve registered
//! locks and report an unsupported cleanup failure, also retaining guard validity. Persistent
//! worktree `locked` markers and pack `.keep` protections are not transient locks and are never
//! registered.
//!
//! # Example
//!
//! In a normal shutdown callback, stop admitting work and wake writers before cleanup, then report
//! failures for manual recovery. Cleanup is process-wide, so this doctest runs in its own process:
//!
//! ```standalone_crate
//! use std::sync::atomic::{AtomicBool, Ordering};
//!
//! // The flag the application's writers poll; set it before cleaning up.
//! let cancel = AtomicBool::new(false);
//! cancel.store(true, Ordering::Relaxed);
//! let report = girt::locks::cleanup()?;
//! // Join cancelled writers before exiting; retain failures for manual recovery.
//! for failure in &report.failures {
//!     eprintln!("lock cleanup failed at {:?}: {}", failure.path, failure.source);
//! }
//! assert!(report.failures.is_empty(), "this process held no girt locks");
//! # Ok::<(), std::io::Error>(())
//! ```
use std::cell::Cell;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, Weak};

static GATE: RwLock<()> = RwLock::new(());
static WAIT_GENERATION: AtomicUsize = AtomicUsize::new(0);
static REGISTRY: std::sync::LazyLock<Mutex<HashMap<usize, Weak<Registration>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
thread_local! { static DEPTH: Cell<usize> = const { Cell::new(0) }; }

/// Effects of one cleanup attempt; failures do not prevent attempts on other owned locks.
///
/// An `Ok` return from [`cleanup`] means the registry was processed, not that every lock was
/// removed. Inspect `failures` and `preserved`. Unix and Windows invalidate the old guard even when
/// a replacement path is preserved; never remove that replacement merely because it has the same
/// name. Persistent worktree lock markers and pack `.keep` files are outside this registry.
///
/// # Preserve a replacement lock path
///
/// A different file at the registered name belongs to a different owner. Cleanup invalidates the
/// old guard and preserves the replacement, even though its spelling is identical.
///
/// ```standalone_crate
/// # #[cfg(unix)]
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use girt::{InitKind, ObjectFormat, Repository};
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Worktree,
/// )?;
/// let edit = repo.edit_index(Default::default())?;
/// let lock = repo.git_dir().join("index.lock");
/// // The registered guard pins its inode, so this replacement has a distinct identity.
/// std::fs::remove_file(&lock)?;
/// std::fs::write(&lock, b"another owner")?;
/// let report = girt::locks::cleanup()?;
/// assert_eq!(report.preserved, [lock.clone()]);
/// assert!(edit.commit().is_err());
/// assert_eq!(std::fs::read(&lock)?, b"another owner");
/// # Ok(())
/// # }
/// # #[cfg(not(unix))]
/// # fn main() {}
/// ```
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
///
/// Registry processing continues for unrelated locks. Keep this diagnostic even when other
/// locks were removed successfully; a failure neither establishes that the path is absent nor
/// authorizes removing a replacement. On Unix, the old guard is already invalidated: abort and
/// drop cannot remove a leftover owned lock after this failure. Manual recovery must establish
/// exclusive control and verify ownership before removal. On Windows and unsupported platforms,
/// the guard stays valid and ordinary abort/drop still performs its cleanup, unless Windows had
/// already moved the lock aside: then the guard is invalidated and the source names the moved file.
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

/// Releases identity-matched transient locks and invalidates their live registered guards.
///
/// This process-wide operation includes index/config edits, refs/reflogs/reftable, shallow
/// metadata, clone config, credentials and worktree administration mutexes. It does not scan for
/// stale locks. Cleanup cancels pending reference lock waits before waiting for active publication
/// and drop operations. On Unix and Windows, retained edits may still own open handles but cannot
/// subsequently publish. A Windows lock that cleanup could not release, and every lock on other
/// platforms, keeps its guard valid for ordinary Drop/abort. Partial effects
/// completed before cleanup remain completed; new work after cleanup must be excluded by the
/// caller.
///
/// # Errors
///
/// Returns `WouldBlock` if invoked reentrantly from an active girt lock operation, avoiding
/// self-deadlock. Per-file failures are returned in [`CleanupReport::failures`]. No raw signal
/// handler may call this function: it locks, allocates and performs filesystem I/O.
///
/// See the [module example](crate::locks#example) for a shutdown sequence.
///
/// # Cancel and invalidate an outstanding edit
///
/// Run cleanup on a normal thread after stopping admissions and requesting cancellation. This
/// standalone example holds a real index edit: on Unix and Windows its guard cannot publish after
/// cleanup. On other platforms cleanup reports unsupported identity cleanup and ordinary abort
/// still works.
///
/// ```standalone_crate
/// use girt::{InitKind, ObjectFormat, Repository};
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(ObjectFormat::Sha1, directory.path().join("r"), InitKind::Bare)?;
/// let edit = repo.edit_index(Default::default())?;
/// let report = girt::locks::cleanup()?;
/// #[cfg(any(unix, windows))]
/// {
///     assert_eq!(report.removed.len(), 1);
///     assert!(edit.commit().is_err());
///     assert!(repo.read_index(Default::default())?.is_none());
/// }
/// #[cfg(not(any(unix, windows)))]
/// {
///     assert_eq!(report.failures.len(), 1);
///     edit.abort()?;
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn cleanup() -> io::Result<CleanupReport> {
    cleanup_matching(|_| true, true)
}

#[cfg(test)]
pub(crate) fn cleanup_under(root: &std::path::Path) -> io::Result<CleanupReport> {
    cleanup_matching(|path| path.starts_with(root), false)
}

fn cleanup_matching(
    select: impl Fn(&std::path::Path) -> bool,
    cancel_waits: bool,
) -> io::Result<CleanupReport> {
    if DEPTH.with(|depth| depth.get() != 0) {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "cleanup inside a lock operation",
        ));
    }
    // Wake pending reference lock waits before waiting for an active publisher's read gate.
    if cancel_waits {
        WAIT_GENERATION.fetch_add(1, Ordering::Relaxed);
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
        if !select(&registration.path) || !registration.active.load(Ordering::Relaxed) {
            continue;
        }
        // Unix invalidates first: its check/unlink sequence cannot be undone. Windows deletes in
        // one handle operation, so a failure leaves the guard valid for ordinary Drop/abort.
        #[cfg(unix)]
        registration.active.store(false, Ordering::Relaxed);
        match registration.remove_owned() {
            Ok(true) => {
                registration.active.store(false, Ordering::Relaxed);
                report.removed.push(registration.path.clone());
            }
            Ok(false) => {
                registration.active.store(false, Ordering::Relaxed);
                report.preserved.push(registration.path.clone());
            }
            Err(source) => report.failures.push(CleanupFailure {
                path: registration.path.clone(),
                source,
            }),
        }
    }
    Ok(report)
}

/// Options for exclusively creating a transient lock file that caller cleanup can release.
///
/// Windows handles also get delete access, so cleanup can delete the file through the writer's
/// own handle when the lock denies delete sharing to everyone else. Sharing is unchanged.
pub(crate) fn lock_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES,
        };
        options.access_mode(FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES | DELETE);
    }
    options
}

/// Like [`lock_options`], but other Windows openers can't rename or delete the file while its
/// handle is open. Suits locks that publish through a separate temporary file.
pub(crate) fn unreplaceable_lock_options() -> fs::OpenOptions {
    #[cfg_attr(not(windows), expect(unused_mut))]
    let mut options = lock_options();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    options
}

pub(crate) fn wait_generation() -> usize {
    WAIT_GENERATION.load(Ordering::Relaxed)
}

/// Shares one OS handle between its writer and the Unix identity pin; cloning never duplicates a
/// descriptor. Windows registrations only keep a weak reference, so the writer still closes the
/// handle before it renames or removes the lock, as delete-sharing rules require.
#[derive(Clone, Debug)]
pub(crate) struct SharedFile(Arc<File>);

impl From<File> for SharedFile {
    fn from(file: File) -> Self {
        Self(Arc::new(file))
    }
}

impl std::ops::Deref for SharedFile {
    type Target = File;
    fn deref(&self) -> &File {
        &self.0
    }
}

impl std::io::Write for SharedFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut file = self.0.as_ref();
        std::io::Write::write(&mut file, bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        let mut file = self.0.as_ref();
        std::io::Write::flush(&mut file)
    }
}

/// An identity failure cannot authorize unlinking the file. Keep the original cause and name
/// the lock requiring manual recovery, rather than returning a context-free metadata error.
#[derive(Debug, thiserror::Error)]
#[error("lock identity unavailable at {path:?}; cleanup refused")]
struct IdentityFailure {
    path: PathBuf,
    #[source]
    source: io::Error,
}

pub(crate) fn capture_identity(path: &std::path::Path, file: &File) -> io::Result<fs::Metadata> {
    file.metadata().map_err(|source| {
        io::Error::new(
            source.kind(),
            IdentityFailure {
                path: path.to_owned(),
                source,
            },
        )
    })
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
    #[cfg(unix)]
    identity: fs::Metadata,
    #[cfg(unix)]
    _pinned: SharedFile,
    /// The writer's handle while it is open; cleanup deletes through it.
    #[cfg(windows)]
    writer: Weak<File>,
    /// Captured at registration; cleanup refuses without it.
    #[cfg(windows)]
    file_id: io::Result<windows::FileId>,
    active: AtomicBool,
}

impl Guard {
    /// The caller holds an operation guard across exclusive creation and registration.
    pub(crate) fn new(path: &std::path::Path, file: &SharedFile, identity: fs::Metadata) -> Self {
        debug_assert!(path.is_absolute());
        #[cfg(not(any(unix, windows)))]
        let _ = file;
        #[cfg(not(unix))]
        let _ = identity;
        let registration = Arc::new(Registration {
            path: path.to_owned(),
            #[cfg(unix)]
            identity,
            #[cfg(unix)]
            _pinned: file.clone(),
            #[cfg(windows)]
            writer: Arc::downgrade(&file.0),
            #[cfg(windows)]
            file_id: windows::FileId::of(file),
            active: AtomicBool::new(true),
        });
        let mut registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
        registry.insert(
            Arc::as_ptr(&registration) as usize,
            Arc::downgrade(&registration),
        );
        Self(registration)
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

    /// Releases the registered file only while the path still names it. Every change goes
    /// through a handle to that file, never the path, so it cannot reach a replacement opened after
    /// the check. A lock that denies delete sharing cannot be replaced at all while its writer is
    /// open, and the writer's handle is then the only one that may rename or delete it.
    #[cfg(windows)]
    fn remove_owned(&self) -> io::Result<bool> {
        let owned = match &self.file_id {
            Ok(id) => *id,
            Err(source) => {
                let message =
                    format!("lock file identity unavailable since registration: {source}");
                return Err(io::Error::new(source.kind(), message));
            }
        };
        let probe = match windows::open_existing(&self.path, false) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error),
        };
        if !windows::names(&probe, owned)? {
            return Ok(false);
        }
        drop(probe);
        if let Some(writer) = self.writer.upgrade() {
            // A deletion through an open handle completes only when that handle closes, so move
            // the name aside first. A failed rename changes nothing.
            let aside = windows::move_aside(&writer, &self.path)?;
            // The name now belongs to whoever takes it next; the old guard must never act on it,
            // even if the deletion mark below fails.
            self.active.store(false, Ordering::Relaxed);
            windows::delete_on_close(&writer).map_err(|source| {
                let message = format!(
                    "released lock moved to {aside:?} but not marked for deletion: {source}"
                );
                io::Error::new(source.kind(), message)
            })?;
            return Ok(true);
        }
        // The writer has closed, so no handle denies delete sharing; verify this handle too.
        let file = match windows::open_existing(&self.path, true) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error),
        };
        if !windows::names(&file, owned)? {
            return Ok(false);
        }
        windows::delete_on_close(&file)?;
        Ok(true)
    }

    #[cfg(not(any(unix, windows)))]
    fn remove_owned(&self) -> io::Result<bool> {
        match fs::symlink_metadata(&self.path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned lock cleanup identity on this platform",
        ))
    }
}

#[cfg(windows)]
pub(crate) mod windows {
    use std::fs::File;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FileDispositionInfo, FileIdInfo, FileRenameInfo,
        GetFileInformationByHandleEx, SetFileInformationByHandle,
    };

    /// A file's identity on its volume, stable across renames and unaffected by path spelling.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct FileId {
        volume: u64,
        file: [u8; 16],
    }

    impl FileId {
        pub(super) fn of(file: &File) -> io::Result<Self> {
            let mut info = FILE_ID_INFO::default();
            // SAFETY: the handle is open and the buffer matches the requested class and size.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileIdInfo,
                    (&raw mut info).cast(),
                    size_of::<FILE_ID_INFO>() as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                volume: info.VolumeSerialNumber,
                file: info.FileId.Identifier,
            })
        }
    }

    /// Opens what the path names now, without following a final reparse point. Attribute-only
    /// access conflicts with no sharing mode; delete access needs every open handle to share it.
    pub(super) fn open_existing(path: &Path, delete: bool) -> io::Result<File> {
        let access = if delete {
            FILE_READ_ATTRIBUTES | DELETE
        } else {
            FILE_READ_ATTRIBUTES
        };
        std::fs::OpenOptions::new()
            .access_mode(access)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }

    /// Whether `file` is a regular file with the registered identity.
    pub(super) fn names(file: &File, owned: FileId) -> io::Result<bool> {
        let kind = file.metadata()?.file_type();
        Ok(kind.is_file() && !kind.is_symlink() && FileId::of(file)? == owned)
    }

    /// Renames the file through its handle to a unique `.lock` name in the same directory, which
    /// Git ignores as a lock. Never replaces an existing file.
    pub(super) fn move_aside(file: &File, path: &Path) -> io::Result<PathBuf> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut aside = path
            .file_name()
            .ok_or_else(|| io::Error::other("lock path has no file name"))?
            .to_os_string();
        let next = NEXT.fetch_add(1, Ordering::Relaxed);
        aside.push(format!(".girt-released-{}-{next}.lock", std::process::id()));
        let aside = path.with_file_name(aside);
        // The full path: a bare name would resolve against the current directory, not the lock's.
        let name: Vec<u16> = aside.as_os_str().encode_wide().collect();
        // The header's one-element name array leaves room for a terminating null.
        let size = size_of::<FILE_RENAME_INFO>() + size_of_val(name.as_slice());
        // Zeroed: no replacement and no root directory.
        let mut buffer = vec![0u64; size.div_ceil(8)];
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: the 8-byte-aligned buffer holds the header and the name from its name field.
        unsafe {
            (&raw mut (*info).FileNameLength).write(size_of_val(name.as_slice()) as u32);
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                (&raw mut (*info).FileName).cast::<u16>(),
                name.len(),
            );
        }
        // SAFETY: the handle is open and the buffer is a rename structure of the stated size.
        let ok = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileRenameInfo,
                info.cast(),
                size as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(aside)
    }

    #[cfg(test)]
    thread_local! {
        /// Fails the next deletion marks on this thread, after any rename has succeeded.
        pub(crate) static FAIL_DELETE_MARK: std::cell::Cell<bool> = const {
            std::cell::Cell::new(false)
        };
    }

    /// Marks the file for deletion once every handle to it has closed, including at process exit.
    pub(super) fn delete_on_close(file: &File) -> io::Result<()> {
        #[cfg(test)]
        if FAIL_DELETE_MARK.with(std::cell::Cell::get) {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: the handle is open and the buffer matches the requested class and size.
        let ok = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&raw const info).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                let file = lock_options().open(path_ref).unwrap();
                created.send(()).unwrap();
                wait.recv().unwrap();
                let _nested = operation();
                let identity = file.metadata().unwrap();
                let file = SharedFile::from(file);
                let guard = Guard::new(path_ref, &file, identity);
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

    fn register(path: &std::path::Path, options: &fs::OpenOptions) -> (SharedFile, Guard) {
        let _operation = operation();
        let file = options.open(path).unwrap();
        let identity = capture_identity(path, &file).unwrap();
        let file = SharedFile::from(file);
        let guard = Guard::new(path, &file, identity);
        (file, guard)
    }

    #[test]
    fn cleanup_frees_the_name_while_an_unreplaceable_writer_stays_open() {
        let root = tempfile::tempdir().unwrap();
        let path = std::path::absolute(root.path().join("owned.lock")).unwrap();
        let (mut file, guard) = register(&path, &unreplaceable_lock_options());
        std::io::Write::write_all(&mut file, b"draft").unwrap();
        let report = cleanup_under(root.path()).unwrap();
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.removed, std::slice::from_ref(&path));
        assert!(guard.check().is_err());
        assert!(!path.exists());
        // Another owner can take the name although the old writer's handle is still open.
        let next = lock_options().open(&path).unwrap();
        drop((file, guard));
        assert!(path.is_file());
        drop(next);
        // Nothing the old writer held outlives its handle.
        let names = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, ["owned.lock"]);
    }

    #[test]
    fn cleanup_after_the_writer_closed_removes_only_the_registered_file() {
        let root = tempfile::tempdir().unwrap();
        let path = std::path::absolute(root.path().join("owned.lock")).unwrap();
        let (file, guard) = register(&path, &unreplaceable_lock_options());
        drop(file);
        let report = cleanup_under(root.path()).unwrap();
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.removed, std::slice::from_ref(&path));
        assert!(guard.check().is_err());
        assert!(!path.exists());

        let (file, guard) = register(&path, &unreplaceable_lock_options());
        drop(file);
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"replacement").unwrap();
        let report = cleanup_under(root.path()).unwrap();
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.preserved, std::slice::from_ref(&path));
        assert!(guard.check().is_err());
        drop(guard);
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
    }

    #[cfg(windows)]
    #[test]
    fn failed_windows_cleanup_reports_the_path_and_keeps_the_guard_valid() {
        let root = tempfile::tempdir().unwrap();
        let path = std::path::absolute(root.path().join("owned.lock")).unwrap();
        // Without delete access on the writer's handle, nothing may delete the file.
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 3);
        let (file, guard) = register(&path, &options);
        let report = cleanup_under(root.path()).unwrap();
        assert!(report.removed.is_empty() && report.preserved.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].path, path);
        assert!(guard.check().is_ok());
        assert!(path.is_file());
        drop(file);
    }

    #[cfg(windows)]
    #[test]
    fn failed_delete_mark_after_rename_invalidates_the_guard() {
        let root = tempfile::tempdir().unwrap();
        let path = std::path::absolute(root.path().join("owned.lock")).unwrap();
        let (file, guard) = register(&path, &unreplaceable_lock_options());
        windows::FAIL_DELETE_MARK.with(|fail| fail.set(true));
        let report = cleanup_under(root.path());
        windows::FAIL_DELETE_MARK.with(|fail| fail.set(false));
        let report = report.unwrap();
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].path, path);
        assert!(
            report.failures[0]
                .source
                .to_string()
                .contains("girt-released")
        );
        assert!(guard.check().is_err());
        // The name is free for a new owner, which the old guard can no longer disturb.
        fs::write(&path, b"replacement").unwrap();
        drop((file, guard));
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
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
            let identity = file.metadata().unwrap();
            let file = SharedFile::from(file);
            Guard::new(&path, &file, identity)
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

#[cfg(all(test, unix))]
mod handle_tests {
    use std::os::fd::AsRawFd;

    use super::*;

    #[test]
    fn identity_pin_shares_the_writers_single_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned.lock");
        let raw = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let identity = raw.metadata().unwrap();
        let file = SharedFile::from(raw);
        let _operation = operation();
        let guard = Guard::new(&path, &file, identity);
        assert_eq!(file.as_raw_fd(), guard.0._pinned.as_raw_fd());
    }
}
