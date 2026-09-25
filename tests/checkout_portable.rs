//! Portable refusal contracts; this suite does not claim Windows checkout support.
use std::sync::atomic::AtomicBool;

use girt::checkout::{Error, Limits, Stage};
use girt::{InitKind, Repository};

#[test]
fn cancellation_precedes_index_access() {
    let temp = tempfile::tempdir().unwrap();
    let repo = Repository::init(temp.path().join("repo"), InitKind::Worktree).unwrap();
    std::fs::write(repo.git_dir().join("index"), b"invalid").unwrap();
    let failure = repo
        .checkout_tree(None, None, Limits::default(), &AtomicBool::new(true))
        .unwrap_err();
    assert!(matches!(*failure.cause, Error::Cancelled));
    assert_eq!(failure.report.stage, Stage::Preparation);
    assert!(!repo.git_dir().join("index.lock").exists());
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[test]
fn unsupported_platform_does_not_lock_or_parse_index() {
    let temp = tempfile::tempdir().unwrap();
    let repo = Repository::init(temp.path().join("repo"), InitKind::Worktree).unwrap();
    std::fs::write(repo.git_dir().join("index"), b"invalid").unwrap();
    let failure = repo
        .checkout_tree(None, None, Limits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(*failure.cause, Error::Refused { .. }));
    assert!(!repo.git_dir().join("index.lock").exists());
}
