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
    let mail = git(root, &["format-patch", "--stdout", "--root"], b"");
    git(root, &["checkout", "--orphan", "destination"], b"");
    git(root, &["rm", "-rf", "."], b"");
    fs::write(root.join("one"), b"obstruction\n").unwrap();
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
