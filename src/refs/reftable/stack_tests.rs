#[cfg(unix)]
use std::collections::BTreeSet;
use std::fs;
#[cfg(unix)]
use std::io;
use std::sync::atomic::AtomicBool;

use rstest::rstest;

use super::decode_tests::{fixture, git};
#[cfg(unix)]
use super::stack::{ExpireStackError, expire_logs, expire_logs_with_sync};
use super::{Snapshot, StackLimits, compact};
use crate::ObjectFormat;
use crate::refs::ReferenceError;

#[cfg(unix)]
#[test]
fn expiry_directory_sync_failure_keeps_old_tables_after_visible_list() {
    let root = fixture("sha1");
    let directory = root.path().join(".git/reftable");
    let before = Snapshot::read(
        &directory,
        ObjectFormat::Sha1,
        StackLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let old_list = fs::read(directory.join("tables.list")).unwrap();
    let log = before
        .table
        .logs
        .iter()
        .find(|record| record.value.is_some())
        .unwrap();
    let keys = BTreeSet::from([(log.name.clone(), log.update_index)]);
    let refs = git(root.path(), &["show-ref", "--head"]);
    let mut syncs = 0;
    let error = expire_logs_with_sync(
        &directory,
        ObjectFormat::Sha1,
        StackLimits::default(),
        &before,
        &keys,
        &AtomicBool::new(false),
        |path| {
            syncs += 1;
            if syncs == 2 {
                return Err(ReferenceError::Io {
                    path: path.to_path_buf(),
                    source: io::Error::from_raw_os_error(28),
                });
            }
            fs::File::open(path)
                .and_then(|file| file.sync_all())
                .map_err(|source| ReferenceError::Io {
                    path: path.to_path_buf(),
                    source,
                })
        },
    );
    assert!(matches!(
        error,
        Err(ExpireStackError::Failed {
            publication_uncertain: true,
            durable: false,
            ..
        })
    ));
    assert_ne!(fs::read(directory.join("tables.list")).unwrap(), old_list);
    for name in &before.names {
        assert!(directory.join(name).exists());
    }
    assert_eq!(git(root.path(), &["show-ref", "--head"]), refs);
}

#[cfg(unix)]
#[test]
fn expiry_rejects_changed_stack_before_publication() {
    let root = fixture("sha1");
    let directory = root.path().join(".git/reftable");
    let before = Snapshot::read(
        &directory,
        ObjectFormat::Sha1,
        StackLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let log = before
        .table
        .logs
        .iter()
        .find(|record| record.value.is_some())
        .unwrap();
    let keys = BTreeSet::from([(log.name.clone(), log.update_index)]);
    git(root.path(), &["commit", "--allow-empty", "-m", "next"]);
    let list = fs::read(directory.join("tables.list")).unwrap();
    assert!(matches!(
        expire_logs(
            &directory,
            ObjectFormat::Sha1,
            StackLimits::default(),
            &before,
            &keys,
            &AtomicBool::new(false),
        ),
        Err(ExpireStackError::Changed)
    ));
    assert_eq!(fs::read(directory.join("tables.list")).unwrap(), list);
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn compaction_preserves_git_refs_logs_and_owned_snapshot(
    #[case] spelling: &str,
    #[case] format: ObjectFormat,
) {
    let root = fixture(spelling);
    git(root.path(), &["config", "reftable.autoCompact", "false"]);
    git(root.path(), &["commit", "--allow-empty", "-m", "second"]);
    git(root.path(), &["update-ref", "refs/tags/deleted", "HEAD"]);
    git(root.path(), &["update-ref", "-d", "refs/tags/deleted"]);
    let before = git(root.path(), &["show-ref", "--head"]);
    let log = git(
        root.path(),
        &["reflog", "show", "--format=%H %gn %gs", "HEAD"],
    );
    let directory = root.path().join(".git/reftable");
    let old = Snapshot::read(
        &directory,
        format,
        StackLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let report = compact(
        &directory,
        format,
        StackLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(report.input_tables > 1);
    assert!(report.retained.is_empty());
    assert_eq!(git(root.path(), &["show-ref", "--head"]), before);
    assert_eq!(
        git(
            root.path(),
            &["reflog", "show", "--format=%H %gn %gs", "HEAD"]
        ),
        log
    );
    git(root.path(), &["refs", "verify"]);
    let new = Snapshot::read(
        &directory,
        format,
        StackLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let retained: Vec<_> = old
        .table
        .references
        .into_iter()
        .filter(|record| record.target.is_some())
        .collect();
    assert_eq!(new.table.references, retained);
}

#[test]
fn stack_cancellation_precedes_lock_creation() {
    let directory = tempfile::tempdir().unwrap();
    assert!(matches!(
        compact(
            directory.path(),
            ObjectFormat::Sha1,
            StackLimits::default(),
            &AtomicBool::new(true)
        ),
        Err(ReferenceError::Cancelled)
    ));
    assert!(!directory.path().join("tables.list.lock").exists());
}

#[rstest]
#[case::parent(b"../outside.ref\n")]
#[case::absolute(b"/outside.ref\n")]
#[case::duplicate(b"one.ref\none.ref\n")]
#[case::truncated(b"one.ref")]
#[case::empty(b"\n")]
#[case::backslash(b"dir\\one.ref\n")]
fn rejects_malformed_list_before_opening_tables(#[case] bytes: &[u8]) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("tables.list"), bytes).unwrap();
    assert!(matches!(
        Snapshot::read(
            directory.path(),
            ObjectFormat::Sha1,
            StackLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(ReferenceError::Malformed { .. })
    ));
}

#[test]
fn compaction_never_steals_stack_lock() {
    let root = fixture("sha1");
    let directory = root.path().join(".git/reftable");
    let before = fs::read(directory.join("tables.list")).unwrap();
    fs::write(directory.join("tables.list.lock"), b"other writer").unwrap();
    assert!(matches!(
        compact(
            &directory,
            ObjectFormat::Sha1,
            StackLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(ReferenceError::Locked(_))
    ));
    assert_eq!(fs::read(directory.join("tables.list")).unwrap(), before);
    assert_eq!(
        fs::read(directory.join("tables.list.lock")).unwrap(),
        b"other writer"
    );
}

#[test]
fn interrupted_list_publication_preserves_old_stack_and_leaves_unlisted_table() {
    use crate::refs::store::Lock;
    let root = fixture("sha1");
    let directory = root.path().join(".git/reftable").canonicalize().unwrap();
    let before = fs::read(directory.join("tables.list")).unwrap();
    let snapshot = Snapshot::read(
        &directory,
        ObjectFormat::Sha1,
        StackLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let lock = Lock::acquire(directory.join("tables.list")).unwrap();
    let bytes = snapshot.table.encode(super::Limits::default()).unwrap();
    let count = fs::read_dir(&directory).unwrap().count();
    assert!(
        super::stack::publish_with(
            &lock,
            &snapshot.names,
            &snapshot.table,
            &bytes,
            StackLimits::default(),
            || Err(std::io::Error::other("injected before list replacement"))
        )
        .is_err()
    );
    assert_eq!(fs::read(directory.join("tables.list")).unwrap(), before);
    assert_eq!(fs::read_dir(&directory).unwrap().count(), count + 1);
    drop(lock);
    let after = Snapshot::read(
        &directory,
        ObjectFormat::Sha1,
        StackLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(after.table, snapshot.table);
}

#[test]
fn compaction_respects_existing_compactor_table_lock() {
    let root = fixture("sha1");
    let directory = root.path().join(".git/reftable");
    let before = fs::read_to_string(directory.join("tables.list")).unwrap();
    let lock = directory.join(format!("{}.lock", before.lines().next().unwrap()));
    fs::write(&lock, b"other compactor").unwrap();
    assert!(matches!(
        compact(
            &directory,
            ObjectFormat::Sha1,
            StackLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(ReferenceError::Locked(_))
    ));
    assert_eq!(
        fs::read_to_string(directory.join("tables.list")).unwrap(),
        before
    );
    assert_eq!(fs::read(lock).unwrap(), b"other compactor");
    assert!(!directory.join("tables.list.lock").exists());
}
