use std::fs;

use rstest::rstest;

use super::*;
use crate::{Repository, Signature};

fn fixture(format: crate::ObjectFormat) -> (tempfile::TempDir, Repository) {
    let temp = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, temp.path().join("repo"), crate::InitKind::Bare).unwrap();
    (temp, repo)
}
fn name(s: &str) -> RefName {
    RefName::new(s).unwrap()
}
fn id(format: crate::ObjectFormat, n: u8) -> ObjectId {
    ObjectId::from_bytes(format, &vec![n; format.digest_len()]).unwrap()
}
fn log() -> Reflog {
    Reflog::Append {
        committer: Signature {
            name: b"A Writer".to_vec(),
            email: b"a@example.com".to_vec(),
            seconds: 1700000000,
            offset_minutes: -420,
        },
        message: b"publish\tbatch".to_vec(),
    }
}
fn edit(format: crate::ObjectFormat, s: &str) -> RefEdit {
    RefEdit {
        name: name(s),
        dereference: false,
        target: Some(Target::Direct(id(format, 1))),
        expected: Expected::Absent,
        reflog: log(),
    }
}
fn clean(repo: &Repository) {
    fn visit(path: &std::path::Path) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            assert!(!path.to_string_lossy().ends_with(".lock"), "{path:?}");
            assert!(
                !path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".girt-")
            );
            if path.is_dir() {
                visit(&path);
            }
        }
    }
    visit(repo.git_dir());
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn resolved_head_and_second_ref_log_exact_ids(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    let mut head = edit(format, "HEAD");
    head.dereference = true;
    let result = refs
        .transaction(&[head, edit(format, "refs/tags/v1")])
        .unwrap();
    assert_eq!(result[0].name, name("refs/heads/main"));
    assert_eq!(result[0].reference, RefOutcome::Published);
    assert_eq!(
        result[0].logs,
        vec![
            (name("HEAD"), LogOutcome::Appended),
            (name("refs/heads/main"), LogOutcome::Appended)
        ]
    );
    assert_eq!(
        refs.read(&name("HEAD")).unwrap(),
        Some(Target::Symbolic(name("refs/heads/main")))
    );
    let entries = refs.reflog(&name("HEAD")).unwrap().unwrap();
    assert_eq!(entries[0].old, id(format, 0));
    assert_eq!(entries[0].new, id(format, 1));
    assert_eq!(entries[0].message, b"publish\tbatch");
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn final_mismatch_preserves_all_refs_and_logs(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    let before = fs::read(repo.git_dir().join("HEAD")).unwrap();
    let result = refs.transaction(&[edit(format, "refs/tags/a"), edit(format, "HEAD")]);
    assert!(matches!(
        result,
        Err(TransactionError::Prepare {
            operation: Some(1),
            source: ReferenceError::Mismatch { .. }
        })
    ));
    assert_eq!(fs::read(repo.git_dir().join("HEAD")).unwrap(), before);
    assert_eq!(refs.read(&name("refs/tags/a")).unwrap(), None);
    assert!(!repo.git_dir().join("logs").exists());
    clean(&repo);
}

#[rstest]
#[case::duplicate_sha1(crate::ObjectFormat::Sha1, "refs/heads/a", "refs/heads/a")]
#[case::duplicate_sha256(crate::ObjectFormat::Sha256, "refs/heads/a", "refs/heads/a")]
#[case::ancestor_sha1(crate::ObjectFormat::Sha1, "refs/heads/a", "refs/heads/a/b")]
#[case::ancestor_sha256(crate::ObjectFormat::Sha256, "refs/heads/a", "refs/heads/a/b")]
#[case::descendant_sha1(crate::ObjectFormat::Sha1, "refs/heads/a/b", "refs/heads/a")]
#[case::descendant_sha256(crate::ObjectFormat::Sha256, "refs/heads/a/b", "refs/heads/a")]
fn rejects_conflicting_batch(
    #[case] format: crate::ObjectFormat,
    #[case] first: &str,
    #[case] second: &str,
) {
    let (_temp, repo) = fixture(format);
    assert!(matches!(
        repo.references()
            .unwrap()
            .transaction(&[edit(format, first), edit(format, second)]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Conflict(_),
            ..
        })
    ));
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn overlapping_symbolic_destination_is_rejected(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let mut head = edit(format, "HEAD");
    head.dereference = true;
    assert!(matches!(
        repo.references()
            .unwrap()
            .transaction(&[head, edit(format, "refs/heads/main")]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Conflict(_),
            ..
        })
    ));
    clean(&repo);
}

