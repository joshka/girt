//! Original isolated Git am interoperability and no-write acceptance cases.
#![cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs;
use std::sync::atomic::AtomicBool;

use girt::am::{ApplyLimits, apply_current};
use girt::{OperationLimits, Repository};
use rstest::rstest;

#[path = "support/apply_git.rs"]
mod oracle;
use oracle::git;
#[path = "support/am_git.rs"]
mod fixture;
use fixture::stopped;

#[rstest]
fn git_continues_the_staged_mail(
    #[values("sha1", "sha256")] format: &str,
    #[values(false, true)] binary: bool,
) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let expected = stopped(root, format, binary);
    let repo = Repository::open(root).unwrap();
    let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
    let head = git(root, &["rev-parse", "HEAD"], b"");
    let report = apply_current(
        &repo,
        &snapshot,
        ApplyLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(!report.writes.is_empty());
    assert!(report.writes.iter().all(|write| write.complete));
    assert_eq!(git(root, &["rev-parse", "HEAD"], b""), head);
    assert_eq!(
        fs::read(repo.git_dir().join("rebase-apply/next")).unwrap(),
        b"1\n"
    );
    assert!(git(root, &["diff", "--name-only"], b"").is_empty());
    git(root, &["am", "--continue"], b"");
    assert_eq!(git(root, &["rev-parse", "HEAD"], b""), expected);
    git(root, &["fsck", "--no-reflogs"], b"");
}

#[rstest]
#[case("dirty")]
#[case("staged")]
#[case("obstruction")]
#[case("state_changed")]
#[case("locked")]
#[case("hunk")]
#[case("cancel")]
#[case("limit")]
fn preparation_failure_preserves_repository(#[case] failure: &str) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    stopped(root, "sha1", false);
    let repo = Repository::open(root).unwrap();
    let mut snapshot = repo.operation_state(OperationLimits::default()).unwrap();
    let mut limits = ApplyLimits::default();
    match failure {
        "dirty" | "staged" => {
            fs::write(root.join("file"), b"local work\n").unwrap();
            if failure == "staged" {
                git(root, &["add", "file"], b"");
            }
        }
        "obstruction" => fs::write(root.join("new"), b"precious untracked\n").unwrap(),
        "state_changed" => fs::write(repo.git_dir().join("rebase-apply/next"), b"2\n").unwrap(),
        "locked" => fs::write(repo.git_dir().join("index.lock"), b"held\n").unwrap(),
        "hunk" => {
            fs::write(repo.git_dir().join("rebase-apply/patch"), b"diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-wrong preimage\n+changed\n").unwrap();
            snapshot = repo.operation_state(OperationLimits::default()).unwrap();
        }
        "limit" => limits.patch.max_input_bytes = 1,
        "cancel" => {}
        _ => unreachable!(),
    }
    let index = fs::read(repo.git_dir().join("index")).unwrap();
    let file = fs::read(root.join("file")).unwrap();
    let new = fs::read(root.join("new")).ok();
    let head = git(root, &["rev-parse", "HEAD"], b"");
    let next = fs::read(repo.git_dir().join("rebase-apply/next")).unwrap();
    let error = apply_current(
        &repo,
        &snapshot,
        limits,
        &AtomicBool::new(failure == "cancel"),
    )
    .unwrap_err();
    assert!(error.worktree.writes.is_empty());
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), index);
    assert_eq!(fs::read(root.join("file")).unwrap(), file);
    assert_eq!(fs::read(root.join("new")).ok(), new);
    assert_eq!(git(root, &["rev-parse", "HEAD"], b""), head);
    assert_eq!(
        fs::read(repo.git_dir().join("rebase-apply/next")).unwrap(),
        next
    );
    if failure == "locked" {
        assert_eq!(
            fs::read(repo.git_dir().join("index.lock")).unwrap(),
            b"held\n"
        );
    }
}

#[test]
fn foreign_snapshot_cannot_select_another_repositories_patch() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    stopped(a.path(), "sha1", false);
    stopped(b.path(), "sha1", true);
    let a = Repository::open(a.path()).unwrap();
    let b = Repository::open(b.path()).unwrap();
    let snapshot = a.operation_state(OperationLimits::default()).unwrap();
    let index = fs::read(b.git_dir().join("index")).unwrap();
    let error = apply_current(
        &b,
        &snapshot,
        ApplyLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(error.worktree.writes.is_empty());
    assert_eq!(fs::read(b.git_dir().join("index")).unwrap(), index);
    assert_eq!(
        fs::read(b.worktree().unwrap().join("file")).unwrap(),
        b"base\n"
    );
}
