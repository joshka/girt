use rstest::rstest;

use super::*;
use crate::index::{Mode, Stat, Timestamp};
use crate::{InitKind, ObjectId};

fn repository() -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(root.path().join("repo"), InitKind::Worktree).unwrap();
    (root, repo)
}
fn populated(repo: &Repository) -> Vec<u8> {
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    edit.replace_entries(vec![Entry::new(
        b"a".to_vec(),
        Mode::Regular,
        ObjectId::for_blob(crate::ObjectFormat::Sha1, b"a"),
    )])
    .unwrap();
    edit.commit().unwrap();
    fs::read(repo.git_dir().join("index")).unwrap()
}
#[test]
fn absent_is_distinct_from_published_empty() {
    let (_root, repo) = repository();
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
#[test]
fn lock_contention_and_drop_preserve_original() {
    let (_root, repo) = repository();
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
#[test]
fn foreign_lock_is_never_removed() {
    let (_root, repo) = repository();
    let path = repo.git_dir().join("index.lock");
    fs::write(&path, b"foreign").unwrap();
    assert!(matches!(
        repo.edit_index(Limits::default()),
        Err(StorageError::Locked(_))
    ));
    assert_eq!(fs::read(path).unwrap(), b"foreign");
}
#[rstest]
#[case::created(false)]
#[case::modified(true)]
fn detects_noncooperating_writer(#[case] existing: bool) {
    let (_root, repo) = repository();
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
#[test]
fn detects_deleted_original() {
    let (_root, repo) = repository();
    populated(&repo);
    let edit = repo.edit_index(Limits::default()).unwrap();
    fs::remove_file(repo.git_dir().join("index")).unwrap();
    assert!(matches!(edit.commit(), Err(StorageError::Changed(_))));
    assert!(!repo.git_dir().join("index").exists());
}
#[test]
fn invalid_read_releases_lock() {
    let (_root, repo) = repository();
    fs::write(repo.git_dir().join("index"), b"broken").unwrap();
    assert!(matches!(
        repo.edit_index(Limits::default()),
        Err(StorageError::Format { .. })
    ));
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), b"broken");
    assert!(!repo.git_dir().join("index.lock").exists());
}
#[test]
fn invalid_edit_preserves_snapshot_and_storage() {
    let (_root, repo) = repository();
    let before = populated(&repo);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    assert!(
        edit.replace_entries(vec![Entry::new(
            b"../a".to_vec(),
            Mode::Regular,
            ObjectId::for_blob(crate::ObjectFormat::Sha1, b"a")
        )])
        .is_err()
    );
    assert_eq!(edit.index().entries()[0].path, b"a");
    drop(edit);
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
}
#[test]
fn encoding_failure_preserves_original() {
    let (_root, repo) = repository();
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
#[test]
fn write_failure_preserves_original() {
    let (_root, repo) = repository();
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
#[test]
fn rename_failure_preserves_original() {
    let (_root, repo) = repository();
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
#[test]
fn bounded_read_preserves_storage() {
    let (_root, repo) = repository();
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
#[test]
fn directory_is_not_a_missing_index() {
    let (_root, repo) = repository();
    fs::create_dir(repo.git_dir().join("index")).unwrap();
    assert!(matches!(
        repo.read_index(Limits::default()),
        Err(StorageError::NotRegular(_))
    ));
}
#[test]
fn publication_preserves_stat_words_and_invalidates_timestamp_trust() {
    let (_root, repo) = repository();
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
        ObjectId::for_blob(crate::ObjectFormat::Sha1, b"a"),
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
#[test]
fn symlink_index_is_rejected_without_following_it() {
    let (_root, repo) = repository();
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
#[test]
fn explicit_abort_does_not_remove_replaced_lock() {
    let (_root, repo) = repository();
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
