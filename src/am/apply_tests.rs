//! Composition fault acceptance: retain working mutations when index publication fails.
#![cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs;
use std::sync::atomic::AtomicBool;

use crate::{OperationLimits, Repository};

#[path = "../../tests/support/apply_git.rs"]
mod oracle;
#[path = "../../tests/support/am_git.rs"]
mod fixture;

#[test]
fn late_index_failure_preserves_working_write_report_and_am_state() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fixture::stopped(root, "sha1", false);
    let repo = Repository::open(root).unwrap();
    let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
    let head = oracle::git(root, &["rev-parse", "HEAD"], b"");
    let error = super::stage::apply_with_hook(
        &repo,
        &snapshot,
        super::ApplyLimits::default(),
        &AtomicBool::new(false),
        || fs::write(repo.git_dir().join("index"), b"independent replacement").unwrap(),
    ).unwrap_err();
    assert!(!error.worktree.writes.is_empty());
    assert!(error.worktree.writes.iter().all(|write| write.complete));
    assert_eq!(fs::read(root.join("file")).unwrap(), b"changed\n-- \n");
    assert_eq!(fs::read(root.join("new")).unwrap(), b"new file\n");
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), b"independent replacement");
    assert_eq!(fs::read(repo.git_dir().join("rebase-apply/next")).unwrap(), b"1\n");
    assert_eq!(oracle::git(root, &["rev-parse", "HEAD"], b""), head);
    assert!(!repo.git_dir().join("index.lock").exists());
}
