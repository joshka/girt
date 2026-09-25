use rstest::rstest;

use super::*;
use crate::index::{Mode, Stat, Timestamp};
use crate::{InitKind, ObjectId};

fn repository(format: crate::ObjectFormat) -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Worktree).unwrap();
    (root, repo)
}
fn populated(repo: &Repository) -> Vec<u8> {
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    edit.replace_entries(vec![Entry::new(
        b"a".to_vec(),
        Mode::Regular,
        ObjectId::for_blob(repo.object_format(), b"a"),
    )])
    .unwrap();
    edit.commit().unwrap();
    fs::read(repo.git_dir().join("index")).unwrap()
}
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn absent_is_distinct_from_published_empty(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    assert!(repo.read_index(Limits::default()).unwrap().is_none());
    repo.edit_index(Limits::default())
        .unwrap()
        .commit()
        .unwrap();
    assert!(
        repo.read_index(Limits::default())
            .unwrap()
            .unwrap()
            .entries()
            .is_empty()
    );
    assert!(!repo.git_dir().join("index.lock").exists());
}
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn lock_contention_and_drop_preserve_original(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    let before = populated(&repo);
    let edit = repo.edit_index(Limits::default()).unwrap();
    assert!(matches!(
        repo.edit_index(Limits::default()),
        Err(StorageError::Locked(_))
    ));
    assert!(repo.git_dir().join("index.lock").exists());
    drop(edit);
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!repo.git_dir().join("index.lock").exists());
}
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn foreign_lock_is_never_removed(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    let path = repo.git_dir().join("index.lock");
    fs::write(&path, b"foreign").unwrap();
    assert!(matches!(
        repo.edit_index(Limits::default()),
        Err(StorageError::Locked(_))
    ));
    assert_eq!(fs::read(path).unwrap(), b"foreign");
}
#[rstest]
#[case::created_sha1(crate::ObjectFormat::Sha1, false)]
#[case::created_sha256(crate::ObjectFormat::Sha256, false)]
#[case::modified_sha1(crate::ObjectFormat::Sha1, true)]
#[case::modified_sha256(crate::ObjectFormat::Sha256, true)]
fn detects_noncooperating_writer(#[case] format: crate::ObjectFormat, #[case] existing: bool) {
    let (_root, repo) = repository(format);
    let before = existing.then(|| populated(&repo));
    let edit = repo.edit_index(Limits::default()).unwrap();
    fs::write(repo.git_dir().join("index"), b"concurrent").unwrap();
    assert!(matches!(edit.commit(), Err(StorageError::Changed(_))));
    assert_eq!(
        fs::read(repo.git_dir().join("index")).unwrap(),
        b"concurrent"
    );
    assert!(!repo.git_dir().join("index.lock").exists());
    drop(before);
}
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn detects_deleted_original(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    populated(&repo);
    let edit = repo.edit_index(Limits::default()).unwrap();
    fs::remove_file(repo.git_dir().join("index")).unwrap();
    assert!(matches!(edit.commit(), Err(StorageError::Changed(_))));
    assert!(!repo.git_dir().join("index").exists());
}
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn invalid_read_releases_lock(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    fs::write(repo.git_dir().join("index"), b"broken").unwrap();
    assert!(matches!(
        repo.edit_index(Limits::default()),
        Err(StorageError::Format { .. })
    ));
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), b"broken");
    assert!(!repo.git_dir().join("index.lock").exists());
}
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn invalid_edit_preserves_snapshot_and_storage(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    let before = populated(&repo);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    assert!(
        edit.replace_entries(vec![Entry::new(
            b"../a".to_vec(),
            Mode::Regular,
            ObjectId::for_blob(repo.object_format(), b"a")
        )])
        .is_err()
    );
    assert_eq!(edit.index().entries()[0].path, b"a");
    drop(edit);
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
}
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn encoding_failure_preserves_original(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    let before = populated(&repo);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    // Inject an encode-only limit failure after the valid snapshot was read.
    edit.limits.max_bytes = 0;
    assert!(matches!(
        edit.commit(),
        Err(StorageError::Format {
            source: Error::Limit(_),
            ..
        })
    ));
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!repo.git_dir().join("index.lock").exists());
}
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn write_failure_preserves_original(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    let before = populated(&repo);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    // A read-only descriptor injects failure without permission/root assumptions.
    drop(edit.file.take());
    edit.file = Some(File::open(&edit.lock_path).unwrap());
    assert!(matches!(
        edit.commit(),
        Err(StorageError::Io {
            operation: "write lock",
            ..
        })
    ));
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!repo.git_dir().join("index.lock").exists());
}
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn rename_failure_preserves_original(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    let before = populated(&repo);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    let error = edit.publish_with_rename(|_, _| Err(io::Error::other("injected rename failure")));
    assert!(matches!(
        error,
        Err(StorageError::Io {
            operation: "replace index",
            ..
        })
    ));
    edit.abort().unwrap();
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!repo.git_dir().join("index.lock").exists());
}
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn bounded_read_preserves_storage(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    let before = populated(&repo);
    let limits = Limits {
        max_bytes: before.len() - 1,
        ..Limits::default()
    };
    assert!(matches!(
        repo.edit_index(limits),
        Err(StorageError::Format {
            source: Error::Limit("bytes"),
            ..
        })
    ));
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!repo.git_dir().join("index.lock").exists());
}
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn directory_is_not_a_missing_index(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    fs::create_dir(repo.git_dir().join("index")).unwrap();
    assert!(matches!(
        repo.read_index(Limits::default()),
        Err(StorageError::NotRegular(_))
    ));
}
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn publication_preserves_stat_words_and_invalidates_timestamp_trust(
    #[case] format: crate::ObjectFormat,
) {
    let (_root, repo) = repository(format);
    let stat = Stat {
        mtime: Timestamp {
            seconds: 42,
            nanoseconds: 123,
        },
        size: 17,
        ..Stat::default()
    };
    let mut entry = Entry::new(
        b"a".to_vec(),
        Mode::Regular,
        ObjectId::for_blob(repo.object_format(), b"a"),
    );
    entry.stat = stat;
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    edit.replace_entries(vec![entry]).unwrap();
    edit.commit().unwrap();
    assert_eq!(
        repo.read_index(Limits::default())
            .unwrap()
            .unwrap()
            .entries()[0]
            .stat,
        stat
    );
    assert_eq!(
        fs::metadata(repo.git_dir().join("index"))
            .unwrap()
            .modified()
            .unwrap(),
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)
    );
}

