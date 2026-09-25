//! Original Git CLI fixtures for reftable repository/backend interoperability.
use std::path::Path;
use std::process::Command;

use girt::refs::{Backend, Expected, RefEdit, RefName, Reflog, Target};
use girt::{ObjectFormat, ObjectId, Repository, Signature};
use rstest::rstest;

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "a@b")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "a@b")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn fixture(format: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--ref-format=reftable",
            "--initial-branch=main",
            &format!("--object-format={format}"),
        ],
    );
    git(root.path(), &["commit", "--allow-empty", "-m", "first"]);
    root
}
fn name(bytes: &[u8]) -> RefName {
    RefName::new(bytes).unwrap()
}
fn tip(root: &Path, format: ObjectFormat) -> ObjectId {
    let bytes = git(root, &["rev-parse", "HEAD"]);
    ObjectId::from_hex(format, String::from_utf8(bytes).unwrap().trim()).unwrap()
}
fn log() -> Reflog {
    Reflog::Append {
        committer: Signature {
            name: b"Girt".to_vec(),
            email: b"girt@example.invalid".to_vec(),
            seconds: 1234567890,
            offset_minutes: -480,
        },
        message: b"original publication".to_vec(),
    }
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn reads_nonempty_repository_and_publishes_conditional_refs(
    #[case] spelling: &str,
    #[case] format: ObjectFormat,
) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.reference_backend(), Backend::Reftable);
    let refs = repo.references().unwrap();
    let id = tip(root.path(), format);
    assert_eq!(refs.resolve(&name(b"HEAD"), 32).unwrap().id, Some(id));
    assert_eq!(refs.list().unwrap().len(), 1);
    refs.transaction(&[RefEdit {
        name: name(b"refs/tags/published"),
        target: Some(Target::Direct(id)),
        expected: Expected::Absent,
        dereference: false,
        reflog: log(),
    }])
    .unwrap();
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/tags/published"]),
        format!("{id}\n").as_bytes()
    );
    assert_eq!(
        git(
            root.path(),
            &["reflog", "show", "--format=%gn %gs", "refs/tags/published"]
        ),
        b"Girt original publication\n"
    );
    assert_eq!(
        refs.reflog(&name(b"refs/tags/published")).unwrap().unwrap()[0].new,
        id
    );
    git(root.path(), &["refs", "verify"]);
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn conditional_mismatch_preserves_refs_and_logs(
    #[case] spelling: &str,
    #[case] format: ObjectFormat,
) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    let before = std::fs::read(repo.git_dir().join("reftable/tables.list")).unwrap();
    let refs = repo.references().unwrap();
    let id = tip(root.path(), format);
    assert!(
        refs.update_without_reflog(
            &name(b"refs/heads/main"),
            Target::Direct(id),
            Expected::Absent
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(repo.git_dir().join("reftable/tables.list")).unwrap(),
        before
    );
    assert!(!repo.git_dir().join("reftable/tables.list.lock").exists());
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn resolved_head_logging_and_log_deletion(#[case] spelling: &str, #[case] format: ObjectFormat) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    let refs = repo.references().unwrap();
    let id = tip(root.path(), format);
    refs.transaction(&[RefEdit {
        name: name(b"HEAD"),
        target: Some(Target::Direct(id)),
        expected: Expected::Value(Target::Direct(id)),
        dereference: true,
        reflog: log(),
    }])
    .unwrap();
    assert_eq!(
        refs.reflog(&name(b"HEAD"))
            .unwrap()
            .unwrap()
            .last()
            .unwrap()
            .message,
        b"original publication"
    );
    assert_eq!(
        refs.reflog(&name(b"refs/heads/main"))
            .unwrap()
            .unwrap()
            .last()
            .unwrap()
            .message,
        b"original publication"
    );
    refs.transaction(&[RefEdit {
        name: name(b"refs/heads/main"),
        target: None,
        expected: Expected::Exists,
        dereference: false,
        reflog: Reflog::Delete,
    }])
    .unwrap();
    assert!(refs.read(&name(b"refs/heads/main")).unwrap().is_none());
    assert!(refs.reflog(&name(b"refs/heads/main")).unwrap().is_none());
    git(root.path(), &["reflog", "exists", "HEAD"]);
    git(root.path(), &["refs", "verify"]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn initializes_git_usable_reftable_repository(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        format,
        root.path().join("new"),
        girt::InitKind::Worktree,
        Backend::Reftable,
    )
    .unwrap();
    assert_eq!(
        git(repo.worktree().unwrap(), &["symbolic-ref", "HEAD"]),
        b"refs/heads/main\n"
    );
    git(
        repo.worktree().unwrap(),
        &[
            "commit",
            "--allow-empty",
            "-m",
            "Git after girt initialization",
        ],
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .resolve(&name(b"HEAD"), 32)
            .unwrap()
            .id,
        Some(tip(repo.worktree().unwrap(), format))
    );
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn linked_worktree_routes_head_and_branch_logs(
    #[case] spelling: &str,
    #[case] format: ObjectFormat,
) {
    let root = fixture(spelling);
    let linked = root.path().join("linked");
    git(
        root.path(),
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let repo = Repository::open(&linked).unwrap();
    let id = tip(&linked, format);
    let original_head = git(&linked, &["rev-parse", "ORIG_HEAD"]);
    let refs = repo.references().unwrap();
    let before = git(root.path(), &["reflog", "show", "--format=%gs", "HEAD"]);
    refs.transaction(&[RefEdit {
        name: name(b"HEAD"),
        target: Some(Target::Direct(id)),
        expected: Expected::Exists,
        dereference: true,
        reflog: log(),
    }])
    .unwrap();
    assert_eq!(
        git(root.path(), &["reflog", "show", "--format=%gs", "HEAD"]),
        before
    );
    assert!(
        git(&linked, &["reflog", "show", "--format=%gs", "HEAD"])
            .starts_with(b"original publication\n")
    );
    assert!(
        git(
            &linked,
            &["reflog", "show", "--format=%gs", "refs/heads/linked"]
        )
        .starts_with(b"original publication\n")
    );
    assert_eq!(refs.resolve(&name(b"HEAD"), 32).unwrap().id, Some(id));
    girt::refs::reftable::compact(
        &repo.git_dir().join("reftable"),
        format,
        girt::refs::reftable::StackLimits::default(),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(git(&linked, &["rev-parse", "ORIG_HEAD"]), original_head);
    git(&linked, &["refs", "verify"]);
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn colocation_detaches_head_after_index_publication(
    #[case] spelling: &str,
    #[case] format: ObjectFormat,
) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    let id = tip(root.path(), format);
    let edit = repo
        .edit_colocation(girt::index::Limits::default())
        .unwrap();
    edit.commit(Target::Direct(id), Expected::Exists, log())
        .unwrap();
    assert_eq!(
        repo.references().unwrap().read(&name(b"HEAD")).unwrap(),
        Some(Target::Direct(id))
    );
    assert_eq!(
        git(root.path(), &["rev-parse", "HEAD"]),
        format!("{id}\n").as_bytes()
    );
    git(root.path(), &["refs", "verify"]);
}

#[test]
fn resolves_branch_conditional_config_from_real_reftable_head() {
    let root = fixture("sha1");
    std::fs::write(
        root.path().join(".git/branch-config"),
        b"[test]\nvalue = actual-branch\n",
    )
    .unwrap();
    git(
        root.path(),
        &["config", "includeIf.onbranch:main.path", "branch-config"],
    );
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(
        repo.config().value("test", None, "value"),
        Some(Some(b"actual-branch".as_slice()))
    );
}

#[test]
fn cancellation_preserves_stack_before_transaction() {
    let root = fixture("sha1");
    let repo = Repository::open(root.path()).unwrap();
    let before = std::fs::read(repo.git_dir().join("reftable/tables.list")).unwrap();
    let edit = RefEdit {
        name: name(b"HEAD"),
        target: None,
        expected: Expected::Any,
        dereference: false,
        reflog: Reflog::Preserve,
    };
    assert!(matches!(
        repo.references()
            .unwrap()
            .transaction_controlled(&[edit], &std::sync::atomic::AtomicBool::new(true)),
        Err(girt::refs::TransactionError::Prepare {
            source: girt::refs::ReferenceError::Cancelled,
            ..
        })
    ));
    assert_eq!(
        std::fs::read(repo.git_dir().join("reftable/tables.list")).unwrap(),
        before
    );
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn refuses_stale_expected_value_after_git_publication(
    #[case] spelling: &str,
    #[case] format: ObjectFormat,
) {
    let root = fixture(spelling);
    let old = tip(root.path(), format);
    let repo = Repository::open(root.path()).unwrap();
    git(root.path(), &["commit", "--allow-empty", "-m", "new tip"]);
    let current = tip(root.path(), format);
    let refs = repo.references().unwrap();
    assert!(matches!(
        refs.update_without_reflog(
            &name(b"refs/heads/main"),
            Target::Direct(old),
            Expected::Value(Target::Direct(old))
        ),
        Err(girt::refs::ReferenceError::Mismatch { .. })
    ));
    assert_eq!(refs.resolve(&name(b"HEAD"), 32).unwrap().id, Some(current));
}

#[rstest]
#[case::any(Expected::Any, true)]
#[case::exists(Expected::Exists, true)]
#[case::absent(Expected::Absent, false)]
#[case::same(Expected::Value(Target::Direct(ObjectId::Sha1([1; 20]))), true)]
#[case::different(Expected::Value(Target::Direct(ObjectId::Sha1([3; 20]))), false)]
#[case::absent_or_same(Expected::AbsentOr(Target::Direct(ObjectId::Sha1([1; 20]))), true)]
#[case::absent_or_different(Expected::AbsentOr(Target::Direct(ObjectId::Sha1([3; 20]))), false)]
fn preserves_expected_value_predicates(#[case] expected: Expected, #[case] succeeds: bool) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        ObjectFormat::Sha1,
        root.path().join("repo"),
        girt::InitKind::Bare,
        Backend::Reftable,
    )
    .unwrap();
    let refs = repo.references().unwrap();
    let name = name(b"refs/tags/condition");
    refs.update_without_reflog(
        &name,
        Target::Direct(ObjectId::Sha1([1; 20])),
        Expected::Absent,
    )
    .unwrap();
    assert_eq!(
        refs.update_without_reflog(&name, Target::Direct(ObjectId::Sha1([2; 20])), expected)
            .is_ok(),
        succeeds
    );
}

#[test]
fn rejected_result_budget_leaves_the_stack_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        ObjectFormat::Sha1,
        root.path().join("repo"),
        girt::InitKind::Bare,
        Backend::Reftable,
    )
    .unwrap();
    let before = std::fs::read(repo.git_dir().join("reftable/tables.list")).unwrap();
    let refs = repo
        .references()
        .unwrap()
        .with_reftable_limits(girt::refs::reftable::StackLimits {
            tables: 1,
            ..Default::default()
        });
    assert!(matches!(
        refs.update_without_reflog(
            &name(b"refs/tags/new"),
            Target::Direct(ObjectId::Sha1([1; 20])),
            Expected::Absent
        ),
        Err(girt::refs::ReferenceError::Reftable(
            girt::refs::reftable::Error::Limit(_)
        ))
    ));
    assert_eq!(
        std::fs::read(repo.git_dir().join("reftable/tables.list")).unwrap(),
        before
    );
}
