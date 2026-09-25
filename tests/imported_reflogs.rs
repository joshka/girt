//! Original Git-generated history and independent mutations of its files and binary records.
#[path = "support/layout_git.rs"]
mod git;

use std::fs;
use std::sync::atomic::AtomicBool;

use girt::refs::{
    Expected, ImportedRecord, RefEdit, RefName, Reflog, ReflogLimits, ReflogReadEnd, Target,
};
use girt::{ObjectFormat, ObjectId, Repository, Signature};
use rstest::rstest;

fn fixture(format: ObjectFormat, backend: &str) -> (tempfile::TempDir, Repository, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    git::git(
        root.path(),
        &[
            "init",
            "--initial-branch=main",
            &format!("--object-format={format}"),
            &format!("--ref-format={backend}"),
        ],
        b"",
    );
    git::git(
        root.path(),
        &["commit", "--allow-empty", "-m", "original"],
        b"",
    );
    let tip = git::git(root.path(), &["rev-parse", "HEAD"], b"");
    let tip = ObjectId::from_hex(format, std::str::from_utf8(&tip).unwrap().trim()).unwrap();
    let repo = Repository::open(root.path()).unwrap();
    (root, repo, tip)
}
fn head() -> RefName {
    RefName::new("HEAD").unwrap()
}
fn data(tip: ObjectId, tail: &[u8]) -> Vec<u8> {
    [
        format!("{} {tip} ", ObjectId::null(tip.format())).as_bytes(),
        tail,
    ]
    .concat()
}
fn append(tip: ObjectId, message: &[u8]) -> RefEdit {
    RefEdit {
        name: head(),
        dereference: false,
        target: Some(Target::Direct(tip)),
        expected: Expected::Exists,
        reflog: Reflog::Append {
            committer: Signature {
                name: b"A".to_vec(),
                email: b"a@b".to_vec(),
                seconds: 1700000000,
                offset_minutes: 0,
            },
            message: message.to_vec(),
        },
    }
}

#[rstest]
#[case::cr(b"A <a@b> 1 +0000\ta\rb\n", b"a\rb\n", true)]
#[case::nul(b"A <a@b> 1 +0000\ta\0b\n", b"\n", true)]
#[case::short(b"A <a@b> 1 +01\tfixture\n", b"", false)]
#[case::short_long_date(b"A <a@b> 1700000000 +01\tfixture\n", b"", false)]
#[case::suffix(b"A <a@b> 1 +0000suffix\tfixture\n", b"suffix\tfixture\n", true)]
#[case::overflow(b"A <a@b> 9223372036854775808 +0000\tfixture\n", b"fixture\n", true)]
#[case::unterminated(b"A <a@b> 1 +0000\tfixture", b"", false)]
#[case::unterminated_long_date(b"A <a@b> 1700000000 +0000\tfixture", b"", false)]
fn exact_import_and_detached_git_display(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] tail: &[u8],
    #[case] shown: &[u8],
    #[case] complete: bool,
) {
    let (root, repo, tip) = fixture(format, "files");
    // A detached HEAD prevents Git's symbolic-HEAD fallback from selecting the branch log.
    fs::write(repo.git_dir().join("HEAD"), format!("{tip}\n")).unwrap();
    let raw = data(tip, tail);
    fs::write(repo.git_dir().join("logs/HEAD"), &raw).unwrap();
    assert_eq!(
        git::git(
            root.path(),
            &["reflog", "show", "--format=%gs", "HEAD"],
            b""
        ),
        shown
    );
    let log = repo
        .references()
        .unwrap()
        .imported_reflog(&head(), ReflogLimits::default(), &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    assert_eq!(log.is_complete(), complete);
    assert_eq!(
        log.records(),
        &[ImportedRecord::File { format, bytes: raw }]
    );
    assert_eq!(log.recoverable_roots().collect::<Vec<_>>(), [tip]);
}

#[rstest]
#[case::short(b"A <a@b> 1 +0\tfixture\n")]
#[case::unterminated(b"A <a@b> 1 +0000\tfixture")]
fn r11_symbolic_head_display_is_branch_fallback(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] tail: &[u8],
) {
    let (root, repo, tip) = fixture(format, "files");
    let branch = git::git(
        root.path(),
        &["reflog", "show", "--format=%H %gn %gs", "refs/heads/main"],
        b"",
    );
    fs::write(repo.git_dir().join("logs/HEAD"), data(tip, tail)).unwrap();
    assert_eq!(
        git::git(
            root.path(),
            &["reflog", "show", "--format=%H %gn %gs", "HEAD"],
            b""
        ),
        branch
    );
    fs::write(repo.git_dir().join("HEAD"), format!("{tip}\n")).unwrap();
    assert!(
        git::git(
            root.path(),
            &["reflog", "show", "--format=%H %gn %gs", "HEAD"],
            b""
        )
        .is_empty()
    );
}

