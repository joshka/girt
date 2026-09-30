use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::*;

fn options(wait: LockWait) -> FilesTransactionOptions {
    FilesTransactionOptions {
        reference_lock_wait: wait,
        packed_refs_lock_wait: wait,
    }
}

fn hold(repo: &Repository, relative: &str) -> std::path::PathBuf {
    let path = repo.git_dir().join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"foreign").unwrap();
    path
}

#[rstest]
#[case::packed_sha1(crate::ObjectFormat::Sha1, "packed-refs.lock")]
#[case::packed_sha256(crate::ObjectFormat::Sha256, "packed-refs.lock")]
#[case::reference_sha1(crate::ObjectFormat::Sha1, "refs/tags/a.lock")]
#[case::reference_sha256(crate::ObjectFormat::Sha256, "refs/tags/a.lock")]
fn released_lock_publishes(#[case] format: crate::ObjectFormat, #[case] path: &str) {
    let (_temp, repo) = fixture(format);
    let lock = hold(&repo, path);
    let started = Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(40));
            fs::remove_file(&lock).unwrap();
        });
        let result = repo
            .references()
            .unwrap()
            .transaction_files_with_options(
                &[edit(format, "refs/tags/a")],
                options(LockWait::For(Duration::from_secs(2))),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(result[0].reference, RefOutcome::Published);
    });
    assert!(started.elapsed() >= Duration::from_millis(40));
    assert!(started.elapsed() < Duration::from_secs(5));
    clean(&repo);
}

#[rstest]
#[case::immediate(LockWait::Immediate)]
#[case::zero(LockWait::For(Duration::ZERO))]
#[case::finite(LockWait::For(Duration::from_millis(40)))]
fn timeout_preserves_foreign_lock_and_cleans_owned_locks(#[case] wait: LockWait) {
    let format = crate::ObjectFormat::Sha1;
    let (_temp, repo) = fixture(format);
    let lock = hold(&repo, "refs/tags/b.lock");
    let started = Instant::now();
    let error = repo
        .references()
        .unwrap()
        .transaction_files_with_options(
            &[edit(format, "refs/tags/a"), edit(format, "refs/tags/b")],
            options(wait),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        TransactionError::Prepare {
            source: ReferenceError::Locked(_),
            ..
        }
    ));
    if let LockWait::For(duration) = wait {
        assert!(started.elapsed() >= duration);
    }
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(fs::read(&lock).unwrap(), b"foreign");
    assert!(!repo.git_dir().join("refs/tags/a").exists());
    fs::remove_file(lock).unwrap();
    clean(&repo);
}

#[rstest]
#[case::packed("packed-refs.lock")]
#[case::reference("refs/tags/b.lock")]
fn cancellation_ends_unbounded_wait_and_cleans_owned_locks(#[case] path: &str) {
    let format = crate::ObjectFormat::Sha1;
    let (_temp, repo) = fixture(format);
    let lock = hold(&repo, path);
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(40));
            cancel.store(true, Ordering::Relaxed);
        });
        let error = repo
            .references()
            .unwrap()
            .transaction_files_with_options(
                &[edit(format, "refs/tags/a"), edit(format, "refs/tags/b")],
                options(LockWait::UntilCancelled),
                &cancel,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            TransactionError::Prepare {
                source: ReferenceError::Cancelled,
                ..
            }
        ));
    });
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(fs::read(&lock).unwrap(), b"foreign");
    assert!(!repo.git_dir().join("refs/tags/a").exists());
    fs::remove_file(lock).unwrap();
    clean(&repo);
}

#[test]
fn already_cancelled_preserves_repository() {
    let format = crate::ObjectFormat::Sha1;
    let (_temp, repo) = fixture(format);
    let error = repo
        .references()
        .unwrap()
        .transaction_files_with_options(
            &[edit(format, "refs/tags/a")],
            options(LockWait::UntilCancelled),
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
    assert!(!repo.git_dir().join("refs/tags/a").exists());
    clean(&repo);
}

#[test]
fn target_changed_during_wait_does_not_refresh_precondition() {
    let format = crate::ObjectFormat::Sha256;
    let (_temp, repo) = fixture(format);
    let lock = hold(&repo, "refs/tags/a.lock");
    let path = repo.git_dir().join("refs/tags/a");
    fs::write(&path, format!("{}\n", id(format, 2))).unwrap();
    let mut update = edit(format, "refs/tags/a");
    update.expected = Expected::Value(Target::Direct(id(format, 2)));
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(40));
            fs::write(&path, format!("{}\n", id(format, 3))).unwrap();
            fs::remove_file(&lock).unwrap();
        });
        let error = repo
            .references()
            .unwrap()
            .transaction_files_with_options(
                &[update],
                options(LockWait::For(Duration::from_secs(2))),
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            TransactionError::Prepare {
                source: ReferenceError::Mismatch { .. },
                ..
            }
        ));
    });
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        format!("{}\n", id(format, 3))
    );
    clean(&repo);
}

