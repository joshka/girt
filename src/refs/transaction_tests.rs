use std::fs;

use rstest::rstest;

use super::*;
use crate::{Repository, Signature};

fn fixture() -> (tempfile::TempDir, Repository) {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("objects")).unwrap();
    fs::create_dir(temp.path().join("refs")).unwrap();
    fs::write(temp.path().join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    let repo = Repository::open(temp.path()).unwrap();
    (temp, repo)
}
fn name(s: &str) -> RefName {
    RefName::new(s).unwrap()
}
fn id(n: u8) -> ObjectId {
    ObjectId::from_bytes([n; 20])
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
fn edit(s: &str) -> RefEdit {
    RefEdit {
        name: name(s),
        dereference: false,
        target: Some(Target::Direct(id(1))),
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

#[test]
fn resolved_head_and_second_ref_log_exact_ids() {
    let (_temp, repo) = fixture();
    let refs = repo.references().unwrap();
    let mut head = edit("HEAD");
    head.dereference = true;
    let result = refs.transaction(&[head, edit("refs/tags/v1")]).unwrap();
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
    assert_eq!(entries[0].old, id(0));
    assert_eq!(entries[0].new, id(1));
    assert_eq!(entries[0].message, b"publish\tbatch");
    clean(&repo);
}

#[test]
fn final_mismatch_preserves_all_refs_and_logs() {
    let (_temp, repo) = fixture();
    let refs = repo.references().unwrap();
    let before = fs::read(repo.git_dir().join("HEAD")).unwrap();
    let result = refs.transaction(&[edit("refs/tags/a"), edit("HEAD")]);
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
#[case::duplicate("refs/heads/a", "refs/heads/a")]
#[case::ancestor("refs/heads/a", "refs/heads/a/b")]
#[case::descendant("refs/heads/a/b", "refs/heads/a")]
fn rejects_conflicting_batch(#[case] first: &str, #[case] second: &str) {
    let (_temp, repo) = fixture();
    assert!(matches!(
        repo.references()
            .unwrap()
            .transaction(&[edit(first), edit(second)]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Conflict(_),
            ..
        })
    ));
    clean(&repo);
}

#[test]
fn overlapping_symbolic_destination_is_rejected() {
    let (_temp, repo) = fixture();
    let mut head = edit("HEAD");
    head.dereference = true;
    assert!(matches!(
        repo.references()
            .unwrap()
            .transaction(&[head, edit("refs/heads/main")]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Conflict(_),
            ..
        })
    ));
    clean(&repo);
}

#[rstest]
#[case::packed("packed-refs.lock")]
#[case::ref_lock("refs/tags/a.lock")]
#[case::log_lock("logs/refs/tags/a.lock")]
fn contention_preserves_foreign_lock(#[case] path: &str) {
    let (_temp, repo) = fixture();
    let lock = repo.git_dir().join(path);
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, b"foreign").unwrap();
    assert!(matches!(
        repo.references()
            .unwrap()
            .transaction(&[edit("refs/tags/a")]),
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

#[test]
fn malformed_log_prevents_reference_publication() {
    let (_temp, repo) = fixture();
    fs::create_dir_all(repo.git_dir().join("logs/refs/tags")).unwrap();
    fs::write(repo.git_dir().join("logs/refs/tags/a"), b"broken").unwrap();
    assert!(matches!(
        repo.references()
            .unwrap()
            .transaction(&[edit("refs/tags/a")]),
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
    fs::write(
        repo.git_dir().join("packed-refs"),
        format!("{} refs/tags/a\n{} refs/tags/b\n", id(1), id(2)),
    )
    .unwrap();
}

#[test]
fn packed_failure_preserves_loose_and_packed_outcomes() {
    let (_temp, repo) = fixture();
    packed(&repo);
    let refs = repo.references().unwrap();
    let prepared = refs
        .prepare_transaction(&[deletion("refs/tags/a")])
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

#[test]
fn packed_first_failure_reports_all_removals_and_retains_loose() {
    let (_temp, repo) = fixture();
    packed(&repo);
    let refs = repo.references().unwrap();
    let prepared = refs
        .prepare_transaction(&[deletion("refs/tags/a"), deletion("refs/tags/b")])
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

#[test]
fn second_ref_failure_retains_first_ref_and_log() {
    let (_temp, repo) = fixture();
    let refs = repo.references().unwrap();
    let prepared = refs
        .prepare_transaction(&[edit("refs/tags/a"), edit("refs/tags/b")])
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
        Some(Target::Direct(id(1)))
    );
    clean(&repo);
}

#[test]
fn log_failure_leaves_published_ref_and_stops_batch() {
    let (_temp, repo) = fixture();
    let refs = repo.references().unwrap();
    let prepared = refs
        .prepare_transaction(&[edit("refs/tags/a"), edit("refs/tags/b")])
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

#[test]
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

#[test]
fn deletion_keeps_history_and_records_zero() {
    let (_temp, repo) = fixture();
    let refs = repo.references().unwrap();
    refs.transaction(&[edit("refs/tags/a")]).unwrap();
    let mut remove = deletion("refs/tags/a");
    remove.reflog = log();
    refs.transaction(&[remove]).unwrap();
    let entries = refs.reflog(&name("refs/tags/a")).unwrap().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].old, id(1));
    assert_eq!(entries[1].new, id(0));
    assert_eq!(refs.read(&name("refs/tags/a")).unwrap(), None);
    clean(&repo);
}

#[test]
fn stored_symbolic_edit_requires_preserve() {
    let (_temp, repo) = fixture();
    let mut symbolic = edit("refs/heads/alias");
    symbolic.target = Some(Target::Symbolic(name("refs/heads/missing")));
    assert!(matches!(
        repo.references().unwrap().transaction(&[symbolic.clone()]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Unsupported(_),
            ..
        })
    ));
    symbolic.reflog = Reflog::Preserve;
    repo.references().unwrap().transaction(&[symbolic]).unwrap();
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

#[test]
fn locks_remain_owned_after_ref_publication_until_logs_finish() {
    let (_temp, repo) = fixture();
    let refs = repo.references().unwrap();
    let prepared = refs.prepare_transaction(&[edit("refs/tags/a")]).unwrap();
    prepared.locks[&name("refs/tags/a")]
        .publish_retaining_lock(format!("{}\n", id(1)).as_bytes())
        .unwrap();
    assert!(repo.git_dir().join("refs/tags/a.lock").exists());
    assert!(repo.git_dir().join("packed-refs.lock").exists());
    drop(prepared);
    clean(&repo);
}

#[test]
fn failing_second_chain_log_preserves_first_log_outcome() {
    let (_temp, repo) = fixture();
    let refs = repo.references().unwrap();
    let mut head = edit("HEAD");
    head.dereference = true;
    let prepared = refs.prepare_transaction(&[head]).unwrap();
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

#[test]
fn invalid_message_fails_before_logs_or_refs_are_created() {
    let (_temp, repo) = fixture();
    let mut operation = edit("refs/tags/a");
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

#[test]
fn malformed_logs_are_untouched_by_preserve_policy() {
    let (_temp, repo) = fixture();
    fs::create_dir_all(repo.git_dir().join("logs/refs/tags")).unwrap();
    fs::write(repo.git_dir().join("logs/refs/tags/a"), b"opaque").unwrap();
    let mut operation = edit("refs/tags/a");
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

#[test]
fn packed_namespace_conflict_rejects_whole_batch() {
    let (_temp, repo) = fixture();
    packed(&repo);
    assert!(matches!(
        repo.references()
            .unwrap()
            .transaction(&[edit("refs/tags/new"), edit("refs/tags/a/child")]),
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

#[test]
fn resolved_cycle_is_rejected_without_publication() {
    let (_temp, repo) = fixture();
    let refs = repo.references().unwrap();
    refs.update_without_reflog(
        &name("refs/heads/main"),
        Target::Symbolic(name("HEAD")),
        Expected::Absent,
    )
    .unwrap();
    let mut head = edit("HEAD");
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