#[rstest]
#[case::nul(b"A <a@b> 1 +0000\ta\0b\n")]
#[case::cr(b"A <a@b> 1 +0000\ta\rb\n")]
#[case::short(b"A <a@b> 1 +01\tfixture\n")]
#[case::suffix(b"A <a@b> 1 +0000suffix\tfixture\n")]
#[case::overflow(b"A <a@b> 9223372036854775808 +0000\tfixture\n")]
#[case::corrupt(b"broken\n")]
fn appends_canonical_record_without_rewriting_imported_history(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] tail: &[u8],
) {
    let (root, repo, tip) = fixture(format, "files");
    let raw = data(tip, tail);
    let path = repo.git_dir().join("logs/HEAD");
    fs::write(&path, &raw).unwrap();
    repo.references()
        .unwrap()
        .transaction(&[append(tip, b"canonical")])
        .unwrap();
    assert!(fs::read(path).unwrap().starts_with(&raw));
    assert_eq!(
        git::git(
            root.path(),
            &["reflog", "show", "-1", "--format=%gs", "HEAD"],
            b""
        ),
        b"canonical\n"
    );
}

#[rstest]
fn unterminated_tail_refuses_before_publication(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (root, repo, tip) = fixture(format, "files");
    let raw = data(tip, b"A <a@b> 1 +0000\tunfinished");
    let path = repo.git_dir().join("logs/HEAD");
    fs::write(&path, &raw).unwrap();
    let before = fs::read(repo.git_dir().join("HEAD")).unwrap();
    assert!(
        repo.references()
            .unwrap()
            .transaction(&[append(tip, b"canonical")])
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), raw);
    assert_eq!(fs::read(repo.git_dir().join("HEAD")).unwrap(), before);
    assert!(!repo.git_dir().join("logs/HEAD.lock").exists());
    // Force a changed object ID: an unchanged detached update can skip writing the reflog.
    let tree = git::git(root.path(), &["mktree"], b"");
    let next = git::git(
        root.path(),
        &["commit-tree", std::str::from_utf8(&tree).unwrap().trim()],
        b"distinct append target\n",
    );
    let next = std::str::from_utf8(&next).unwrap().trim();
    // Git appends directly; observation establishes the deliberate safer boundary.
    git::git(
        root.path(),
        &["update-ref", "--no-deref", "-m", "git append", "HEAD", next],
        b"",
    );
    let after = fs::read(path).unwrap();
    assert!(after.starts_with(&raw));
    assert!(after[raw.len()..].starts_with(format!("{tip} {next} ").as_bytes()));
}

#[rstest]
fn canonical_validation_stays_strict(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("files", "reftable")] backend: &str,
    #[values(b"a\rb".as_slice(), b"a\0b".as_slice(), b"a\nb".as_slice())] message: &[u8],
) {
    let (_root, repo, tip) = fixture(format, backend);
    assert!(
        repo.references()
            .unwrap()
            .transaction(&[append(tip, message)])
            .is_err()
    );
}

