use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::*;

fn write(repo: &Repository, path: &str, bytes: &[u8]) {
    let path = repo.git_dir().join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn run(refs: &References<'_>, edits: &[RefEdit]) -> Result<Vec<RefEditOutcome>, TransactionError> {
    refs.transaction_files_with_stored_log_ids(
        edits,
        FilesTransactionOptions::default(),
        &AtomicBool::new(false),
    )
}

#[rstest]
#[case::direct_terminal(b"1111111111111111111111111111111111111111\n".as_slice())]
#[case::chain(b"ref: refs/heads/missing\n".as_slice())]
#[case::cycle(b"ref: refs/heads/alias\n".as_slice())]
#[case::malformed_terminal(b"not a reference\n".as_slice())]
fn symbolic_replacement_never_reads_or_locks_terminal(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
    #[case] terminal: &[u8],
    #[values(false, true)] deletion: bool,
) {
    let (_temp, repo) = fixture(format);
    write(&repo, "refs/heads/alias", b"ref: refs/heads/base\n");
    write(&repo, "refs/heads/base", terminal);
    write(&repo, "refs/heads/base.lock", b"foreign");
    write(
        &repo,
        "logs/refs/heads/base",
        b"terminal log remains untouched\n",
    );
    let refs = repo.references().unwrap();
    let mut operation = edit(format, "refs/heads/alias");
    operation.expected = Expected::Value(Target::Symbolic(name("refs/heads/base")));
    if deletion {
        operation.target = None;
    }
    let result = run(&refs, &[operation]).unwrap();
    assert_eq!(
        result[0].logs,
        [(name("refs/heads/alias"), LogOutcome::Appended)]
    );
    let entries = refs.reflog(&name("refs/heads/alias")).unwrap().unwrap();
    assert_eq!(entries[0].old, id(format, 0));
    assert_eq!(entries[0].new, id(format, if deletion { 0 } else { 1 }));
    assert_eq!(
        fs::read(repo.git_dir().join("refs/heads/base")).unwrap(),
        terminal
    );
    assert_eq!(
        fs::read(repo.git_dir().join("logs/refs/heads/base")).unwrap(),
        b"terminal log remains untouched\n"
    );
    assert_eq!(
        fs::read(repo.git_dir().join("refs/heads/base.lock")).unwrap(),
        b"foreign"
    );
    fs::remove_file(repo.git_dir().join("refs/heads/base.lock")).unwrap();
    clean(&repo);
}

#[rstest]
fn dangling_symbolic_is_present_for_exact_expectations(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
    #[values(false, true)] expected_absent: bool,
) {
    let (_temp, repo) = fixture(format);
    write(&repo, "refs/heads/alias", b"ref: refs/heads/missing\n");
    let refs = repo.references().unwrap();
    let mut operation = edit(format, "refs/heads/alias");
    operation.expected = if expected_absent {
        Expected::Absent
    } else {
        Expected::Value(Target::Direct(id(format, 1)))
    };
    assert!(matches!(
        run(&refs, &[operation]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Mismatch { .. },
            ..
        })
    ));
    assert_eq!(
        refs.read(&name("refs/heads/alias")).unwrap(),
        Some(Target::Symbolic(name("refs/heads/missing")))
    );
    assert!(refs.reflog(&name("refs/heads/alias")).unwrap().is_none());
    clean(&repo);
}

#[rstest]
fn dangling_symbolic_replacement_logs_null(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
) {
    let (_temp, repo) = fixture(format);
    write(&repo, "refs/heads/alias", b"ref: refs/heads/missing\n");
    let refs = repo.references().unwrap();
    let mut operation = edit(format, "refs/heads/alias");
    operation.expected = Expected::Value(Target::Symbolic(name("refs/heads/missing")));
    run(&refs, &[operation]).unwrap();
    assert_eq!(
        refs.reflog(&name("refs/heads/alias")).unwrap().unwrap()[0].old,
        id(format, 0)
    );
    assert!(!repo.git_dir().join("refs/heads/missing").exists());
    clean(&repo);
}

#[rstest]
fn direct_conditional_append_compares_stored_target(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
    #[values(append_if_changed(), append_existing_if_changed())] policy: Reflog,
    #[values(false, true)] changed: bool,
) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    run(&refs, &[edit(format, "refs/heads/a")]).unwrap();
    let mut operation = edit(format, "refs/heads/a");
    operation.expected = Expected::Value(Target::Direct(id(format, 1)));
    operation.target = Some(Target::Direct(id(format, if changed { 2 } else { 1 })));
    operation.reflog = policy;
    run(&refs, &[operation]).unwrap();
    let entries = refs.reflog(&name("refs/heads/a")).unwrap().unwrap();
    assert_eq!(entries.len(), if changed { 2 } else { 1 });
    if changed {
        assert_eq!(
            (entries[1].old, entries[1].new),
            (id(format, 1), id(format, 2))
        );
    }
    clean(&repo);
}