#[rstest]
#[case::packed_sha1(crate::ObjectFormat::Sha1, "packed-refs.lock")]
#[case::packed_sha256(crate::ObjectFormat::Sha256, "packed-refs.lock")]
#[case::ref_lock_sha1(crate::ObjectFormat::Sha1, "refs/tags/a.lock")]
#[case::ref_lock_sha256(crate::ObjectFormat::Sha256, "refs/tags/a.lock")]
#[case::log_lock_sha1(crate::ObjectFormat::Sha1, "logs/refs/tags/a.lock")]
#[case::log_lock_sha256(crate::ObjectFormat::Sha256, "logs/refs/tags/a.lock")]
fn contention_preserves_foreign_lock(#[case] format: crate::ObjectFormat, #[case] path: &str) {
    let (_temp, repo) = fixture(format);
    let lock = repo.git_dir().join(path);
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, b"foreign").unwrap();
    assert!(matches!(
        repo.references()
            .unwrap()
            .transaction(&[edit(format, "refs/tags/a")]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Locked(_),
            ..
        })
    ));
    assert_eq!(fs::read(&lock).unwrap(), b"foreign");
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name("refs/tags/a"))
            .unwrap(),
        None
    );
    fs::remove_file(lock).unwrap();
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn malformed_log_prevents_reference_publication(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    fs::create_dir_all(repo.git_dir().join("logs/refs/tags")).unwrap();
    fs::write(repo.git_dir().join("logs/refs/tags/a"), b"broken").unwrap();
    assert!(matches!(
        repo.references()
            .unwrap()
            .transaction(&[edit(format, "refs/tags/a")]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Malformed { .. },
            ..
        })
    ));
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name("refs/tags/a"))
            .unwrap(),
        None
    );
    clean(&repo);
}

