//! Original isolated Git fixtures for caller-bounded files reference transactions.
#[path = "support/maintenance_git.rs"]
mod oracle;

use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use girt::refs::{
    Expected, FilesTransactionLimits, FilesTransactionOptions, RefEdit, RefName, ReferenceError,
    Reflog, Target, TransactionError,
};
use girt::{ObjectFormat, ObjectId, Repository, Signature};

fn git(root: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut controlled = vec!["-c", "maintenance.auto=false", "-c", "gc.auto=0"];
    controlled.extend_from_slice(args);
    oracle::git(root, &controlled, input)
}

fn fixture(format: ObjectFormat) -> (tempfile::TempDir, Repository, ObjectId, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &["init", "--bare", &format!("--object-format={format}")],
        b"",
    );
    git(
        root.path(),
        &["symbolic-ref", "HEAD", "refs/heads/main"],
        b"",
    );
    let tree = git(root.path(), &["mktree"], b"");
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let first = git(root.path(), &["commit-tree", tree], b"first\n");
    let first = std::str::from_utf8(&first).unwrap().trim();
    let next = git(root.path(), &["commit-tree", tree, "-p", first], b"next\n");
    let next = std::str::from_utf8(&next).unwrap().trim();
    git(
        root.path(),
        &[
            "update-ref",
            "--create-reflog",
            "-m",
            "seed",
            "refs/heads/main",
            first,
        ],
        b"",
    );
    git(root.path(), &["pack-refs", "--all", "--prune"], b"");
    let repo = Repository::open(root.path()).unwrap();
    (root, repo, first.parse().unwrap(), next.parse().unwrap())
}

fn edit(old: ObjectId, new: ObjectId) -> RefEdit {
    RefEdit::new(
        RefName::new("refs/heads/main").unwrap(),
        Some(Target::Direct(new)),
        Expected::Value(Target::Direct(old)),
        Reflog::Append {
            committer: Signature {
                name: b"Maintenance Fixture".to_vec(),
                email: b"maintenance@example.com".to_vec(),
                seconds: 1_700_000_000,
                offset_minutes: 0,
            },
            message: b"bounded update".to_vec(),
        },
    )
}

fn options(limits: FilesTransactionLimits) -> FilesTransactionOptions {
    FilesTransactionOptions::GIT_DEFAULT.with_limits(limits)
}

#[test]
fn each_budget_refuses_before_reference_or_reflog_publication_and_releases_locks() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for warm in [false, true] {
            let (root, repo, old, new) = fixture(format);
            let refs = repo.references().unwrap();
            if warm {
                refs.list().unwrap();
            }
            let packed = fs::read(root.path().join("packed-refs")).unwrap();
            let log = fs::read(root.path().join("logs/refs/heads/main")).unwrap();
            for limits in [
                FilesTransactionLimits::default().with_max_edits(0),
                FilesTransactionLimits::default().with_max_packed_refs(0),
                FilesTransactionLimits::default().with_max_input_bytes(packed.len() - 1),
                FilesTransactionLimits::default().with_max_prepared_bytes(0),
            ] {
                let error = refs
                    .transaction_files_with_options(
                        &[edit(old, new)],
                        options(limits),
                        &AtomicBool::new(false),
                    )
                    .unwrap_err();
                assert!(matches!(
                    error,
                    TransactionError::Prepare {
                        source: ReferenceError::Limit(_),
                        ..
                    }
                ));
                assert_eq!(fs::read(root.path().join("packed-refs")).unwrap(), packed);
                assert_eq!(
                    fs::read(root.path().join("logs/refs/heads/main")).unwrap(),
                    log
                );
                assert!(!root.path().join("refs/heads/main").exists());
                for lock in [
                    "packed-refs.lock",
                    "refs/heads/main.lock",
                    "logs/refs/heads/main.lock",
                ] {
                    assert!(!root.path().join(lock).exists(), "left {lock}");
                }
            }
            // The bounded refusals do not impose a budget on the trusted entry point.
            refs.transaction(&[edit(old, new)]).unwrap();
            assert_eq!(
                refs.read(&RefName::new("refs/heads/main").unwrap())
                    .unwrap(),
                Some(Target::Direct(new))
            );
            git(root.path(), &["fsck", "--strict", "--no-dangling"], b"");
        }
    }
}

#[test]
fn edit_admission_precedes_lock_acquisition_and_preserves_foreign_locks() {
    let (root, repo, old, new) = fixture(ObjectFormat::Sha1);
    fs::write(root.path().join("packed-refs.lock"), b"foreign owner").unwrap();
    let error = repo
        .references()
        .unwrap()
        .transaction_files_with_options(
            &[edit(old, new)],
            options(FilesTransactionLimits::default().with_max_edits(0)),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        TransactionError::Prepare {
            source: ReferenceError::Limit(_),
            ..
        }
    ));
    assert_eq!(
        fs::read(root.path().join("packed-refs.lock")).unwrap(),
        b"foreign owner"
    );
}

#[test]
fn aggregate_input_budget_includes_symbolic_discovery_and_locked_rereads() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (root, repo, old, new) = fixture(format);
        let alias = root.path().join("refs/heads/alias");
        fs::write(&alias, b"ref: refs/heads/main\n").unwrap();
        let packed = fs::read(root.path().join("packed-refs")).unwrap();
        let mut change = edit(old, new);
        change.name = RefName::new("refs/heads/alias").unwrap();
        change.dereference = girt::refs::Dereference::Follow;
        let error = repo
            .references()
            .unwrap()
            .transaction_files_with_options(
                &[change],
                options(FilesTransactionLimits::default().with_max_input_bytes(
                    packed.len() + fs::metadata(&alias).unwrap().len() as usize,
                )),
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            TransactionError::Prepare {
                source: ReferenceError::Limit(_),
                ..
            }
        ));
        assert_eq!(fs::read(&alias).unwrap(), b"ref: refs/heads/main\n");
        assert_eq!(fs::read(root.path().join("packed-refs")).unwrap(), packed);
        assert!(!root.path().join("refs/heads/main").exists());
        for lock in [
            "packed-refs.lock",
            "refs/heads/main.lock",
            "refs/heads/alias.lock",
        ] {
            assert!(!root.path().join(lock).exists());
        }
    }
}

