//! No-clobber publication of caller-validated, synchronized pack artifacts.
//!
//! The caller excludes conflicting publishers and synchronizes complete temporary contents before
//! calling. This module only publishes names; it does not validate objects or authorize retirement.
//! A required promisor marker belongs to the caller and must be durable before pack visibility.
//! Pack names are synchronized before index visibility on Unix; other platforms retain the existing
//! weaker name-durability guarantee. Interruption can leave a complete unindexed pack.
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tempfile::NamedTempFile;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("pack publication cancelled")]
    Cancelled,
    #[error("conflicting existing pack artifact: {0}")]
    Existing(PathBuf),
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub(crate) fn pair_checked<E>(
    pack: NamedTempFile,
    index: NamedTempFile,
    basename: &Path,
    cancel: &AtomicBool,
    after_pack: impl FnOnce(),
    mut fresh: impl FnMut() -> Result<(), E>,
) -> Result<(), E>
where
    E: From<Error>,
{
    check(cancel)?;
    fresh()?;
    file(pack, &basename.with_extension("pack"), cancel)?;
    // Persist the pack name before publishing its index visibility marker.
    #[cfg(unix)]
    File::open(basename.parent().expect("pack basename has a directory"))
        .map_err(Error::Io)?
        .sync_all()
        .map_err(Error::Io)?;
    after_pack();
    check(cancel)?;
    fresh()?;
    file(index, &basename.with_extension("idx"), cancel)?;
    Ok(())
}

pub(crate) fn file(
    temporary: NamedTempFile,
    path: &Path,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            let mut existing = File::open(path)?;
            let mut candidate = error.file.reopen()?;
            if existing.metadata()?.len() != candidate.metadata()?.len() {
                return Err(Error::Existing(path.into()));
            }
            let mut a = [0; 8192];
            let mut b = [0; 8192];
            loop {
                check(cancel)?;
                let n = existing.read(&mut a)?;
                candidate.read_exact(&mut b[..n])?;
                if a[..n] != b[..n] {
                    return Err(Error::Existing(path.into()));
                }
                if n == 0 {
                    return Ok(());
                }
            }
        }
        Err(error) => Err(error.error.into()),
    }
}

fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tempfile::NamedTempFile;

    use super::*;

    fn temporary(directory: &Path, bytes: &[u8]) -> NamedTempFile {
        let mut file = NamedTempFile::new_in(directory).unwrap();
        file.write_all(bytes).unwrap();
        file.as_file().sync_all().unwrap();
        file
    }

    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    fn refuses_conflicting_existing_bytes_without_clobber(#[case] different_length: bool) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pack-existing.pack");
        let original = vec![b'a'; 17_000];
        fs::write(&path, &original).unwrap();
        let mut candidate = original.clone();
        if different_length {
            candidate.push(b'b');
        } else {
            candidate[16_999] = b'b';
        }
        let result = file(
            temporary(root.path(), &candidate),
            &path,
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(Error::Existing(ref conflict)) if *conflict == path));
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn identical_existing_artifacts_are_verified_and_reused() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pack-existing.pack");
        let original = vec![b'a'; 17_000];
        fs::write(&path, &original).unwrap();
        file(
            temporary(root.path(), &original),
            &path,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(fs::read(path).unwrap(), original);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn pack_name_is_synced_before_index_visibility() {
        let root = tempfile::tempdir().unwrap();
        let basename = root.path().join("pack-pair");
        let events = RefCell::new(Vec::new());
        let checks = Cell::new(0);
        pair_checked::<Error>(
            temporary(root.path(), b"pack"),
            temporary(root.path(), b"index"),
            &basename,
            &AtomicBool::new(false),
            || {
                // This checkpoint follows pack-name directory synchronization on Unix.
                assert_eq!(fs::read(basename.with_extension("pack")).unwrap(), b"pack");
                assert!(!basename.with_extension("idx").exists());
                events.borrow_mut().push("pack");
            },
            || {
                checks.set(checks.get() + 1);
                events.borrow_mut().push("fresh");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*events.borrow(), ["fresh", "pack", "fresh"]);
        assert_eq!(checks.get(), 2);
        assert_eq!(fs::read(basename.with_extension("idx")).unwrap(), b"index");
    }

    #[test]
    fn cancellation_between_names_preserves_only_unindexed_pack() {
        let root = tempfile::tempdir().unwrap();
        let basename = root.path().join("pack-pair");
        let cancel = AtomicBool::new(false);
        let result = pair_checked::<Error>(
            temporary(root.path(), b"pack"),
            temporary(root.path(), b"index"),
            &basename,
            &cancel,
            || cancel.store(true, Ordering::Relaxed),
            || Ok(()),
        );
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(fs::read(basename.with_extension("pack")).unwrap(), b"pack");
        assert!(!basename.with_extension("idx").exists());
    }
}