fn deletion(s: &str) -> RefEdit {
    RefEdit {
        name: name(s),
        dereference: false,
        target: None,
        expected: Expected::Any,
        reflog: Reflog::Preserve,
    }
}
fn packed(repo: &Repository) {
    let format = repo.object_format();
    fs::write(
        repo.git_dir().join("packed-refs"),
        format!(
            "{} refs/tags/a\n{} refs/tags/b\n",
            id(format, 1),
            id(format, 2)
        ),
    )
    .unwrap();
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn packed_failure_preserves_loose_and_packed_outcomes(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    packed(&repo);
    let refs = repo.references().unwrap();
    let prepared = refs
        .prepare_files_transaction(&[deletion("refs/tags/a")])
        .unwrap();
    // A directory forces rename failure after successful preparation.
    fs::remove_file(repo.git_dir().join("packed-refs")).unwrap();
    fs::create_dir(repo.git_dir().join("packed-refs")).unwrap();
    let Err(TransactionError::Publish { outcomes, .. }) = prepared.publish() else {
        panic!("expected publication failure")
    };
    assert_eq!(outcomes[0].reference, RefOutcome::Unchanged);
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn packed_first_failure_reports_all_removals_and_retains_loose(
    #[case] format: crate::ObjectFormat,
) {
    let (_temp, repo) = fixture(format);
    packed(&repo);
    let refs = repo.references().unwrap();
    let prepared = refs
        .prepare_files_transaction(&[deletion("refs/tags/a"), deletion("refs/tags/b")])
        .unwrap();
    fs::create_dir(repo.git_dir().join("refs/tags/a")).unwrap();
    let Err(TransactionError::Publish { outcomes, source }) = prepared.publish() else {
        panic!("expected publication failure")
    };
    assert!(matches!(source, ReferenceError::PackedDeleted { .. }));
    assert_eq!(outcomes[0].reference, RefOutcome::PackedRemoved);
    assert_eq!(outcomes[1].reference, RefOutcome::PackedRemoved);
    assert_eq!(fs::read(repo.git_dir().join("packed-refs")).unwrap(), b"");
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn second_ref_failure_retains_first_ref_and_log(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    let prepared = refs
        .prepare_files_transaction(&[edit(format, "refs/tags/a"), edit(format, "refs/tags/b")])
        .unwrap();
    fs::create_dir(repo.git_dir().join("refs/tags/b")).unwrap();
    let Err(TransactionError::Publish { outcomes, .. }) = prepared.publish() else {
        panic!("expected publication failure")
    };
    assert_eq!(outcomes[0].reference, RefOutcome::Published);
    assert_eq!(outcomes[0].logs[0].1, LogOutcome::Appended);
    assert_eq!(outcomes[1].reference, RefOutcome::Unchanged);
    assert_eq!(outcomes[1].logs[0].1, LogOutcome::NotAttempted);
    assert_eq!(
        refs.read(&name("refs/tags/a")).unwrap(),
        Some(Target::Direct(id(format, 1)))
    );
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn log_failure_leaves_published_ref_and_stops_batch(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    let prepared = refs
        .prepare_files_transaction(&[edit(format, "refs/tags/a"), edit(format, "refs/tags/b")])
        .unwrap();
    fs::create_dir(repo.git_dir().join("logs/refs/tags/a")).unwrap();
    let Err(TransactionError::Publish { outcomes, .. }) = prepared.publish() else {
        panic!("expected publication failure")
    };
    assert_eq!(outcomes[0].reference, RefOutcome::Published);
    assert_eq!(
        outcomes[0].logs[0].1,
        LogOutcome::Failed { bytes_written: 0 }
    );
    assert_eq!(outcomes[1].reference, RefOutcome::Unchanged);
    assert_eq!(refs.read(&name("refs/tags/b")).unwrap(), None);
    clean(&repo);
}

#[rstest]
fn short_append_retains_byte_count() {
    struct FailAfterPrefix(Vec<u8>);
    impl Write for FailAfterPrefix {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0.is_empty() {
                self.0.extend_from_slice(&bytes[..3]);
                Ok(3)
            } else {
                Err(io::Error::other("injected"))
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = FailAfterPrefix(Vec::new());
    let (count, _) = append_record(&mut writer, b"record\n").unwrap_err();
    assert_eq!(count, 3);
    assert_eq!(writer.0, b"rec");
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn deletion_keeps_history_and_records_zero(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    refs.transaction(&[edit(format, "refs/tags/a")]).unwrap();
    let mut remove = deletion("refs/tags/a");
    remove.reflog = log();
    refs.transaction(&[remove]).unwrap();
    let entries = refs.reflog(&name("refs/tags/a")).unwrap().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].old, id(format, 1));
    assert_eq!(entries[1].new, id(format, 0));
    assert_eq!(refs.read(&name("refs/tags/a")).unwrap(), None);
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn stored_symbolic_edit_logs_unborn_target(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let mut symbolic = edit(format, "refs/heads/alias");
    symbolic.target = Some(Target::Symbolic(name("refs/heads/missing")));
    let refs = repo.references().unwrap();
    refs.transaction(&[symbolic]).unwrap();
    let entries = refs.reflog(&name("refs/heads/alias")).unwrap().unwrap();
    assert_eq!(entries[0].old, ObjectId::null(format));
    assert_eq!(entries[0].new, ObjectId::null(format));
    assert_eq!(
        repo.references()
            .unwrap()
            .resolve(&name("refs/heads/alias"), 1)
            .unwrap()
            .id,
        None
    );
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn locks_remain_owned_after_ref_publication_until_logs_finish(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    let prepared = refs
        .prepare_files_transaction(&[edit(format, "refs/tags/a")])
        .unwrap();
    prepared.locks[&name("refs/tags/a")]
        .publish_retaining_lock(format!("{}\n", id(format, 1)).as_bytes())
        .unwrap();
    assert!(repo.git_dir().join("refs/tags/a.lock").exists());
    assert!(repo.git_dir().join("packed-refs.lock").exists());
    drop(prepared);
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn failing_second_chain_log_preserves_first_log_outcome(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    let mut head = edit(format, "HEAD");
    head.dereference = true;
    let prepared = refs.prepare_files_transaction(&[head]).unwrap();
    fs::create_dir(repo.git_dir().join("logs/refs/heads/main")).unwrap();
    let Err(TransactionError::Publish { outcomes, .. }) = prepared.publish() else {
        panic!("expected failure")
    };
    assert_eq!(outcomes[0].reference, RefOutcome::Published);
    assert_eq!(
        outcomes[0].logs,
        vec![
            (name("HEAD"), LogOutcome::Appended),
            (
                name("refs/heads/main"),
                LogOutcome::Failed { bytes_written: 0 }
            )
        ]
    );
    assert_eq!(refs.reflog(&name("HEAD")).unwrap().unwrap().len(), 1);
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn invalid_message_fails_before_logs_or_refs_are_created(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let mut operation = edit(format, "refs/tags/a");
    operation.reflog = Reflog::Append {
        committer: Signature {
            name: b"A".to_vec(),
            email: b"a@b".to_vec(),
            seconds: 1,
            offset_minutes: 0,
        },
        message: b"injected\nrecord".to_vec(),
    };
    assert!(matches!(
        repo.references().unwrap().transaction(&[operation]),
        Err(TransactionError::Prepare { .. })
    ));
    assert!(!repo.git_dir().join("logs").exists());
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name("refs/tags/a"))
            .unwrap(),
        None
    );
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn malformed_logs_are_untouched_by_preserve_policy(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    fs::create_dir_all(repo.git_dir().join("logs/refs/tags")).unwrap();
    fs::write(repo.git_dir().join("logs/refs/tags/a"), b"opaque").unwrap();
    let mut operation = edit(format, "refs/tags/a");
    operation.reflog = Reflog::Preserve;
    repo.references()
        .unwrap()
        .transaction(&[operation])
        .unwrap();
    assert_eq!(
        fs::read(repo.git_dir().join("logs/refs/tags/a")).unwrap(),
        b"opaque"
    );
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn packed_namespace_conflict_rejects_whole_batch(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    packed(&repo);
    assert!(matches!(
        repo.references().unwrap().transaction(&[
            edit(format, "refs/tags/new"),
            edit(format, "refs/tags/a/child")
        ]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Conflict(_),
            ..
        })
    ));
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name("refs/tags/new"))
            .unwrap(),
        None
    );
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn resolved_cycle_is_rejected_without_publication(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    refs.update_without_reflog(
        &name("refs/heads/main"),
        Target::Symbolic(name("HEAD")),
        Expected::Absent,
    )
    .unwrap();
    let mut head = edit(format, "HEAD");
    head.dereference = true;
    assert!(matches!(
        refs.transaction(&[head]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Cycle(_),
            ..
        })
    ));
    clean(&repo);
}

fn detach(format: crate::ObjectFormat) -> RefEdit {
    RefEdit {
        name: name("HEAD"),
        dereference: false,
        target: Some(Target::Direct(id(format, 1))),
        expected: Expected::Value(Target::Symbolic(name("refs/heads/main"))),
        reflog: log(),
    }
}
fn old_branch(repo: &Repository, old: Option<ObjectId>) {
    if let Some(id) = old {
        repo.references()
            .unwrap()
            .update_without_reflog(
                &name("refs/heads/main"),
                Target::Direct(id),
                Expected::Absent,
            )
            .unwrap();
    }
}

#[rstest]
#[case::unborn_sha1(crate::ObjectFormat::Sha1, None, 0)]
#[case::unborn_sha256(crate::ObjectFormat::Sha256, None, 0)]
#[case::born_sha1(crate::ObjectFormat::Sha1, Some(2), 2)]
#[case::born_sha256(crate::ObjectFormat::Sha256, Some(2), 2)]
fn stored_detachment_logs_resolved_old_identity_only_on_head(
    #[case] format: crate::ObjectFormat,

    #[case] old: Option<u8>,
    #[case] expected: u8,
) {
    let old = old.map(|n| id(format, n));
    let expected = id(format, expected);
    let (_temp, repo) = fixture(format);
    old_branch(&repo, old);
    let refs = repo.references().unwrap();
    let outcomes = refs.transaction(&[detach(format)]).unwrap();
    assert_eq!(
        refs.read(&name("HEAD")).unwrap(),
        Some(Target::Direct(id(format, 1)))
    );
    assert_eq!(
        refs.read(&name("refs/heads/main")).unwrap(),
        old.map(Target::Direct)
    );
    assert_eq!(outcomes[0].name, name("HEAD"));
    assert_eq!(outcomes[0].logs, vec![(name("HEAD"), LogOutcome::Appended)]);
    let entries = refs.reflog(&name("HEAD")).unwrap().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].old, expected);
    assert_eq!(entries[0].new, id(format, 1));
    assert_eq!(refs.reflog(&name("refs/heads/main")).unwrap(), None);
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn detachment_resolves_multiple_hops_to_packed_old_tip(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    refs.update_without_reflog(
        &name("refs/heads/main"),
        Target::Symbolic(name("refs/heads/old")),
        Expected::Absent,
    )
    .unwrap();
    fs::write(
        repo.git_dir().join("packed-refs"),
        format!("{} refs/heads/old\n", id(format, 2)),
    )
    .unwrap();
    let prepared = refs.prepare_files_transaction(&[detach(format)]).unwrap();
    assert!(prepared.locks.contains_key(&name("HEAD")));
    assert!(prepared.locks.contains_key(&name("refs/heads/main")));
    assert!(prepared.locks.contains_key(&name("refs/heads/old")));
    let outcomes = prepared.publish().unwrap();
    assert_eq!(outcomes[0].logs, vec![(name("HEAD"), LogOutcome::Appended)]);
    assert_eq!(
        refs.reflog(&name("HEAD")).unwrap().unwrap()[0].old,
        id(format, 2)
    );
    assert_eq!(
        refs.read(&name("refs/heads/old")).unwrap(),
        Some(Target::Direct(id(format, 2)))
    );
    assert_eq!(
        refs.read(&name("refs/heads/main")).unwrap(),
        Some(Target::Symbolic(name("refs/heads/old")))
    );
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn detachment_checks_stored_symbolic_value_not_resolved_identity(
    #[case] format: crate::ObjectFormat,
) {
    let (_temp, repo) = fixture(format);
    old_branch(&repo, Some(id(format, 2)));
    let refs = repo.references().unwrap();
    let mut operation = detach(format);
    operation.expected = Expected::Value(Target::Direct(id(format, 2)));
    assert!(matches!(
        refs.transaction(&[operation]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Mismatch { .. },
            ..
        })
    ));
    assert_eq!(
        refs.read(&name("HEAD")).unwrap(),
        Some(Target::Symbolic(name("refs/heads/main")))
    );
    assert_eq!(refs.reflog(&name("HEAD")).unwrap(), None);
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn detachment_locks_unborn_dependency_against_concurrent_creation(
    #[case] format: crate::ObjectFormat,
) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    let prepared = refs.prepare_files_transaction(&[detach(format)]).unwrap();
    let writer = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                repo.references().unwrap().update_without_reflog(
                    &name("refs/heads/main"),
                    Target::Direct(id(format, 2)),
                    Expected::Absent,
                )
            })
            .join()
            .unwrap()
    });
    assert!(matches!(writer, Err(ReferenceError::Locked(_))));
    prepared.publish().unwrap();
    assert_eq!(
        refs.reflog(&name("HEAD")).unwrap().unwrap()[0].old,
        id(format, 0)
    );
    assert_eq!(refs.read(&name("refs/heads/main")).unwrap(), None);
    clean(&repo);
}

#[rstest]
#[case::old_branch_sha1(crate::ObjectFormat::Sha1, "refs/heads/main.lock")]
#[case::old_branch_sha256(crate::ObjectFormat::Sha256, "refs/heads/main.lock")]
#[case::head_log_sha1(crate::ObjectFormat::Sha1, "logs/HEAD.lock")]
#[case::head_log_sha256(crate::ObjectFormat::Sha256, "logs/HEAD.lock")]
fn detachment_dependency_or_log_lock_preserves_head(
    #[case] format: crate::ObjectFormat,
    #[case] path: &str,
) {
    let (_temp, repo) = fixture(format);
    let path = repo.git_dir().join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"foreign").unwrap();
    let refs = repo.references().unwrap();
    assert!(matches!(
        refs.transaction(&[detach(format)]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Locked(_),
            ..
        })
    ));
    assert_eq!(
        refs.read(&name("HEAD")).unwrap(),
        Some(Target::Symbolic(name("refs/heads/main")))
    );
    assert_eq!(refs.reflog(&name("HEAD")).unwrap(), None);
    assert_eq!(fs::read(&path).unwrap(), b"foreign");
    fs::remove_file(path).unwrap();
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn failed_detachment_log_reports_published_head_without_changing_old_branch(
    #[case] format: crate::ObjectFormat,
) {
    let (_temp, repo) = fixture(format);
    old_branch(&repo, Some(id(format, 2)));
    let refs = repo.references().unwrap();
    let prepared = refs.prepare_files_transaction(&[detach(format)]).unwrap();
    fs::create_dir(repo.git_dir().join("logs/HEAD")).unwrap();
    let Err(TransactionError::Publish { outcomes, .. }) = prepared.publish() else {
        panic!("expected append failure")
    };
    assert_eq!(outcomes[0].name, name("HEAD"));
    assert_eq!(outcomes[0].reference, RefOutcome::Published);
    assert_eq!(
        outcomes[0].logs,
        vec![(name("HEAD"), LogOutcome::Failed { bytes_written: 0 })]
    );
    assert_eq!(
        refs.read(&name("HEAD")).unwrap(),
        Some(Target::Direct(id(format, 1)))
    );
    assert_eq!(
        refs.read(&name("refs/heads/main")).unwrap(),
        Some(Target::Direct(id(format, 2)))
    );
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn detachment_rejects_old_chain_cycle_before_publication(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    refs.update_without_reflog(
        &name("refs/heads/main"),
        Target::Symbolic(name("HEAD")),
        Expected::Absent,
    )
    .unwrap();
    assert!(matches!(
        refs.transaction(&[detach(format)]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Cycle(_),
            ..
        })
    ));
    assert_eq!(
        refs.read(&name("HEAD")).unwrap(),
        Some(Target::Symbolic(name("refs/heads/main")))
    );
    assert_eq!(refs.reflog(&name("HEAD")).unwrap(), None);
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn detachment_rejects_batch_edit_of_old_identity_dependency(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    assert!(matches!(
        refs.transaction(&[detach(format), edit(format, "refs/heads/main")]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Conflict(_),
            ..
        })
    ));
    assert_eq!(
        refs.read(&name("HEAD")).unwrap(),
        Some(Target::Symbolic(name("refs/heads/main")))
    );
    assert_eq!(refs.read(&name("refs/heads/main")).unwrap(), None);
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn detachment_does_not_read_or_append_old_branch_log(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    old_branch(&repo, Some(id(format, 2)));
    fs::create_dir_all(repo.git_dir().join("logs/refs/heads")).unwrap();
    fs::write(
        repo.git_dir().join("logs/refs/heads/main"),
        b"opaque old branch log",
    )
    .unwrap();
    repo.references()
        .unwrap()
        .transaction(&[detach(format)])
        .unwrap();
    assert_eq!(
        fs::read(repo.git_dir().join("logs/refs/heads/main")).unwrap(),
        b"opaque old branch log"
    );
    clean(&repo);
}

#[rstest]
#[case::sha1_target(crate::ObjectFormat::Sha1, Some(Target::Direct(ObjectId::Sha256([1;32]))), Expected::Absent)]
#[case::sha256_target(crate::ObjectFormat::Sha256, Some(Target::Direct(ObjectId::Sha1([1;20]))), Expected::Absent)]
#[case::sha1_expectation(crate::ObjectFormat::Sha1, None, Expected::Value(Target::Direct(ObjectId::Sha256([1;32]))))]
#[case::sha256_expectation(crate::ObjectFormat::Sha256, None, Expected::Value(Target::Direct(ObjectId::Sha1([1;20]))))]
fn rejects_wrong_format_before_locking(
    #[case] format: crate::ObjectFormat,
    #[case] target: Option<Target>,
    #[case] expected: Expected,
) {
    let (_root, repo) = fixture(format);
    let lock = repo.git_dir().join("packed-refs.lock");
    fs::write(&lock, b"another owner").unwrap();
    let mut change = edit(format, "refs/heads/new");
    change.target = target;
    change.expected = expected;
    let result = repo.references().unwrap().transaction(&[change]);
    assert!(matches!(
        result,
        Err(TransactionError::Prepare {
            source: ReferenceError::ObjectFormat(_),
            ..
        })
    ));
    assert_eq!(fs::read(lock).unwrap(), b"another owner");
    assert!(!repo.git_dir().join("refs/heads/new").exists());
    assert!(!repo.git_dir().join("logs").exists());
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn absent_or_same_never_overwrites_another_value(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    let mut keep = edit(format, "refs/jj/keep");
    keep.expected = Expected::AbsentOr(keep.target.clone().unwrap());
    refs.transaction(&[keep.clone()]).unwrap();
    refs.transaction(&[keep.clone()]).unwrap();
    refs.update_without_reflog(&keep.name, Target::Direct(id(format, 2)), Expected::Exists)
        .unwrap();
    assert!(matches!(
        refs.transaction(&[keep]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Mismatch { .. },
            ..
        })
    ));
    assert_eq!(
        refs.read(&name("refs/jj/keep")).unwrap(),
        Some(Target::Direct(id(format, 2)))
    );
    assert_eq!(
        refs.reflog(&name("refs/jj/keep")).unwrap().unwrap().len(),
        2
    );
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn must_exist_rejects_absence_before_any_publication(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    let mut required = edit(format, "refs/heads/required");
    required.expected = Expected::Exists;
    assert!(matches!(
        refs.transaction(&[edit(format, "refs/heads/first"), required]),
        Err(TransactionError::Prepare {
            operation: Some(1),
            source: ReferenceError::Mismatch { actual: None }
        })
    ));
    assert_eq!(refs.read(&name("refs/heads/first")).unwrap(), None);
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn explicit_deletion_removes_ref_and_log(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    refs.transaction(&[edit(format, "refs/heads/topic")])
        .unwrap();
    let mut delete = deletion("refs/heads/topic");
    delete.reflog = Reflog::Delete;
    let outcome = refs.transaction(&[delete]).unwrap();
    assert_eq!(
        outcome[0].logs,
        vec![(name("refs/heads/topic"), LogOutcome::Deleted)]
    );
    assert_eq!(refs.read(&name("refs/heads/topic")).unwrap(), None);
    assert_eq!(refs.reflog(&name("refs/heads/topic")).unwrap(), None);
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn symbolic_head_logs_old_and_new_terminal_ids(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    old_branch(&repo, Some(id(format, 1)));
    refs.update_without_reflog(
        &name("refs/heads/next"),
        Target::Direct(id(format, 2)),
        Expected::Absent,
    )
    .unwrap();
    let mut head = edit(format, "HEAD");
    head.target = Some(Target::Symbolic(name("refs/heads/next")));
    head.expected = Expected::Exists;
    refs.transaction(&[head]).unwrap();
    let logs = refs.reflog(&name("HEAD")).unwrap().unwrap();
    assert_eq!(logs[0].old, id(format, 1));
    assert_eq!(logs[0].new, id(format, 2));
    assert_eq!(
        refs.resolve(&name("HEAD"), 32).unwrap().id,
        Some(id(format, 2))
    );
    assert_eq!(refs.reflog(&name("refs/heads/next")).unwrap(), None);
    clean(&repo);
}

#[rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn failed_log_deletion_reports_published_reference(#[case] format: crate::ObjectFormat) {
    let (_temp, repo) = fixture(format);
    let refs = repo.references().unwrap();
    refs.transaction(&[edit(format, "refs/heads/topic")])
        .unwrap();
    let mut delete = deletion("refs/heads/topic");
    delete.reflog = Reflog::Delete;
    let prepared = refs.prepare_files_transaction(&[delete]).unwrap();
    let log_path = repo.git_dir().join("logs/refs/heads/topic");
    fs::remove_file(&log_path).unwrap();
    fs::create_dir(&log_path).unwrap();
    let error = prepared.publish().unwrap_err();
    let TransactionError::Publish { outcomes, source } = error else {
        panic!("publication expected")
    };
    assert_eq!(outcomes[0].reference, RefOutcome::Published);
    assert_eq!(
        outcomes[0].logs[0].1,
        LogOutcome::Failed { bytes_written: 0 }
    );
    assert!(std::error::Error::source(&source).is_some());
    assert_eq!(refs.read(&name("refs/heads/topic")).unwrap(), None);
    assert!(log_path.is_dir());
    clean(&repo);
}
