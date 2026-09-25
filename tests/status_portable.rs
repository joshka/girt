//! Status API platform boundary. Actual descriptor-relative traversal is macOS/Linux only.
use std::sync::atomic::AtomicBool;

use girt::status::{Baseline, Error, Limits, Untracked};
use girt::{InitKind, Repository};

#[test]
fn cancellation_precedes_platform_and_storage_access() {
    let temp = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        girt::ObjectFormat::Sha1,
        temp.path().join("repo"),
        InitKind::Worktree,
    )
    .unwrap();
    std::fs::write(repo.git_dir().join("index"), b"not an index").unwrap();
    let result = repo.raw_status(
        Baseline::Tree(None),
        Untracked::Omit,
        Limits::default(),
        &AtomicBool::new(true),
    );
    assert!(matches!(result, Err(Error::Cancelled)));
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[test]
fn unsupported_platform_fails_before_index_read() {
    let temp = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        girt::ObjectFormat::Sha1,
        temp.path().join("repo"),
        InitKind::Worktree,
    )
    .unwrap();
    std::fs::write(repo.git_dir().join("index"), b"not an index").unwrap();
    let result = repo.raw_status(
        Baseline::Tree(None),
        Untracked::Omit,
        Limits::default(),
        &AtomicBool::new(false),
    );
    assert!(matches!(result, Err(Error::Unsupported { .. })));
}