#[test]
fn each_reference_gets_a_fresh_budget() {
    let format = crate::ObjectFormat::Sha1;
    let (_temp, repo) = fixture(format);
    let first = hold(&repo, "refs/tags/a.lock");
    let second = hold(&repo, "refs/tags/b.lock");
    let started = Instant::now();
    std::thread::scope(|scope| {
        // Each lock is released within its own 2 s budget but after the first budget would have
        // expired. The margins absorb slow file deletion on Windows runners.
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(1200));
            fs::remove_file(&first).unwrap();
            std::thread::sleep(Duration::from_millis(1200));
            fs::remove_file(&second).unwrap();
        });
        // Native locks sort by name even though publication follows this reverse input order.
        let result = repo
            .references()
            .unwrap()
            .transaction_files_with_options(
                &[edit(format, "refs/tags/b"), edit(format, "refs/tags/a")],
                options(LockWait::For(Duration::from_secs(2))),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(result[0].name, name("refs/tags/b"));
        assert_eq!(result[1].reference, RefOutcome::Published);
    });
    assert!(started.elapsed() >= Duration::from_millis(2400));
    assert!(started.elapsed() < Duration::from_secs(10));
    clean(&repo);
}

#[test]
fn reflog_lock_still_fails_immediately() {
    let format = crate::ObjectFormat::Sha1;
    let (_temp, repo) = fixture(format);
    let lock = hold(&repo, "logs/refs/tags/a.lock");
    let started = Instant::now();
    let error = repo
        .references()
        .unwrap()
        .transaction_files_with_options(
            &[edit(format, "refs/tags/a")],
            options(LockWait::UntilCancelled),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        TransactionError::Prepare {
            source: ReferenceError::Locked(_),
            ..
        }
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    fs::remove_file(lock).unwrap();
    clean(&repo);
}

#[test]
fn reftable_options_fail_before_effects() {
    let temp = tempfile::tempdir().unwrap();
    let format = crate::ObjectFormat::Sha1;
    let repo = Repository::init_with_backend(
        format,
        temp.path().join("repo"),
        crate::InitKind::Bare,
        crate::refs::Backend::Reftable,
    )
    .unwrap();
    let before = fs::read(repo.git_dir().join("reftable/tables.list")).unwrap();
    let error = repo
        .references()
        .unwrap()
        .transaction_files_with_options(
            &[edit(format, "refs/tags/a")],
            options(LockWait::UntilCancelled),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        TransactionError::Prepare {
            source: ReferenceError::Unsupported(_),
            ..
        }
    ));
    assert_eq!(
        fs::read(repo.git_dir().join("reftable/tables.list")).unwrap(),
        before
    );
    clean(&repo);
}

#[test]
fn options_preparation_reuses_partial_publication_without_retry() {
    let format = crate::ObjectFormat::Sha1;
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    let prepared = refs
        .prepare_files_transaction_with_options(
            &[edit(format, "refs/tags/a"), edit(format, "refs/tags/b")],
            options(LockWait::UntilCancelled),
            &AtomicBool::new(false),
            LogIdentity::Resolved,
        )
        .unwrap();
    fs::create_dir(repo.git_dir().join("refs/tags/b")).unwrap();
    let Err(TransactionError::Publish { outcomes, .. }) = prepared.publish() else {
        panic!("expected publication failure")
    };
    assert_eq!(outcomes[0].reference, RefOutcome::Published);
    assert_eq!(outcomes[1].reference, RefOutcome::Unchanged);
    assert_eq!(refs.reflog(&name("refs/tags/a")).unwrap().unwrap().len(), 1);
    clean(&repo);
}

#[rstest]
#[case::packed("packed-refs.lock", FilesTransactionOptions { reference_lock_wait: LockWait::UntilCancelled, packed_refs_lock_wait: LockWait::Immediate })]
#[case::reference("refs/tags/a.lock", FilesTransactionOptions { reference_lock_wait: LockWait::Immediate, packed_refs_lock_wait: LockWait::UntilCancelled })]
fn packed_and_reference_policies_are_independent(
    #[case] path: &str,
    #[case] policies: FilesTransactionOptions,
) {
    let format = crate::ObjectFormat::Sha1;
    let (_temp, repo) = fixture(format);
    let lock = hold(&repo, path);
    let error = repo
        .references()
        .unwrap()
        .transaction_files_with_options(
            &[edit(format, "refs/tags/a")],
            policies,
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        TransactionError::Prepare {
            source: ReferenceError::Locked(_),
            ..
        }
    ));
    fs::remove_file(lock).unwrap();
    clean(&repo);
}

#[test]
fn non_contention_failure_does_not_wait() {
    let format = crate::ObjectFormat::Sha1;
    let (_temp, repo) = fixture(format);
    fs::write(repo.git_dir().join("refs/blocked"), b"obstruction").unwrap();
    let error = repo
        .references()
        .unwrap()
        .transaction_files_with_options(
            &[edit(format, "refs/blocked/a")],
            options(LockWait::UntilCancelled),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(error, TransactionError::Prepare { .. }));
    assert!(!matches!(
        error,
        TransactionError::Prepare {
            source: ReferenceError::Locked(_) | ReferenceError::Cancelled,
            ..
        }
    ));
    clean(&repo);
}