#[rstest]
fn existing_log_policy_selects_only_the_alias_log(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
    #[values(false, true)] exists: bool,
) {
    let (_temp, repo) = fixture(format);
    write(&repo, "refs/heads/alias", b"ref: refs/heads/base\n");
    if exists {
        write(&repo, "logs/refs/heads/alias", b"");
    }
    write(&repo, "logs/refs/heads/base", b"untouched\n");
    let refs = repo.references().unwrap();
    let mut operation = edit(format, "refs/heads/alias");
    operation.expected = Expected::Exists;
    operation.reflog = append_existing_if_changed();
    let result = run(&refs, &[operation]).unwrap();
    assert_eq!(result[0].logs.len(), usize::from(exists));
    assert_eq!(refs.has_reflog(&name("refs/heads/alias")).unwrap(), exists);
    assert_eq!(
        fs::read(repo.git_dir().join("logs/refs/heads/base")).unwrap(),
        b"untouched\n"
    );
    clean(&repo);
}

#[rstest]
#[case::dereference(true, false)]
#[case::symbolic_replacement(false, true)]
fn invalid_later_edit_is_rejected_before_any_lock(
    #[case] dereference: bool,
    #[case] symbolic: bool,
) {
    let format = crate::ObjectFormat::Sha1;
    let (_temp, repo) = fixture(format);
    write(&repo, "packed-refs.lock", b"foreign");
    let mut invalid = edit(format, "refs/heads/b");
    invalid.dereference = dereference;
    if symbolic {
        invalid.target = Some(Target::Symbolic(name("refs/heads/a")));
    }
    let error = run(
        &repo.references().unwrap(),
        &[edit(format, "refs/heads/a"), invalid],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        TransactionError::Prepare {
            operation: Some(1),
            source: ReferenceError::Unsupported(_)
        }
    ));
    assert!(!repo.git_dir().join("refs/heads/a").exists());
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs.lock")).unwrap(),
        b"foreign"
    );
    fs::remove_file(repo.git_dir().join("packed-refs.lock")).unwrap();
    clean(&repo);
}

#[test]
fn reftable_is_rejected_before_effects() {
    let temp = tempfile::tempdir().unwrap();
    let format = crate::ObjectFormat::Sha1;
    let repo = Repository::init_with_backend(
        format,
        temp.path().join("repo"),
        crate::InitKind::Bare,
        super::super::super::Backend::Reftable,
    )
    .unwrap();
    let before = fs::read(repo.git_dir().join("reftable/tables.list")).unwrap();
    assert!(matches!(
        run(&repo.references().unwrap(), &[edit(format, "refs/heads/a")]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Unsupported(_),
            ..
        })
    ));
    assert_eq!(
        fs::read(repo.git_dir().join("reftable/tables.list")).unwrap(),
        before
    );
    clean(&repo);
}

#[rstest]
#[case::alias("refs/heads/alias.lock")]
#[case::log("logs/refs/heads/alias.lock")]
fn edited_name_and_log_locks_remain_authoritative(#[case] path: &str) {
    let format = crate::ObjectFormat::Sha1;
    let (_temp, repo) = fixture(format);
    write(&repo, "refs/heads/alias", b"ref: refs/heads/base\n");
    write(&repo, path, b"foreign");
    let mut operation = edit(format, "refs/heads/alias");
    operation.expected = Expected::Exists;
    assert!(matches!(
        run(&repo.references().unwrap(), &[operation]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Locked(_),
            ..
        })
    ));
    assert_eq!(
        fs::read(repo.git_dir().join("refs/heads/alias")).unwrap(),
        b"ref: refs/heads/base\n"
    );
    fs::remove_file(repo.git_dir().join(path)).unwrap();
    clean(&repo);
}

