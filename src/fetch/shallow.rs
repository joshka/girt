//! Conditional publication of Git's shared shallow boundary file.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use crate::{ObjectFormat, ObjectId, ShallowRoots};

/// Failure acquiring or publishing the shared shallow boundary file.
#[derive(Debug, thiserror::Error)]
pub enum FetchShallowError {
    /// The shallow roots differ from the snapshot captured at preparation.
    #[error("shallow metadata changed since fetch preparation")]
    Changed,
    /// Existing shallow metadata is invalid, inaccessible, or exceeds the byte budget.
    #[error("shallow metadata: {0}")]
    Read(#[from] crate::ShallowError),
    /// Lock creation, file sync, or replacement failed.
    #[error("shallow metadata I/O: {0}")]
    Io(#[from] io::Error),
}

pub(super) struct Lock {
    path: PathBuf,
    lock_path: PathBuf,
    file: Option<File>,
    #[cfg(unix)]
    identity: fs::Metadata,
    old: Vec<ObjectId>,
}

impl Lock {
    pub(super) fn acquire(
        common_dir: &std::path::Path,
        format: ObjectFormat,
        old: &[ObjectId],
        cancel: &AtomicBool,
    ) -> Result<Self, FetchShallowError> {
        let path = common_dir.join("shallow");
        let lock_path = common_dir.join("shallow.lock");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(3); // Keep the held lock unreplaceable on Windows.
        }
        let file = options.open(&lock_path)?;
        #[cfg(unix)]
        let identity = file.metadata()?;
        let lock = Self {
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
}
