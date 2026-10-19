//! Original Git-created unborn am fixtures under isolated configuration.
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use girt::am::CommitLimits;
use girt::refs::{Dereference, RefEdit, RefName, Reflog};
use girt::sequencer::{Action, Limits, State};
use girt::{OperationLimits, Repository, Signature};
use rstest::rstest;

#[path = "support/apply_git.rs"]
mod oracle;
use oracle::{git, git_output};

fn stopped(root: &Path, format: &str, backend: &str) {
    stopped_series(root, format, backend, false);
}

fn stopped_series(root: &Path, format: &str, backend: &str, queued: bool) {
    stopped_count(root, format, backend, if queued { 2 } else { 1 });
}

fn stopped_count(root: &Path, format: &str, backend: &str, count: usize) {
    git(
        root,
        &[
            "init",
            "-b",
            "main",
            &format!("--object-format={format}"),
            &format!("--ref-format={backend}"),
        ],
        b"",
    );
    fs::write(root.join("one"), b"one\n").unwrap();
    git(root, &["add", "."], b"");
    git(root, &["commit", "-m", "root mail"], b"");
    for name in ["two", "three"].into_iter().take(count - 1) {
        fs::write(root.join(name), format!("{name}\n")).unwrap();
        git(root, &["add", name], b"");
        git(root, &["commit", "-m", name], b"");
    }
    let mail = git(root, &["format-patch", "--stdout", "--root"], b"");
    git(root, &["checkout", "--orphan", "destination"], b"");
    git(root, &["rm", "-rf", "."], b"");
    fs::write(root.join("one"), b"obstruction\n").unwrap();
    if count > 1 {
        fs::write(root.join("two"), b"obstruction\n").unwrap();
    }
    assert!(!git_output(root, &["am"], &mail).status.success());
    fs::remove_file(root.join("one")).unwrap();
}