#[rstest]
fn large_history_limits_are_explicit(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (_root, repo, tip) = fixture(format, "files");
    let raw = data(tip, b"A <a@b> 1 +0000\tfixture\n").repeat(10_000);
    fs::write(repo.git_dir().join("logs/HEAD"), &raw).unwrap();
    let refs = repo.references().unwrap();
    let log = refs
        .imported_reflog(&head(), ReflogLimits::default(), &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    assert!(log.is_complete());
    assert_eq!(log.records().len(), 10_000);
    let limited = refs
        .imported_reflog(
            &head(),
            ReflogLimits {
                records: 12,
                ..ReflogLimits::default()
            },
            &AtomicBool::new(false),
        )
        .unwrap()
        .unwrap();
    assert!(matches!(limited.end(), ReflogReadEnd::Limit("records")));
    assert!(!limited.is_complete());
    assert_eq!(limited.recoverable_roots().count(), 12);
}

#[rstest]
fn reads_original_git_binary_history(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (root, repo, tip) = fixture(format, "reftable");
    let log = repo
        .references()
        .unwrap()
        .imported_reflog(&head(), ReflogLimits::default(), &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    assert!(log.is_complete());
    assert_eq!(log.recoverable_roots().collect::<Vec<_>>(), [tip]);
    assert_eq!(
        log.records()[0].fields().unwrap().message,
        b"commit (initial): original"
    );
    assert_eq!(
        git::git(
            root.path(),
            &["reflog", "show", "--format=%gs", "HEAD"],
            b""
        ),
        b"commit (initial): original\n"
    );
    let limited = repo
        .references()
        .unwrap()
        .imported_reflog(
            &head(),
            ReflogLimits {
                retained_bytes: 1,
                ..ReflogLimits::default()
            },
            &AtomicBool::new(false),
        )
        .unwrap()
        .unwrap();
    assert!(!limited.is_complete());
    assert!(matches!(limited.end(), ReflogReadEnd::Limit(_)));
}

#[rstest]
fn binary_unsigned_fields_and_stack_failures_remain_explicit(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    use girt::refs::reftable::{Limits, LogRecord, LogValue, RecordName, Snapshot, StackLimits};
    let (_root, repo, tip) = fixture(format, "reftable");
    let directory = repo.git_dir().join("reftable");
    let mut snapshot = Snapshot::read(
        &directory,
        format,
        StackLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let value = LogValue {
        old: ObjectId::null(format),
        new: tip,
        name: b"A\xff".to_vec(),
        email: b"a@b".to_vec(),
        seconds: u64::MAX,
        offset_minutes: -30,
        message: b"a\0b\r\n".to_vec(),
    };
    snapshot.table.logs = vec![LogRecord {
        name: RecordName::new(b"HEAD").unwrap(),
        update_index: snapshot.table.max_update_index,
        value: Some(value.clone()),
    }];
    let encoded = snapshot.table.encode(Limits::default()).unwrap();
    fs::write(directory.join("original.ref"), &encoded).unwrap();
    fs::write(directory.join("tables.list"), b"original.ref\n").unwrap();
    let log = repo
        .references()
        .unwrap()
        .imported_reflog(&head(), ReflogLimits::default(), &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    assert!(log.is_complete());
    assert_eq!(
        log.records(),
        &[ImportedRecord::Reftable {
            update_index: snapshot.table.max_update_index,
            value
        }]
    );
    assert_eq!(
        log.records()[0].fields().unwrap().seconds,
        i128::from(u64::MAX)
    );
    assert_eq!(log.recoverable_roots().collect::<Vec<_>>(), [tip]);
    assert_eq!(log.records()[0].fields().unwrap().message, b"a\0b\r");
    let limits = StackLimits {
        records: Limits {
            bytes: 1,
            ..Limits::default()
        },
        ..StackLimits::default()
    };
    assert!(
        repo.references()
            .unwrap()
            .with_reftable_limits(limits)
            .imported_reflog(&head(), ReflogLimits::default(), &AtomicBool::new(false))
            .is_err()
    );
    fs::write(directory.join("original.ref"), b"broken").unwrap();
    assert!(
        repo.references()
            .unwrap()
            .imported_reflog(&head(), ReflogLimits::default(), &AtomicBool::new(false))
            .is_err()
    );
    fs::remove_file(directory.join("original.ref")).unwrap();
    assert!(
        repo.references()
            .unwrap()
            .imported_reflog(&head(), ReflogLimits::default(), &AtomicBool::new(false))
            .is_err()
    );
}

#[rstest]
fn absent_cancelled_and_path_failure_are_distinct(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (_root, repo, _tip) = fixture(format, "files");
    let refs = repo.references().unwrap();
    assert!(
        refs.imported_reflog(
            &RefName::new("refs/heads/missing").unwrap(),
            ReflogLimits::default(),
            &AtomicBool::new(false)
        )
        .unwrap()
        .is_none()
    );
    let log = refs
        .imported_reflog(&head(), ReflogLimits::default(), &AtomicBool::new(true))
        .unwrap()
        .unwrap();
    assert!(matches!(log.end(), ReflogReadEnd::Cancelled));
    assert!(!log.is_complete());
    fs::remove_file(repo.git_dir().join("logs/HEAD")).unwrap();
    fs::create_dir(repo.git_dir().join("logs/HEAD")).unwrap();
    assert!(
        refs.imported_reflog(&head(), ReflogLimits::default(), &AtomicBool::new(false))
            .is_err()
    );
}

#[rstest]
fn retains_one_mebibyte_original_message(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (root, repo, tip) = fixture(format, "files");
    fs::write(repo.git_dir().join("HEAD"), format!("{tip}\n")).unwrap();
    let tail = [
        b"A <a@b> 1 +0000\t".as_slice(),
        &vec![b'x'; 1024 * 1024],
        b"\n",
    ]
    .concat();
    let raw = data(tip, &tail);
    fs::write(repo.git_dir().join("logs/HEAD"), &raw).unwrap();
    let log = repo
        .references()
        .unwrap()
        .imported_reflog(&head(), ReflogLimits::default(), &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    assert!(log.is_complete());
    assert_eq!(
        log.records(),
        &[ImportedRecord::File { format, bytes: raw }]
    );
    assert_eq!(
        git::git(
            root.path(),
            &["reflog", "show", "--format=%gs", "HEAD"],
            b""
        )
        .len(),
        1024 * 1024 + 1
    );
}

#[rstest]
fn linked_worktree_import_routes_private_and_shared_history(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("files", "reftable")] backend: &str,
) {
    let (root, repo, tip) = fixture(format, backend);
    let linked = root.path().join("linked");
    git::git(
        root.path(),
        &["worktree", "add", "-b", "linked", "linked", "main"],
        b"",
    );
    let private = Repository::open(&linked).unwrap();
    let mut change = append(tip, b"private update");
    change.dereference = true;
    private
        .references()
        .unwrap()
        .transaction(&[change])
        .unwrap();
    let cancel = AtomicBool::new(false);
    let private_log = private
        .references()
        .unwrap()
        .imported_reflog(&head(), ReflogLimits::default(), &cancel)
        .unwrap()
        .unwrap();
    let shared_log = repo
        .references()
        .unwrap()
        .imported_reflog(
            &RefName::new("refs/heads/linked").unwrap(),
            ReflogLimits::default(),
            &cancel,
        )
        .unwrap()
        .unwrap();
    let main_log = repo
        .references()
        .unwrap()
        .imported_reflog(&head(), ReflogLimits::default(), &cancel)
        .unwrap()
        .unwrap();
    assert!(private_log.is_complete());
    assert!(shared_log.is_complete());
    assert!(
        private_log
            .records()
            .last()
            .unwrap()
            .fields()
            .unwrap()
            .message
            .starts_with(b"private update")
    );
    assert!(
        shared_log
            .records()
            .last()
            .unwrap()
            .fields()
            .unwrap()
            .message
            .starts_with(b"private update")
    );
    assert!(
        !main_log
            .records()
            .last()
            .unwrap()
            .fields()
            .unwrap()
            .message
            .starts_with(b"private update")
    );
}

#[rstest]
fn colocation_preserves_imported_bytes_and_adds_readable_history(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (_root, repo, tip) = fixture(format, "files");
    let raw = data(tip, b"A <a@b> 9223372036854775808 +0000\ta\0b\n");
    fs::write(repo.git_dir().join("logs/HEAD"), &raw).unwrap();
    let change = append(tip, b"colocation");
    repo.edit_colocation(girt::index::Limits::default())
        .unwrap()
        .commit(Target::Direct(tip), Expected::Exists, change.reflog)
        .unwrap();
    let log = repo
        .references()
        .unwrap()
        .imported_reflog(&head(), ReflogLimits::default(), &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    assert!(log.is_complete());
    assert_eq!(
        log.records()[0],
        ImportedRecord::File { format, bytes: raw }
    );
    assert_eq!(log.records()[1].fields().unwrap().message, b"colocation");
}