#[test]
fn default_controlled_publication_matches_git_reference_and_reflog_bytes() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (root, repo, old, new) = fixture(format);
        let (expected, _, expected_old, expected_new) = fixture(format);
        assert_eq!((old, new), (expected_old, expected_new));
        let mut tag = edit(old, new);
        tag.name = RefName::new("refs/tags/new").unwrap();
        tag.expected = Expected::Absent;
        repo.references()
            .unwrap()
            .transaction_files_with_options(
                &[edit(old, new), tag],
                options(FilesTransactionLimits::default()),
                &AtomicBool::new(false),
            )
            .unwrap();
        let input = format!("update refs/heads/main {new} {old}\ncreate refs/tags/new {new}\n");
        git(
            expected.path(),
            &[
                "update-ref",
                "--create-reflog",
                "-m",
                "bounded update",
                "--stdin",
            ],
            input.as_bytes(),
        );
        for path in [
            "packed-refs",
            "refs/heads/main",
            "refs/tags/new",
            "logs/refs/heads/main",
            "logs/refs/tags/new",
            "HEAD",
        ] {
            assert_eq!(
                fs::read(root.path().join(path)).unwrap(),
                fs::read(expected.path().join(path)).unwrap(),
                "{path}"
            );
        }
        git(root.path(), &["fsck", "--strict", "--no-dangling"], b"");
    }
}

#[test]
fn precancelled_controlled_transaction_returns_without_publication() {
    let (root, repo, old, new) = fixture(ObjectFormat::Sha1);
    let error = repo
        .references()
        .unwrap()
        .transaction_files_with_options(
            &[edit(old, new)],
            options(FilesTransactionLimits::default()),
            &AtomicBool::new(true),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        TransactionError::Prepare {
            source: ReferenceError::Cancelled,
            ..
        }
    ));
    assert!(!root.path().join("packed-refs.lock").exists());
    assert!(!root.path().join("refs/heads/main").exists());
}

#[test]
fn packed_replacement_budget_refuses_before_any_packed_or_loose_deletion() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (root, repo, old, _) = fixture(format);
        let input: String = (0..64)
            .map(|i| format!("create refs/tags/budget/{i:03} {old}\n"))
            .collect();
        git(root.path(), &["update-ref", "--stdin"], input.as_bytes());
        git(root.path(), &["pack-refs", "--all", "--prune"], b"");
        let packed = fs::read(root.path().join("packed-refs")).unwrap();
        let name = RefName::new("refs/tags/budget/000").unwrap();
        let change = RefEdit::new(
            name.clone(),
            None,
            Expected::Value(Target::Direct(old)),
            Reflog::Preserve,
        );
        let error = repo
            .references()
            .unwrap()
            .transaction_files_with_options(
                &[change],
                options(FilesTransactionLimits::default().with_max_prepared_bytes(1024)),
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            TransactionError::Prepare {
                source: ReferenceError::Limit(_),
                ..
            }
        ));
        assert_eq!(fs::read(root.path().join("packed-refs")).unwrap(), packed);
        assert_eq!(
            repo.references().unwrap().read(&name).unwrap(),
            Some(Target::Direct(old))
        );
        assert!(!root.path().join("packed-refs.lock").exists());
        assert!(!root.path().join("refs/tags/budget/000.lock").exists());
    }
}

#[test]
fn loose_file_budget_is_checked_before_parsing_oversized_contents() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (root, repo, old, new) = fixture(format);
        let path = root.path().join("refs/heads/main");
        let oversized = vec![b'x'; 4096];
        fs::write(&path, &oversized).unwrap();
        let packed = fs::read(root.path().join("packed-refs")).unwrap();
        let error = repo
            .references()
            .unwrap()
            .transaction_files_with_options(
                &[edit(old, new)],
                options(FilesTransactionLimits::default().with_max_input_bytes(packed.len() + 32)),
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            TransactionError::Prepare {
                source: ReferenceError::Limit(_),
                ..
            }
        ));
        assert_eq!(fs::read(&path).unwrap(), oversized);
        assert_eq!(fs::read(root.path().join("packed-refs")).unwrap(), packed);
        assert!(!root.path().join("packed-refs.lock").exists());
    }
}

#[test]
fn aggregate_input_budget_also_admits_the_existing_reflog_append_tail() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (root, repo, old, new) = fixture(format);
        let packed = fs::read(root.path().join("packed-refs")).unwrap();
        let log = fs::read(root.path().join("logs/refs/heads/main")).unwrap();
        let error = repo
            .references()
            .unwrap()
            .transaction_files_with_options(
                &[edit(old, new)],
                options(FilesTransactionLimits::default().with_max_input_bytes(packed.len())),
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            TransactionError::Prepare {
                source: ReferenceError::Limit(_),
                ..
            }
        ));
        assert_eq!(
            fs::read(root.path().join("logs/refs/heads/main")).unwrap(),
            log
        );
        assert_eq!(fs::read(root.path().join("packed-refs")).unwrap(), packed);
        assert!(!root.path().join("refs/heads/main").exists());
        for lock in [
            "packed-refs.lock",
            "refs/heads/main.lock",
            "logs/refs/heads/main.lock",
        ] {
            assert!(!root.path().join(lock).exists());
        }
    }
}
