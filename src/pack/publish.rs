//! No-clobber publication of caller-validated, synchronized pack artifacts.

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
