use std::fs;
use std::sync::atomic::AtomicBool;

use rstest::rstest;

use super::decode_tests::{fixture, git};
use super::{Snapshot, StackLimits, compact};
use crate::ObjectFormat;
use crate::refs::ReferenceError;

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
