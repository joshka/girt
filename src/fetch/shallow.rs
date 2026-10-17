//! Conditional publication of Git's shared shallow boundary file.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use crate::{ObjectFormat, ObjectId, ShallowRoots};

/// Failure acquiring or publishing the shared shallow boundary file.
///
/// A fetch that changes the shallow boundaries takes a `shallow.lock` file before installing its
/// objects, and replaces `.git/shallow` after verifying them, before publishing references. This
/// error arrives inside [`FetchFinishFailure::Shallow`](super::FetchFinishFailure::Shallow).
/// `.git/shallow` and every reference are unchanged; the pack is installed only if the failure
/// came after taking the lock, which the report's `installed` field shows.
///
/// - [`Changed`](Self::Changed): another operation changed the shallow boundaries after
///   [`FetchRequest::prepare`](super::FetchRequest::prepare). Prepare the fetch again.
/// - [`Read`](Self::Read): the existing `.git/shallow` file is malformed, unreadable or too large.
///   Inspect it; girt will not replace a file it cannot interpret.
/// - [`Io`](Self::Io): the lock could not be taken or the file not replaced. An existing
///   `shallow.lock` usually means another fetch is running; retry once it finishes, or remove the
///   lock if it was left behind by a crash.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeSet;
/// use std::num::NonZeroU32;
/// use std::sync::atomic::AtomicBool;
///
/// use girt::fetch::{
///     FetchFinishFailure, FetchLimits, FetchRequest, FetchShallowError, FetchUpdateLimits,
///     KnownHistory, LocalFetchObserver,
/// };
/// use girt::refs::Reflog;
/// use girt::remote::Remote;
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, main, _) = include!("../doctest/remote.rs");
/// # let commit = include!("../doctest/commit.rs");
/// # let second = commit(&upstream, &[main], "Second commit")?;
/// # upstream.references()?.update_without_reflog(&girt::refs::RefName::new("refs/heads/main")?, girt::refs::Target::Direct(second), girt::refs::Expected::Value(girt::refs::Target::Direct(main)))?;
/// // Upstream's `main` has two commits, so a depth-one fetch must publish a shallow boundary,
/// // but another process holds the shallow lock.
/// std::fs::write(repo.git_dir().join("shallow.lock"), b"")?;
///
/// let origin = Remote::find(repo.config(), b"origin")?.expect("configured");
/// let request =
///     FetchRequest::prepare(repo.clone(), origin.fetch_refspecs().clone(), BTreeSet::new(), Reflog::Preserve)?
///         .with_depth(NonZeroU32::MIN);
/// let cancel = AtomicBool::new(false);
/// let ready = request.receive_local(
///     upstream.git_dir(),
///     &KnownHistory::default(),
///     FetchLimits::default(),
///     TransportControl::new(&cancel),
///     LocalFetchObserver::default(),
/// )?;
/// let error = ready.finish(FetchUpdateLimits::default(), &cancel).unwrap_err();
/// assert!(matches!(*error.source, FetchFinishFailure::Shallow(FetchShallowError::Io(_))));
/// // The lock is taken before installation, so nothing was written.
/// assert!(error.report.installed.is_none());
/// assert!(repo.references()?.list()?.is_empty(), "no reference was published");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FetchShallowError {
    /// The shallow roots differ from the snapshot captured at preparation.
    #[error("shallow metadata changed since fetch preparation")]
    Changed,
    /// Existing shallow metadata is invalid, inaccessible, or exceeds the byte budget.
    #[error("shallow metadata")]
    Read(#[from] crate::ShallowError),
    /// Lock creation, file sync, or replacement failed.
    #[error("shallow metadata I/O")]
    Io(#[from] io::Error),
}

#[derive(Debug)]
pub(super) struct Lock {
    path: PathBuf,
    lock_path: PathBuf,
    file: Option<crate::locks::SharedFile>,
    #[cfg(unix)]
    identity: fs::Metadata,
    old: Vec<ObjectId>,
    cleanup: crate::locks::Guard,
}

impl Lock {
    pub(super) fn acquire(
        common_dir: &std::path::Path,
        format: ObjectFormat,
        old: &[ObjectId],
        cancel: &AtomicBool,
    ) -> Result<Self, FetchShallowError> {
        let _lock_operation = crate::locks::operation();
        let path = common_dir.join("shallow");
        let lock_path = std::path::absolute(common_dir.join("shallow.lock"))?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(3); // Keep the held lock unreplaceable on Windows.
        }
        let file = options.open(&lock_path)?;
        let identity = crate::locks::capture_identity(&lock_path, &file)?;
        let file = crate::locks::SharedFile::from(file);
        let cleanup = crate::locks::Guard::new(&lock_path, &file, identity.clone());
        let lock = Self {
            cleanup,
            path,
            lock_path,
            file: Some(file),
            #[cfg(unix)]
            identity,
            old: old.to_vec(),
        };
        let current = ShallowRoots::read(&lock.path, format, 16 * 1024 * 1024, cancel)?;
        if current.iter().ne(old.iter().copied()) {
            return Err(FetchShallowError::Changed);
        }
        Ok(lock)
    }

    /// Publishes the new sorted roots after object installation while retaining the lock.
    /// A later reference failure leaves this new file in place for explicit recovery.
    pub(super) fn publish(&mut self, roots: &[ObjectId]) -> Result<bool, FetchShallowError> {
        let _lock_operation = crate::locks::operation();
        if self.old == roots {
            return Ok(false);
        }
        self.check_owned()?;
        let mut file = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())?;
        for id in roots {
            writeln!(file, "{id}")?;
        }
        file.as_file().sync_all()?;
        self.check_owned()?;
        file.persist(&self.path).map_err(|error| error.error)?;
        Ok(true)
    }

    fn check_owned(&self) -> Result<(), FetchShallowError> {
        self.cleanup.check()?;
        let current = fs::symlink_metadata(&self.lock_path)?;
        if !current.is_file() || current.file_type().is_symlink() {
            return Err(FetchShallowError::Changed);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if current.dev() != self.identity.dev() || current.ino() != self.identity.ino() {
                return Err(FetchShallowError::Changed);
            }
        }
        Ok(())
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _lock_operation = crate::locks::operation();
        if self.check_owned().is_ok() {
            drop(self.file.take());
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_snapshot_and_foreign_lock_preserve_shallow_bytes() {
        let root = tempfile::tempdir().unwrap();
        let first = ObjectId::for_blob(ObjectFormat::Sha1, b"first");
        let second = ObjectId::for_blob(ObjectFormat::Sha1, b"second");
        let path = root.path().join("shallow");
        fs::write(&path, format!("{first}\n")).unwrap();
        assert!(matches!(
            Lock::acquire(
                root.path(),
                ObjectFormat::Sha1,
                &[second],
                &AtomicBool::new(false)
            ),
            Err(FetchShallowError::Changed)
        ));
        assert!(!root.path().join("shallow.lock").exists());
        fs::write(root.path().join("shallow.lock"), b"foreign").unwrap();
        assert!(matches!(
            Lock::acquire(
                root.path(),
                ObjectFormat::Sha1,
                &[first],
                &AtomicBool::new(false)
            ),
            Err(FetchShallowError::Io(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), format!("{first}\n").as_bytes());
        assert_eq!(
            fs::read(root.path().join("shallow.lock")).unwrap(),
            b"foreign"
        );
    }
    #[cfg(unix)]
    #[test]
    fn cleanup_invalidates_shallow_publication_without_changing_roots() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().canonicalize().unwrap();
        let old = ObjectId::for_blob(ObjectFormat::Sha1, b"original root");
        let new = ObjectId::for_blob(ObjectFormat::Sha1, b"new root");
        let path = directory.join("shallow");
        let bytes = format!("{old}\n");
        fs::write(&path, &bytes).unwrap();
        let mut lock = Lock::acquire(
            &directory,
            ObjectFormat::Sha1,
            &[old],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            crate::locks::cleanup_under(&directory)
                .unwrap()
                .removed
                .len(),
            1
        );
        assert!(lock.publish(&[new]).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), bytes);
    }
}