#[cfg(unix)]
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn symlink_index_is_rejected_without_following_it(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    let target = repo.git_dir().join("other");
    fs::write(&target, b"keep").unwrap();
    std::os::unix::fs::symlink(&target, repo.git_dir().join("index")).unwrap();
    assert!(matches!(
        repo.edit_index(Limits::default()),
        Err(StorageError::NotRegular(_))
    ));
    assert_eq!(fs::read(target).unwrap(), b"keep");
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[cfg(unix)]
#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn explicit_abort_does_not_remove_replaced_lock(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    let edit = repo.edit_index(Limits::default()).unwrap();
    fs::rename(
        repo.git_dir().join("index.lock"),
        repo.git_dir().join("owned.lock"),
    )
    .unwrap();
    fs::write(repo.git_dir().join("index.lock"), b"foreign").unwrap();
    assert!(matches!(edit.abort(), Err(StorageError::Changed(_))));
    assert_eq!(
        fs::read(repo.git_dir().join("index.lock")).unwrap(),
        b"foreign"
    );
}

#[rstest]
#[case::v3_sha1(crate::ObjectFormat::Sha1, crate::index::Version::V3)]
#[case::v3_sha256(crate::ObjectFormat::Sha256, crate::index::Version::V3)]
#[case::v4_sha1(crate::ObjectFormat::Sha1, crate::index::Version::V4)]
#[case::v4_sha256(crate::ObjectFormat::Sha256, crate::index::Version::V4)]
fn versioned_publication_fault_and_race_preserve_bytes(
    #[case] format: crate::ObjectFormat,
    #[case] version: crate::index::Version,
) {
    let (_root, repo) = repository(format);
    populated(&repo);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    let mut entries = edit.index().entries().to_vec();
    entries[0].intent_to_add = true;
    edit.replace_entries(entries).unwrap();
    edit.set_version(version).unwrap();
    edit.commit().unwrap();
    let before = fs::read(repo.git_dir().join("index")).unwrap();

    let mut edit = repo.edit_index(Limits::default()).unwrap();
    edit.replace_entries(vec![]).unwrap();
    assert!(
        edit.publish_with_rename(|_, _| Err(io::Error::other("injected")))
            .is_err()
    );
    edit.abort().unwrap();
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);

    let mut edit = repo.edit_index(Limits::default()).unwrap();
    edit.replace_entries(vec![]).unwrap();
    fs::write(repo.git_dir().join("index"), b"independent writer").unwrap();
    assert!(matches!(edit.commit(), Err(StorageError::Changed(_))));
    assert_eq!(
        fs::read(repo.git_dir().join("index")).unwrap(),
        b"independent writer"
    );
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[rstest]
#[case::same(0, 17)]
#[case::path(1, 0)]
#[case::stage(2, 0)]
#[case::mode(3, 0)]
#[case::id(4, 0)]
#[case::assume_valid(5, 0)]
#[case::intent(6, 0)]
#[case::skip(7, 0)]
fn stat_reuse_requires_identical_entry(#[case] difference: u8, #[case] expected_size: u32) {
    let (_root, repo) = repository(crate::ObjectFormat::Sha1);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    let mut original = Entry::new(
        b"a".to_vec(),
        Mode::Regular,
        ObjectId::for_blob(repo.object_format(), b"a"),
    );
    original.stat.size = 17;
    edit.replace_entries(vec![original.clone()]).unwrap();
    let draft = differing_entry(original.clone(), difference);
    edit.replace_entries_reusing_stat(vec![draft]).unwrap();
    assert_eq!(edit.index().entries()[0].stat.size, expected_size);
}

fn differing_entry(mut entry: Entry, difference: u8) -> Entry {
    entry.stat = Stat::default();
    match difference {
        0 => {}
        1 => entry.path = b"b".to_vec(),
        2 => entry.stage = crate::index::Stage::Ours,
        3 => entry.mode = Mode::Executable,
        4 => entry.id = ObjectId::for_blob(entry.id.format(), b"different"),
        5 => entry.assume_valid = true,
        6 => entry.intent_to_add = true,
        7 => entry.skip_worktree = true,
        _ => unreachable!(),
    }
    entry
}
