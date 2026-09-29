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

fn add_extension(repo: &Repository, signature: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut bytes = populated(repo);
    bytes.truncate(bytes.len() - repo.object_format().digest_len());
    bytes.extend_from_slice(signature);
    bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
    bytes.extend_from_slice(data);
    let checksum = repo.object_format().checksum(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    fs::write(repo.git_dir().join("index"), &bytes).unwrap();
    bytes
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn invalidates_tree_cache_without_changing_entries(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    let before = add_extension(&repo, b"TREE", b"cached tree");
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    let entries = edit.index().entries().to_vec();
    edit.replace_entries(entries.clone()).unwrap();
    edit.invalidate_tree_cache().unwrap();
    assert!(edit.index().extensions().is_empty());
    edit.commit().unwrap();

    let after = fs::read(repo.git_dir().join("index")).unwrap();
    assert_ne!(after, before);
    let index = repo.read_index(Limits::default()).unwrap().unwrap();
    assert_eq!(index.entries(), entries);
    assert!(index.extensions().is_empty());
}

#[rstest]
#[case::unknown(*b"TEST", b"opaque".as_slice())]
#[case::split(*b"link", &[0; 20])]
fn tree_invalidation_rejects_other_extensions_without_changes(
    #[case] signature: [u8; 4],
    #[case] data: &[u8],
) {
    let (_root, repo) = repository(crate::ObjectFormat::Sha1);
    let before = add_extension(&repo, &signature, data);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    assert_eq!(
        edit.invalidate_tree_cache(),
        Err(Error::ExtensionPreventsEdit(signature))
    );
    assert_eq!(edit.index().extensions()[0].signature(), signature);
    edit.abort().unwrap();
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!repo.git_dir().join("index.lock").exists());
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
fn conditional_commit_creates_missing_index(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    let path = repo.git_dir().join("index");
    let edit = repo.edit_index(Limits::default()).unwrap();
    assert!(edit.original_missing());
    edit.commit_new_with_options(IndexCommitOptions::default())
        .unwrap();
    assert!(Index::parse(format, &fs::read(&path).unwrap(), Limits::default()).is_ok());
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn conditional_commit_preserves_foreign_index(#[case] format: crate::ObjectFormat) {
    let (_root, repo) = repository(format);
    let path = repo.git_dir().join("index");
    let edit = repo.edit_index(Limits::default()).unwrap();
    fs::write(&path, b"foreign").unwrap();
    assert!(matches!(
        edit.commit_new_with_options(IndexCommitOptions::default()),
        Err(StorageError::Changed(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), b"foreign");
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn conditional_publication_does_not_replace_last_moment_writer(
    #[case] format: crate::ObjectFormat,
) {
    let (_root, repo) = repository(format);
    let path = repo.git_dir().join("index");
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    let error = edit.publish_with_policy(
        IndexCommitOptions::default(),
        crate::file_policy::sync_file,
        |lock, selected| {
            fs::write(selected, b"last moment writer")?;
            fs::hard_link(lock, selected)
        },
    );
    assert!(matches!(error, Err(StorageError::Io { .. })));
    edit.abort().unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"last moment writer");
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn conditional_publication_reports_post_install_cleanup_failure(
    #[case] format: crate::ObjectFormat,
) {
    let (_root, repo) = repository(format);
    let path = repo.git_dir().join("index");
    let lock = repo.git_dir().join("index.lock");
    let edit = repo.edit_index(Limits::default()).unwrap();
    let error = edit.commit_new_with_cleanup(IndexCommitOptions::default(), |_| {
        Err(io::Error::other("injected cleanup failure"))
    });
    assert!(matches!(
        error,
        Err(StorageError::PublishedCleanup {
            path: published,
            lock: retained,
            ..
        }) if published == path && retained == lock
    ));
    assert!(Index::parse(format, &fs::read(&path).unwrap(), Limits::default()).is_ok());
    assert!(lock.exists());
    fs::remove_file(lock).unwrap();
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

fn specialized_index(repo: &Repository, sparse: bool) -> Vec<u8> {
    let format = repo.object_format();
    if sparse {
        let mut entry = Entry::new(
            b"dir/".to_vec(),
            Mode::SparseDirectory,
            ObjectId::null(format),
        );
        entry.skip_worktree = true;
        let mut edit = repo.edit_index(Limits::default()).unwrap();
        edit.replace_entries(vec![entry]).unwrap();
        edit.commit().unwrap();
        return fs::read(repo.git_dir().join("index")).unwrap();
    }
    let shared = populated(repo);
    let hash = &shared[shared.len() - format.digest_len()..];
    let id = ObjectId::from_bytes(format, hash).unwrap();
    fs::write(
        repo.git_dir().join(format!("sharedindex.{id}")),
        shared.clone(),
    )
    .unwrap();
    let mut bytes = b"DIRC\0\0\0\x02\0\0\0\0link".to_vec();
    bytes.extend_from_slice(&((format.digest_len() + 40) as u32).to_be_bytes());
    bytes.extend_from_slice(hash);
    // Two original empty EWAH bitmaps: bit count, word count, one run word, run pointer.
    bytes.extend_from_slice(b"\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\0");
    bytes.extend_from_slice(b"\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\0");
    bytes.extend_from_slice(format.checksum(&bytes).as_bytes());
    fs::write(repo.git_dir().join("index"), &bytes).unwrap();
    bytes
}

#[rstest]
#[case::split_sha1(crate::ObjectFormat::Sha1, false)]
#[case::split_sha256(crate::ObjectFormat::Sha256, false)]
#[case::sparse_sha1(crate::ObjectFormat::Sha1, true)]
#[case::sparse_sha256(crate::ObjectFormat::Sha256, true)]
fn specialized_publication_faults_and_recovery(
    #[case] format: crate::ObjectFormat,
    #[case] sparse: bool,
) {
    let (_root, repo) = repository(format);
    let before = specialized_index(&repo, sparse);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    edit.replace_entries(vec![]).unwrap();
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
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    edit.replace_entries(vec![]).unwrap();
    assert!(
        edit.publish_with_rename(|_, _| Err(io::Error::other("injected rename")))
            .is_err()
    );
    edit.abort().unwrap();
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    // A fresh guard can recover after inspecting the failed attempt's unchanged destination.
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    edit.replace_entries(vec![]).unwrap();
    edit.commit().unwrap();
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
#[case::split_sha1(crate::ObjectFormat::Sha1, false)]
#[case::split_sha256(crate::ObjectFormat::Sha256, false)]
#[case::sparse_sha1(crate::ObjectFormat::Sha1, true)]
#[case::sparse_sha256(crate::ObjectFormat::Sha256, true)]
fn specialized_publication_preserves_concurrent_main(
    #[case] format: crate::ObjectFormat,
    #[case] sparse: bool,
) {
    let (_root, repo) = repository(format);
    specialized_index(&repo, sparse);
    let edit = repo.edit_index(Limits::default()).unwrap();
    fs::write(repo.git_dir().join("index"), b"concurrent replacement").unwrap();
    assert!(matches!(edit.commit(), Err(StorageError::Changed(_))));
    assert_eq!(
        fs::read(repo.git_dir().join("index")).unwrap(),
        b"concurrent replacement"
    );
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[rstest]
fn alternate_absent_index_publication_preserves_default(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
) {
    let (root, repo) = repository(format);
    let default = populated(&repo);
    let alternate = root.path().join("alternate.index");
    assert!(
        repo.read_index_at(&alternate, Limits::default())
            .unwrap()
            .is_none()
    );
    let mut edit = repo.edit_index_at(&alternate, Limits::default()).unwrap();
    assert!(edit.index().entries().is_empty());
    assert!(!alternate.exists());
    assert!(root.path().join("alternate.index.lock").is_file());
    edit.replace_entries(Vec::new()).unwrap();
    edit.commit().unwrap();
    assert!(
        repo.read_index_at(&alternate, Limits::default())
            .unwrap()
            .unwrap()
            .entries()
            .is_empty()
    );
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), default);
    assert!(!root.path().join("alternate.index.lock").exists());
}

#[rstest]
fn alternate_existing_index_noop_preserves_encoding(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
) {
    let (root, repo) = repository(format);
    let original = add_extension(&repo, b"TREE", b"cached tree");
    let alternate = root.path().join("alternate");
    fs::write(&alternate, &original).unwrap();
    let index = repo
        .read_index_at(&alternate, Limits::default())
        .unwrap()
        .unwrap();
    let edit = repo.edit_index_at(&alternate, Limits::default()).unwrap();
    assert_eq!(edit.index().entries(), index.entries());
    edit.commit().unwrap();
    assert_eq!(fs::read(&alternate).unwrap(), original);
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), original);
}

#[rstest]
fn alternate_index_contention_and_changed_snapshot_preserve_storage(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
) {
    let (root, repo) = repository(format);
    let original = populated(&repo);
    let alternate = root.path().join("alternate");
    fs::write(&alternate, &original).unwrap();
    let edit = repo.edit_index_at(&alternate, Limits::default()).unwrap();
    assert!(matches!(
        repo.edit_index_at(&alternate, Limits::default()),
        Err(StorageError::Locked(_))
    ));
    let replacement = Index::empty(format).encode(Limits::default()).unwrap();
    fs::write(&alternate, &replacement).unwrap();
    assert!(matches!(edit.commit(), Err(StorageError::Changed(_))));
    assert_eq!(fs::read(&alternate).unwrap(), replacement);
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), original);
    assert!(!root.path().join("alternate.lock").exists());
}

#[cfg(unix)]
#[rstest]
fn alternate_replaced_lock_is_not_published_or_removed(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
) {
    let (root, repo) = repository(format);
    let alternate = root.path().join("alternate");
    let edit = repo.edit_index_at(&alternate, Limits::default()).unwrap();
    let lock = root.path().join("alternate.lock");
    fs::rename(&lock, root.path().join("owned-lock")).unwrap();
    fs::write(&lock, b"replacement").unwrap();
    assert!(edit.commit().is_err());
    assert_eq!(fs::read(&lock).unwrap(), b"replacement");
    assert!(!alternate.exists());
}

#[rstest]
fn alternate_index_directory_is_rejected_and_lock_released(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
) {
    let (root, repo) = repository(format);
    let alternate = root.path().join("alternate");
    fs::create_dir(&alternate).unwrap();
    assert!(matches!(
        repo.read_index_at(&alternate, Limits::default()),
        Err(StorageError::NotRegular(_))
    ));
    assert!(matches!(
        repo.edit_index_at(&alternate, Limits::default()),
        Err(StorageError::NotRegular(_))
    ));
    assert!(!root.path().join("alternate.lock").exists());
}

#[cfg(unix)]
#[rstest]
fn alternate_index_leaf_symlink_is_rejected_and_ancestor_alias_is_preserved(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
) {
    let (root, repo) = repository(format);
    let original = populated(&repo);
    let leaf = root.path().join("leaf");
    std::os::unix::fs::symlink(repo.git_dir().join("index"), &leaf).unwrap();
    assert!(matches!(
        repo.read_index_at(&leaf, Limits::default()),
        Err(StorageError::NotRegular(_))
    ));
    assert!(matches!(
        repo.edit_index_at(&leaf, Limits::default()),
        Err(StorageError::NotRegular(_))
    ));
    assert_eq!(fs::read(&leaf).unwrap(), original);
    assert!(!root.path().join("leaf.lock").exists());
    let ancestor = root.path().join("ancestor");
    std::os::unix::fs::symlink(repo.git_dir(), &ancestor).unwrap();
    let path = ancestor.join("other.index");
    let edit = repo.edit_index_at(&path, Limits::default()).unwrap();
    assert_eq!(edit.destination, path);
    edit.commit().unwrap();
    assert!(repo.git_dir().join("other.index").is_file());
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), original);
}

#[rstest]
fn explicit_index_rejects_null_identity_split_without_dependency_reads(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
) {
    let (root, repo) = repository(format);
    let original = add_extension(&repo, b"link", &vec![0; format.digest_len()]);
    let alternate = root.path().join("alternate");
    fs::write(&alternate, &original).unwrap();
    assert!(repo.read_index(Limits::default()).is_ok());
    assert!(
        matches!(repo.read_index_at(&alternate, Limits::default()), Err(StorageError::Format { source: Error::MandatoryExtension(signature), .. }) if signature == *b"link")
    );
    assert!(
        matches!(repo.edit_index_at(&alternate, Limits::default()), Err(StorageError::Format { source: Error::MandatoryExtension(signature), .. }) if signature == *b"link")
    );
    assert_eq!(fs::read(&alternate).unwrap(), original);
    assert!(!root.path().join("alternate.lock").exists());
}

#[test]
fn alternate_relative_path_uses_current_directory_once() {
    let (_root, repo) = repository(crate::ObjectFormat::Sha1);
    let relative_directory = tempfile::Builder::new()
        .prefix("alternate-index-test-")
        .tempdir_in(".")
        .unwrap();
    let relative = relative_directory.path().join("index");
    let edit = repo.edit_index_at(&relative, Limits::default()).unwrap();
    assert_eq!(edit.destination, std::path::absolute(&relative).unwrap());
    edit.commit().unwrap();
    assert!(relative.is_file());
    assert!(
        repo.read_index_at(&relative, Limits::default())
            .unwrap()
            .is_some()
    );
    assert!(repo.read_index(Limits::default()).unwrap().is_none());
}

#[rstest]
fn discard_resolve_undo_requires_offsets_first_and_explicit_commit(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
) {
    let (_root, repo) = repository(format);
    let mut bytes = add_extension(&repo, b"TREE", b"opaque tree");
    bytes.truncate(bytes.len() - format.digest_len());
    bytes.extend_from_slice(b"REUC\0\0\0\x03badEOIE\0\0\0\x00IEOT\0\0\0\x00");
    bytes.extend_from_slice(format.checksum(&bytes).as_bytes());
    let path = repo.git_dir().join("index");
    fs::write(&path, &bytes).unwrap();
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    let before = edit.index().clone();
    assert_eq!(
        edit.discard_resolve_undo(),
        Err(Error::ExtensionPreventsEdit(*b"EOIE"))
    );
    assert_eq!(edit.index(), &before);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    edit.invalidate_entry_offsets().unwrap();
    edit.discard_resolve_undo().unwrap();
    assert_eq!(edit.index().entries(), before.entries());
    assert_eq!(edit.index().extensions().len(), 1);
    assert_eq!(edit.index().extensions()[0].signature(), *b"TREE");
    assert_eq!(edit.index().extensions()[0].data(), b"opaque tree");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    edit.commit().unwrap();
    let result = repo.read_index(Limits::default()).unwrap().unwrap();
    assert_eq!(result.entries(), before.entries());
    assert_eq!(result.extensions()[0].data(), b"opaque tree");
    assert_eq!(result.extensions().len(), 1);
}