#[rstest]
fn unborn_final_recovery_matches_git(
    #[values("sha1", "sha256")] format: &str,
    #[values("files", "reftable")] backend: &str,
    #[values("continue", "skip", "abort")] action: &str,
) {
    let actual = tempfile::tempdir().unwrap();
    let expected = tempfile::tempdir().unwrap();
    stopped(actual.path(), format, backend);
    stopped(expected.path(), format, backend);
    let repo = Repository::open(actual.path()).unwrap();
    let state = State::read_repository(&repo, Limits::default())
        .unwrap()
        .unwrap();
    assert_eq!(repo.head().unwrap().id, None);
    let writer =
        Signature::parse(git(actual.path(), &["var", "GIT_COMMITTER_IDENT"], b"").trim_ascii())
            .unwrap();
    let commit = if action == "continue" {
        for root in [actual.path(), expected.path()] {
            git(
                root,
                &["apply", "--index"],
                &fs::read(root.join(".git/rebase-apply/patch")).unwrap(),
            );
        }
        let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
        let mail = girt::am::State::from_operation(&snapshot).unwrap().unwrap();
        let tree = git(actual.path(), &["write-tree"], b"");
        let tree = std::str::from_utf8(&tree).unwrap().trim().parse().unwrap();
        let commit = mail
            .prepare_commit(
                tree,
                None,
                writer.clone(),
                CommitLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        repo.loose_objects().write_commit(&commit).unwrap();
        Some(commit)
    } else {
        None
    };
    let decision = match action {
        "continue" => Action::Continue(commit.as_ref().unwrap().id()),
        "skip" => Action::Skip,
        _ => Action::Abort,
    };
    let plan = state.prepare_unborn_am(&repo, decision).unwrap();
    assert_eq!(
        plan.destination(),
        commit.as_ref().map(|commit| commit.id())
    );
    if let Some(commit) = &commit {
        let edit = RefEdit::create(
            RefName::HEAD,
            commit.id(),
            Reflog::Append {
                committer: writer,
                message: plan.reflog_for(commit),
            },
        )
        .with_dereference(Dereference::Follow);
        repo.references().unwrap().transaction(&[edit]).unwrap();
    }
    plan.publish_metadata(&repo).unwrap();
    git(expected.path(), &["am", &format!("--{action}")], b"");
    for args in [
        vec!["symbolic-ref", "HEAD"],
        vec!["ls-files", "--stage"],
        vec!["status", "--porcelain"],
    ] {
        assert_eq!(
            git(actual.path(), &args, b""),
            git(expected.path(), &args, b"")
        );
    }
    assert_eq!(
        repo.head().unwrap().id,
        Repository::open(expected.path())
            .unwrap()
            .head()
            .unwrap()
            .id
    );
    assert!(!repo.git_dir().join("rebase-apply").exists());
    assert!(state.original_head().is_none());
    git(actual.path(), &["fsck", "--no-reflogs"], b"");
}

#[rstest]
#[case("checkpoint")]
#[case("original")]
#[case("queued")]
#[case("born")]
#[case("retarget")]
#[case("foreign")]
#[case("changed")]
fn unsafe_unborn_recovery_keeps_mail(#[case] failure: &str) {
    let root = tempfile::tempdir().unwrap();
    stopped(root.path(), "sha1", "files");
    let repo = Repository::open(root.path()).unwrap();
    let dir = repo.git_dir().join("rebase-apply");
    let source = git(root.path(), &["rev-parse", "main"], b"");
    match failure {
        "checkpoint" => fs::write(dir.join("abort-safety"), &source).unwrap(),
        "original" => fs::write(repo.git_dir().join("ORIG_HEAD"), &source).unwrap(),
        "queued" => {
            fs::write(dir.join("last"), b"2\n").unwrap();
            fs::write(dir.join("0002"), b"mail\n").unwrap();
        }
        "born" => {
            git(
                root.path(),
                &[
                    "update-ref",
                    "refs/heads/destination",
                    std::str::from_utf8(&source).unwrap().trim(),
                ],
                b"",
            );
        }
        _ => (),
    }
    let state = State::read_repository(&repo, Limits::default())
        .unwrap()
        .unwrap();
    if matches!(failure, "checkpoint" | "original" | "queued" | "born") {
        assert!(state.prepare_unborn_am(&repo, Action::Skip).is_err());
    } else {
        let plan = state.prepare_unborn_am(&repo, Action::Skip).unwrap();
        match failure {
            "retarget" => {
                git(
                    root.path(),
                    &["symbolic-ref", "HEAD", "refs/heads/other"],
                    b"",
                );
            }
            "changed" => fs::write(dir.join("patch"), b"changed\n").unwrap(),
            "foreign" => {
                let other = tempfile::tempdir().unwrap();
                stopped(other.path(), "sha1", "files");
                assert!(
                    plan.publish_metadata(&Repository::open(other.path()).unwrap())
                        .is_err()
                );
                assert!(dir.exists());
                return;
            }
            _ => unreachable!(),
        }
        assert!(plan.publish_metadata(&repo).is_err());
    }
    assert!(dir.join("0001").exists());
    assert_eq!(fs::read(dir.join("next")).unwrap(), b"1\n");
}

#[rstest]
fn queued_unborn_mail_matches_git_and_resumes(
    #[values("sha1", "sha256")] format: &str,
    #[values("files", "reftable")] backend: &str,
    #[values(false, true)] continue_root: bool,
) {
    let actual = tempfile::tempdir().unwrap();
    let expected = tempfile::tempdir().unwrap();
    stopped_series(actual.path(), format, backend, true);
    stopped_series(expected.path(), format, backend, true);
    let repo = Repository::open(actual.path()).unwrap();
    let state = State::read_repository(&repo, Limits::default())
        .unwrap()
        .unwrap();
    let writer =
        Signature::parse(git(actual.path(), &["var", "GIT_COMMITTER_IDENT"], b"").trim_ascii())
            .unwrap();
    let commit = if continue_root {
        for root in [actual.path(), expected.path()] {
            git(
                root,
                &["apply", "--index"],
                &fs::read(root.join(".git/rebase-apply/patch")).unwrap(),
            );
        }
        let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
        let mail = girt::am::State::from_operation(&snapshot).unwrap().unwrap();
        let tree = git(actual.path(), &["write-tree"], b"");
        let tree = std::str::from_utf8(&tree).unwrap().trim().parse().unwrap();
        let commit = mail
            .prepare_commit(
                tree,
                None,
                writer.clone(),
                CommitLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        repo.loose_objects().write_commit(&commit).unwrap();
        Some(commit)
    } else {
        None
    };
    let action = commit
        .as_ref()
        .map_or(Action::Skip, |commit| Action::Continue(commit.id()));
    let plan = state
        .prepare_next_unborn_am(
            &repo,
            action,
            girt::mailinfo::Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        plan.destination(),
        commit.as_ref().map(|commit| commit.id())
    );
    if let Some(commit) = &commit {
        let edit = RefEdit::create(
            RefName::HEAD,
            commit.id(),
            Reflog::Append {
                committer: writer,
                message: plan.reflog_for(commit),
            },
        )
        .with_dereference(Dereference::Follow);
        repo.references().unwrap().transaction(&[edit]).unwrap();
    }
    plan.publish_metadata(&repo).unwrap();
    assert!(
        !git_output(
            expected.path(),
            &[
                "am",
                if continue_root {
                    "--continue"
                } else {
                    "--skip"
                }
            ],
            b""
        )
        .status
        .success()
    );
    for name in [
        "next",
        "last",
        "abort-safety",
        "author-script",
        "final-commit",
        "info",
        "msg",
        "patch",
        "0001",
        "0002",
    ] {
        assert_eq!(
            fs::read(repo.git_dir().join("rebase-apply").join(name)).unwrap(),
            fs::read(expected.path().join(".git/rebase-apply").join(name)).unwrap(),
            "{name}"
        );
    }
    for root in [actual.path(), expected.path()] {
        fs::remove_file(root.join("two")).unwrap();
        git(
            root,
            &["apply", "--index"],
            &fs::read(root.join(".git/rebase-apply/patch")).unwrap(),
        );
        git(root, &["am", "--continue"], b"");
    }
    assert_eq!(
        repo.head().unwrap().id,
        Repository::open(expected.path())
            .unwrap()
            .head()
            .unwrap()
            .id
    );
    assert_eq!(
        git(actual.path(), &["ls-files", "--stage"], b""),
        git(expected.path(), &["ls-files", "--stage"], b"")
    );
    assert!(!repo.git_dir().join("rebase-apply").exists());
}

#[rstest]
#[case("mail")]
#[case("limit")]
#[case("cancel")]
#[case("stale")]
fn queued_unborn_failure_keeps_source_mail(#[case] failure: &str) {
    let root = tempfile::tempdir().unwrap();
    stopped_series(root.path(), "sha1", "files", true);
    let repo = Repository::open(root.path()).unwrap();
    let dir = repo.git_dir().join("rebase-apply");
    if failure == "mail" {
        fs::remove_file(dir.join("0002")).unwrap();
    }
    let state = State::read_repository(&repo, Limits::default())
        .unwrap()
        .unwrap();
    let limits = if failure == "limit" {
        girt::mailinfo::Limits::default().with_max_input_bytes(0)
    } else {
        girt::mailinfo::Limits::default()
    };
    let plan = state.prepare_next_unborn_am(
        &repo,
        Action::Skip,
        limits,
        &AtomicBool::new(failure == "cancel"),
    );
    if failure == "stale" {
        fs::write(dir.join("0002"), b"changed\n").unwrap();
        assert!(plan.unwrap().publish_metadata(&repo).is_err());
    } else {
        assert!(plan.is_err());
    }
    assert_eq!(fs::read(dir.join("next")).unwrap(), b"1\n");
    assert_eq!(fs::read(dir.join("abort-safety")).unwrap(), b"");
    assert!(dir.join("0001").exists());
}

fn partially_applied(root: &Path, format: &str, backend: &str, queued_tail: bool) {
    stopped_count(root, format, backend, if queued_tail { 3 } else { 2 });
    git(
        root,
        &["apply", "--index"],
        &fs::read(root.join(".git/rebase-apply/patch")).unwrap(),
    );
    assert!(
        !git_output(root, &["am", "--continue"], b"")
            .status
            .success()
    );
    fs::remove_file(root.join("two")).unwrap();
}

fn clear_and_delete_branch(root: &Path, repo: &Repository, old: girt::ObjectId) {
    let empty = git(root, &["hash-object", "-w", "-t", "tree", "--stdin"], b"");
    git(
        root,
        &[
            "read-tree",
            "--reset",
            "-u",
            std::str::from_utf8(&empty).unwrap().trim(),
        ],
        b"",
    );
    let edit =
        RefEdit::delete(RefName::HEAD, old, Reflog::Delete).with_dereference(Dereference::Follow);
    repo.references().unwrap().transaction(&[edit]).unwrap();
}

#[rstest]
fn abort_after_earlier_mail_restores_unborn_like_git(
    #[values("sha1", "sha256")] format: &str,
    #[values("files", "reftable")] backend: &str,
    #[values(false, true)] queued_tail: bool,
) {
    let actual = tempfile::tempdir().unwrap();
    let expected = tempfile::tempdir().unwrap();
    partially_applied(actual.path(), format, backend, queued_tail);
    partially_applied(expected.path(), format, backend, queued_tail);
    let repo = Repository::open(actual.path()).unwrap();
    let old = repo.head().unwrap().id.unwrap();
    let state = State::read_repository(&repo, Limits::default())
        .unwrap()
        .unwrap();
    assert_eq!(state.original_head(), None);
    let plan = state.prepare_abort_to_unborn_am(&repo, old).unwrap();
    assert_eq!(plan.destination(), None);
    assert!(plan.file_after("rebase-apply/next").is_none());
    // Restoration and conditional branch deletion remain explicit caller-owned operations.
    clear_and_delete_branch(actual.path(), &repo, old);
    plan.publish_metadata(&repo).unwrap();
    git(expected.path(), &["am", "--abort"], b"");
    for args in [
        vec!["symbolic-ref", "HEAD"],
        vec!["ls-files", "--stage"],
        vec!["status", "--porcelain"],
    ] {
        assert_eq!(
            git(actual.path(), &args, b""),
            git(expected.path(), &args, b"")
        );
    }
    assert_eq!(repo.head().unwrap().id, None);
    assert_eq!(
        Repository::open(expected.path())
            .unwrap()
            .head()
            .unwrap()
            .id,
        None
    );
    assert!(
        !repo
            .references()
            .unwrap()
            .has_reflog(&RefName::branch("destination").unwrap())
            .unwrap()
    );
    assert!(!repo.git_dir().join("rebase-apply").exists());
    git(actual.path(), &["fsck", "--no-reflogs"], b"");
}

#[rstest]
#[case("checkpoint")]
#[case("original")]
#[case("expected")]
#[case("not_deleted")]
#[case("retarget")]
#[case("foreign")]
#[case("changed")]
fn unsafe_abort_to_unborn_retains_active_mail(#[case] failure: &str) {
    let root = tempfile::tempdir().unwrap();
    partially_applied(root.path(), "sha1", "files", true);
    let repo = Repository::open(root.path()).unwrap();
    let old = repo.head().unwrap().id.unwrap();
    let dir = repo.git_dir().join("rebase-apply");
    match failure {
        "checkpoint" => fs::write(dir.join("abort-safety"), b"").unwrap(),
        "original" => fs::write(repo.git_dir().join("ORIG_HEAD"), format!("{old}\n")).unwrap(),
        _ => (),
    }
    let state = State::read_repository(&repo, Limits::default())
        .unwrap()
        .unwrap();
    let expected = if failure == "expected" {
        girt::ObjectId::null(repo.object_format())
    } else {
        old
    };
    let plan = state.prepare_abort_to_unborn_am(&repo, expected);
    if matches!(failure, "checkpoint" | "original" | "expected") {
        assert!(plan.is_err());
    } else {
        let plan = plan.unwrap();
        match failure {
            "not_deleted" => (),
            "retarget" => {
                clear_and_delete_branch(root.path(), &repo, old);
                git(
                    root.path(),
                    &["symbolic-ref", "HEAD", "refs/heads/other"],
                    b"",
                );
            }
            "changed" => {
                clear_and_delete_branch(root.path(), &repo, old);
                fs::write(dir.join("patch"), b"changed\n").unwrap();
            }
            "foreign" => {
                let other = tempfile::tempdir().unwrap();
                partially_applied(other.path(), "sha1", "files", true);
                assert!(
                    plan.publish_metadata(&Repository::open(other.path()).unwrap())
                        .is_err()
                );
                assert!(dir.join("0002").exists());
                return;
            }
            _ => unreachable!(),
        }
        assert!(plan.publish_metadata(&repo).is_err());
    }
    assert_eq!(fs::read(dir.join("next")).unwrap(), b"2\n");
    for name in ["0001", "0002", "0003"] {
        assert!(dir.join(name).exists());
    }
}