fn wait_for(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "transaction did not acquire first reference lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[rstest]
fn alias_change_during_wait_is_not_refreshed(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
) {
    let (_temp, repo) = fixture(format);
    write(&repo, "refs/heads/alias", b"ref: refs/heads/base\n");
    write(&repo, "refs/heads/alias.lock", b"foreign");
    let mut operation = edit(format, "refs/heads/alias");
    operation.expected = Expected::Value(Target::Symbolic(name("refs/heads/base")));
    std::thread::scope(|scope| {
        scope.spawn(|| {
            // Locks follow discovery and sort by name. This proves the alias was observed before
            // we change it, without relying on a sleep to time the race.
            wait_for(&repo.git_dir().join("refs/heads/0.lock"));
            write(&repo, "refs/heads/alias", b"ref: refs/heads/other\n");
            fs::remove_file(repo.git_dir().join("refs/heads/alias.lock")).unwrap();
        });
        let result = repo
            .references()
            .unwrap()
            .transaction_files_with_stored_log_ids(
                &[edit(format, "refs/heads/0"), operation],
                FilesTransactionOptions {
                    reference_lock_wait: LockWait::For(Duration::from_secs(5)),
                    ..Default::default()
                },
                &AtomicBool::new(false),
            );
        assert!(matches!(
            result,
            Err(TransactionError::Prepare {
                source: ReferenceError::Mismatch { .. },
                ..
            })
        ));
    });
    assert!(!repo.git_dir().join("refs/heads/0").exists());
    assert_eq!(
        fs::read(repo.git_dir().join("refs/heads/alias")).unwrap(),
        b"ref: refs/heads/other\n"
    );
    clean(&repo);
}

#[test]
fn cancellation_during_wait_cleans_owned_locks() {
    let format = crate::ObjectFormat::Sha1;
    let (_temp, repo) = fixture(format);
    write(&repo, "refs/heads/b.lock", b"foreign");
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            wait_for(&repo.git_dir().join("refs/heads/a.lock"));
            cancel.store(true, Ordering::Relaxed);
        });
        let result = repo
            .references()
            .unwrap()
            .transaction_files_with_stored_log_ids(
                &[edit(format, "refs/heads/a"), edit(format, "refs/heads/b")],
                FilesTransactionOptions {
                    reference_lock_wait: LockWait::UntilCancelled,
                    ..Default::default()
                },
                &cancel,
            );
        assert!(matches!(
            result,
            Err(TransactionError::Prepare {
                source: ReferenceError::Cancelled,
                ..
            })
        ));
    });
    fs::remove_file(repo.git_dir().join("refs/heads/b.lock")).unwrap();
    assert!(!repo.git_dir().join("refs/heads/a").exists());
    clean(&repo);
}

#[rstest]
fn partial_publication_retains_precise_outcomes(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    let prepared = refs
        .prepare_files_transaction_with_options(
            &[edit(format, "refs/tags/a"), edit(format, "refs/tags/b")],
            FilesTransactionOptions::default(),
            &AtomicBool::new(false),
            LogIdentity::Stored,
        )
        .unwrap();
    fs::create_dir(repo.git_dir().join("refs/tags/b")).unwrap();
    let Err(TransactionError::Publish { outcomes, .. }) = prepared.publish() else {
        panic!("expected partial publication")
    };
    assert_eq!(outcomes[0].reference, RefOutcome::Published);
    assert_eq!(outcomes[1].reference, RefOutcome::Unchanged);
    assert_eq!(
        refs.reflog(&name("refs/tags/a")).unwrap().unwrap()[0].old,
        id(format, 0)
    );
    clean(&repo);
}

#[rstest]
fn symbolic_alias_logs_null_even_when_terminal_equals_replacement(
    #[values(crate::ObjectFormat::Sha1, crate::ObjectFormat::Sha256)] format: crate::ObjectFormat,
    #[values("HEAD", "refs/heads/alias", "refs/tags/alias")] alias: &str,
    #[values(log(), append_if_changed(), append_existing_if_changed())] policy: Reflog,
) {
    let (_temp, repo) = fixture(format);
    write(&repo, alias, b"ref: refs/heads/base\n");
    write(
        &repo,
        "refs/heads/base",
        format!("{}\n", id(format, 1)).as_bytes(),
    );
    write(&repo, &format!("logs/{alias}"), b"");
    let refs = repo.references().unwrap();
    let mut operation = edit(format, alias);
    operation.expected = Expected::Value(Target::Symbolic(name("refs/heads/base")));
    operation.reflog = policy;
    run(&refs, &[operation]).unwrap();
    let entries = refs.reflog(&name(alias)).unwrap().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        (entries[0].old, entries[0].new),
        (id(format, 0), id(format, 1))
    );
    assert_eq!(
        refs.read(&name("refs/heads/base")).unwrap(),
        Some(Target::Direct(id(format, 1)))
    );
    assert!(!refs.has_reflog(&name("refs/heads/base")).unwrap());
    clean(&repo);
}
