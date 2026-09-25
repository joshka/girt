//! Original fixtures use Git's public CLI; no upstream implementation or tests are inputs.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::ops::ControlFlow;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::checkout::Limits;
use girt::{InitKind, ObjectId, Repository};
use rstest::rstest;

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(root)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "index.version=2",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.filemode=true",
            "-c",
            "core.symlinks=true",
            "-c",
            "core.precomposeunicode=false",
            "-c",
            "diff.renames=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Checkout Fixture")
        .env("GIT_AUTHOR_EMAIL", "checkout@example.invalid")
        .env("GIT_COMMITTER_NAME", "Checkout Fixture")
        .env("GIT_COMMITTER_EMAIL", "checkout@example.invalid")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn tree(root: &Path) -> ObjectId {
    std::str::from_utf8(&git(root, &["rev-parse", "HEAD^{tree}"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
fn source(parent: &Path) -> Repository {
    let repo = Repository::init(parent.join("source"), InitKind::Worktree).unwrap();
    let root = repo.worktree().unwrap();
    fs::create_dir(root.join("dir")).unwrap();
    fs::write(root.join("dir/raw"), b"one\r\ntwo\0\xff").unwrap();
    fs::write(root.join("run"), b"#!/bin/sh\necho raw\n").unwrap();
    fs::set_permissions(root.join("run"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("dir/raw", root.join("link")).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "original fixture"]);
    repo
}
fn checkout(repo: &Repository, baseline: Option<ObjectId>, target: ObjectId) {
    let report = repo
        .checkout_tree(
            baseline,
            Some(target),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.index_published);
}

#[rstest]
#[case::git(false)]
#[case::girt(true)]
fn no_checkout_clone_then_git_status_and_commit(#[case] use_girt: bool) {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path());
    let destination = temp.path().join("clone");
    clone_without_checkout(source.worktree().unwrap(), &destination, use_girt);
    let repo = Repository::open(&destination).unwrap();
    let head = git(&destination, &["rev-parse", "HEAD"]);
    assert!(!repo.git_dir().join("index").exists());
    checkout(&repo, None, tree(source.worktree().unwrap()));
    assert_eq!(git(&destination, &["status", "--porcelain=v1", "-z"]), b"");
    assert_eq!(
        git(&destination, &["ls-files", "--stage", "-z"]),
        git(source.worktree().unwrap(), &["ls-files", "--stage", "-z"])
    );
    assert_eq!(
        fs::read(destination.join("dir/raw")).unwrap(),
        b"one\r\ntwo\0\xff"
    );
    assert_eq!(
        fs::read_link(destination.join("link")).unwrap(),
        Path::new("dir/raw")
    );
    assert_eq!(git(&destination, &["rev-parse", "HEAD"]), head);
    fs::write(destination.join("dir/raw"), b"next\n").unwrap();
    git(&destination, &["add", "."]);
    git(&destination, &["commit", "-qm", "after girt checkout"]);
    assert_eq!(git(&destination, &["show", "HEAD:dir/raw"]), b"next\n");
    git(&destination, &["fsck", "--no-dangling"]);
}
fn clone_without_checkout(source: &Path, destination: &Path, use_girt: bool) {
    if !use_girt {
        git(
            source,
            &[
                "clone",
                "--no-checkout",
                "--",
                source.to_str().unwrap(),
                destination.to_str().unwrap(),
            ],
        );
        return;
    }
    let request = girt::clone::CloneRequest::prepare_tracking(
        destination,
        InitKind::Worktree,
        source.to_str().unwrap().as_bytes(),
        girt::clone::BranchSelection::Default,
        girt::refs::Reflog::Preserve,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let prepared = request
        .receive_local(
            source.join(".git"),
            girt::fetch::FetchLimits::default(),
            girt::transport::TransportControl {
                cancel: &cancel,
                deadline: None,
            },
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    prepared.finish(Default::default(), &cancel).unwrap();
}

#[test]
fn tracked_update_leaves_head_and_matches_git_tree_index() {
    let temp = tempfile::tempdir().unwrap();
    let repo = source(temp.path());
    let root = repo.worktree().unwrap();
    let old = tree(root);
    git(root, &["mv", "dir/raw", "renamed"]);
    fs::write(root.join("new"), b"new\n").unwrap();
    fs::remove_file(root.join("run")).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "target"]);
    let target = tree(root);
    let target_entries = git(root, &["ls-files", "--stage", "-z"]);
    git(root, &["checkout", "--detach", "HEAD~1"]);
    let head = git(root, &["rev-parse", "HEAD"]);
    checkout(&repo, Some(old), target);
    assert_eq!(git(root, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(root, &["ls-files", "--stage", "-z"]), target_entries);
    git(root, &["update-index", "--refresh"]);
    assert_eq!(git(root, &["diff-files", "--name-only"]), b"");
    assert_eq!(git(root, &["write-tree"]), format!("{target}\n").as_bytes());
    assert!(!git(root, &["diff-index", "--cached", "--name-only", "HEAD"]).is_empty());
}

#[rstest]
#[case::separate(false)]
#[case::linked(true)]
fn checkout_routes_index_to_correct_worktree(#[case] linked: bool) {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path());
    let root = temp.path().join("worktree");
    make_worktree(source.worktree().unwrap(), &root, linked);
    let repo = Repository::open(&root).unwrap();
    let shared_index = fs::read(source.git_dir().join("index")).unwrap();
    let marker = fs::read(root.join(".git")).unwrap();
    checkout(&repo, None, tree(source.worktree().unwrap()));
    assert_eq!(git(&root, &["status", "--porcelain=v1"]), b"");
    assert_eq!(
        fs::read(source.git_dir().join("index")).unwrap(),
        shared_index
    );
    assert_eq!(fs::read(root.join(".git")).unwrap(), marker);
}
fn make_worktree(source: &Path, root: &Path, linked: bool) {
    if linked {
        git(
            source,
            &[
                "worktree",
                "add",
                "--detach",
                "--no-checkout",
                root.to_str().unwrap(),
                "HEAD",
            ],
        );
    } else {
        let metadata = root.with_extension("git");
        git(
            source,
            &[
                "clone",
                "--no-checkout",
                "--separate-git-dir",
                metadata.to_str().unwrap(),
                source.to_str().unwrap(),
                root.to_str().unwrap(),
            ],
        );
    }
}

#[test]
fn raw_checkout_deliberately_does_not_apply_attributes() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path());
    let root = source.worktree().unwrap();
    fs::write(
        root.join(".gitattributes"),
        b"text.txt text eol=crlf filter=missing\n",
    )
    .unwrap();
    fs::write(root.join("text.txt"), b"line\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "attributes"]);
    let dest = temp.path().join("clone");
    clone_without_checkout(root, &dest, false);
    let repo = Repository::open(&dest).unwrap();
    checkout(&repo, None, tree(root));
    assert_eq!(fs::read(dest.join("text.txt")).unwrap(), b"line\n");
    assert_eq!(git(&dest, &["show", "HEAD:text.txt"]), b"line\n");
    git(&dest, &["checkout-index", "--force", "text.txt"]);
    assert_eq!(fs::read(dest.join("text.txt")).unwrap(), b"line\r\n");
}

#[test]
fn missing_index_requires_empty_explicit_baseline() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path());
    let dest = temp.path().join("clone");
    clone_without_checkout(source.worktree().unwrap(), &dest, false);
    let repo = Repository::open(&dest).unwrap();
    let target = tree(source.worktree().unwrap());
    let error = repo
        .checkout_tree(
            Some(target),
            Some(target),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(error.report.applied.is_empty());
    assert!(!dest.join("dir").exists());
    checkout(&repo, None, target);
}
