//! Public worktree administration behavior compared with Git's own reader.
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use girt::refs::{Backend, RefName};
use girt::{InitKind, ObjectFormat, Repository, WorktreeAdminError};
use rstest::rstest;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[rstest]
#[case::sha1_files(ObjectFormat::Sha1, Backend::Files)]
#[case::sha256_reftable(ObjectFormat::Sha256, Backend::Reftable)]
fn repairs_moved_checkout_and_common_repository(
    #[case] format: ObjectFormat,
    #[case] backend: Backend,
) {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().join("before");
    fs::create_dir(&base).unwrap();
    let repo =
        Repository::init_with_backend(format, base.join("main"), InitKind::Worktree, backend)
            .unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let linked = repo
        .create_orphan_worktree(base.join("topic"), &branch, 2)
        .unwrap();
    let name = linked.git_dir().file_name().unwrap().to_owned();
    let moved = root.path().join("after");
    fs::rename(&base, &moved).unwrap();
    let repo = Repository::open(moved.join("main")).unwrap();
    let registration = repo.common_dir().join("worktrees").join(name);
    let checkout = moved.join("topic");
    repo.repair_worktree(&registration, &checkout).unwrap();
    let reopened = Repository::open(&checkout).unwrap();
    assert_eq!(reopened.common_dir(), repo.common_dir());
    git(&checkout, &["symbolic-ref", "HEAD"]);
    git(&checkout, &["status", "--porcelain"]);
}

#[test]
fn prune_requires_expiry_absence_and_no_lock() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let linked = repo
        .create_orphan_worktree(root.path().join("topic"), &branch, 2)
        .unwrap();
    let registration = linked.git_dir().to_owned();
    let future = SystemTime::now() + Duration::from_secs(60);
    assert!(matches!(
        repo.prune_worktree(&registration, future),
        Err(WorktreeAdminError::Protected(_))
    ));
    repo.lock_worktree(&registration, "keep").unwrap();
    fs::rename(root.path().join("topic"), root.path().join("moved")).unwrap();
    assert!(matches!(
        repo.prune_worktree(&registration, future),
        Err(WorktreeAdminError::Protected(_))
    ));
    repo.unlock_worktree(&registration, "keep").unwrap();
    assert!(matches!(
        repo.prune_worktree(&registration, SystemTime::UNIX_EPOCH),
        Err(WorktreeAdminError::Protected(_))
    ));
    repo.prune_worktree(&registration, future).unwrap();
    assert!(!registration.exists());
    assert!(root.path().join("moved").exists());
}

#[test]
fn repair_rewrites_relative_links_after_checkout_move() {
    let root = tempfile::tempdir().unwrap();
    let main = root.path().join("main");
    let repo = Repository::init(ObjectFormat::Sha256, &main, InitKind::Worktree).unwrap();
    let config = repo.common_dir().join("config");
    let mut bytes = fs::read(&config).unwrap();
    bytes.extend_from_slice(b"[extensions]\nrelativeWorktrees = true\n");
    fs::write(&config, bytes).unwrap();
    let repo = Repository::open(&main).unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let old = root.path().join("old");
    let linked = repo.create_orphan_worktree(&old, &branch, 2).unwrap();
    let registration = linked.git_dir().to_owned();
    let moved = root.path().join("moved");
    fs::rename(&old, &moved).unwrap();
    repo.repair_worktree(&registration, &moved).unwrap();
    let forward = fs::read(moved.join(".git")).unwrap();
    assert!(!forward.starts_with(b"gitdir: /"));
    git(&moved, &["symbolic-ref", "HEAD"]);
}

#[test]
fn stale_admin_lock_and_foreign_gitfile_refuse_mutation() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let checkout = root.path().join("topic");
    let linked = repo.create_orphan_worktree(&checkout, &branch, 2).unwrap();
    let registration = linked.git_dir();
    let guard = registration.join("girt-admin.lock");
    fs::write(&guard, b"leftover").unwrap();
    assert!(matches!(
        repo.prune_worktree(registration, SystemTime::now() + Duration::from_secs(60)),
        Err(WorktreeAdminError::Busy(_))
    ));
    fs::remove_file(&guard).unwrap();
    let gitfile = checkout.join(".git");
    fs::write(&gitfile, b"gitdir: /unrelated/live\n").unwrap();
    assert!(matches!(
        repo.repair_worktree(registration, &checkout),
        Err(WorktreeAdminError::Protected(_))
    ));
    assert_eq!(fs::read(&gitfile).unwrap(), b"gitdir: /unrelated/live\n");
}
