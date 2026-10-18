//! Original fixtures use Git's public CLI; no upstream implementation or tests are inputs.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::ops::ControlFlow;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::checkout::Limits;
use girt::{InitKind, ObjectFormat, ObjectId, Repository};
use rstest::rstest;

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    git_with_locks(root, args, "0")
}
fn git_with_locks(root: &Path, args: &[&str], locks: &str) -> Vec<u8> {
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
        .env("GIT_OPTIONAL_LOCKS", locks)
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
    source_in(parent, ObjectFormat::Sha1)
}
fn source_in(parent: &Path, format: ObjectFormat) -> Repository {
    git(
        parent,
        &["init", "-q", &format!("--object-format={format}"), "source"],
    );
    let repo = Repository::open(parent.join("source")).unwrap();
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
        .checkout_tree_with(
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
fn no_checkout_clone_then_git_status_and_commit(
    #[case] use_girt: bool,
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let temp = tempfile::tempdir().unwrap();
    let source = source_in(temp.path(), format);
    let destination = temp.path().join("clone");
    clone_without_checkout(source.worktree().unwrap(), &destination, use_girt);
    let repo = Repository::open(&destination).unwrap();
    assert_eq!(repo.object_format(), format);
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
            girt::transport::TransportControl::new(&cancel).with_deadline(None),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    prepared.finish(Default::default(), &cancel).unwrap();
}

#[rstest]
fn tracked_update_leaves_head_and_matches_git_tree_index(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let temp = tempfile::tempdir().unwrap();
    let repo = source_in(temp.path(), format);
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
        .checkout_tree_with(
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

// Git CLI-generated extensions; format contract: https://git-scm.com/docs/gitformat-index.
// The hook reports a full rescan, so the fixture also runs on Linux without a native daemon.
fn cache_status(root: &Path, monitor: &str) -> Vec<u8> {
    git_with_locks(
        root,
        &[
            "-c",
            monitor,
            "-c",
            "core.untrackedCache=true",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        ],
        "1",
    )
}
fn assert_monitor_caches(repo: &Repository) {
    let index = repo.read_index(Default::default()).unwrap().unwrap();
    assert!(index.extensions().iter().any(|e| e.signature() == *b"FSMN"));
    assert!(index.extensions().iter().any(|e| e.signature() == *b"UNTR"));
}

#[rstest]
#[case::hook("core.fsmonitor=.git/hooks/test-monitor")]
#[cfg_attr(target_os = "macos", case::builtin("core.fsmonitor=true"))]
fn checkout_drops_git_monitor_caches_and_git_rebuilds_them(
    #[case] monitor: &str,
    #[values(false, true)] changed: bool,
) {
    let temp = tempfile::tempdir().unwrap();
    let repo = source(temp.path());
    let root = repo.worktree().unwrap();
    let baseline = tree(root);
    fs::write(root.join("dir/raw"), b"replacement\n").unwrap();
    git(root, &["add", "."]);
    let target: ObjectId = std::str::from_utf8(&git(root, &["write-tree"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    git(root, &["reset", "--hard", "HEAD"]);
    let hook = repo.git_dir().join("hooks/test-monitor");
    fs::write(&hook, b"#!/bin/sh\nprintf 'token\\0/\\0'\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(cache_status(root, monitor).is_empty());
    assert_monitor_caches(&repo);
    let report = repo
        .raw_status_with(
            girt::status::Baseline::Head,
            girt::status::Untracked::RawFilesWithoutIgnores,
            &girt::pathspec::Pathspec::all(),
            Default::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.staged.is_empty());
    assert!(report.unstaged.is_empty());
    let mut edit = repo.edit_index(Default::default()).unwrap();
    edit.invalidate_tree_cache().unwrap();
    edit.commit().unwrap();
    assert!(cache_status(root, monitor).is_empty());
    assert_monitor_caches(&repo);
    let target = if changed { target } else { baseline };
    checkout(&repo, Some(baseline), target);
    assert!(
        repo.read_index(Default::default())
            .unwrap()
            .unwrap()
            .extensions()
            .is_empty()
    );
    // Compare against HEAD: checkout deliberately leaves HEAD at the baseline.
    let expected = if changed {
        b"M  dir/raw\n".as_slice()
    } else {
        b""
    };
    assert_eq!(cache_status(root, monitor), expected);
    assert_monitor_caches(&repo);
    fs::write(root.join("run"), b"dirty\n").unwrap();
    fs::write(root.join("untracked"), b"new\n").unwrap();
    let mut expected = expected.to_vec();
    expected.extend_from_slice(b" M run\n?? untracked\n");
    assert_eq!(cache_status(root, monitor), expected);
    #[cfg(target_os = "macos")]
    if monitor == "core.fsmonitor=true" {
        git(root, &["fsmonitor--daemon", "stop"]);
    }
}

#[test]
fn checkout_drops_git_written_entry_offset_accelerators() {
    let temp = tempfile::tempdir().unwrap();
    let repo = source(temp.path());
    let root = repo.worktree().unwrap();
    // Enough entries for Git to emit a multi-block IEOT with two index threads.
    for number in 0..2000 {
        fs::write(root.join(format!("entry-{number:04}")), b"fixture\n").unwrap();
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "offset fixture"]);
    git(
        root,
        &[
            "-c",
            "index.threads=2",
            "-c",
            "index.recordOffsetTable=true",
            "-c",
            "index.recordEndOfIndexEntries=true",
            "update-index",
            "--force-write-index",
        ],
    );
    let index = repo.read_index(Default::default()).unwrap().unwrap();
    assert!(index.extensions().iter().any(|e| e.signature() == *b"EOIE"));
    assert!(index.extensions().iter().any(|e| e.signature() == *b"IEOT"));
    let baseline = tree(root);
    checkout(&repo, Some(baseline), baseline);
    assert!(
        repo.read_index(Default::default())
            .unwrap()
            .unwrap()
            .extensions()
            .is_empty()
    );
    assert!(git(root, &["status", "--porcelain"]).is_empty());
}

#[test]
fn progress_counts_applied_operations_before_publication() {
    let temp = tempfile::tempdir().unwrap();
    let repo = source(temp.path());
    let baseline = tree(repo.worktree().unwrap());
    let mut events = Vec::new();
    let report = repo
        .checkout_tree_with_progress(
            Some(baseline),
            None,
            Limits::default(),
            &AtomicBool::new(false),
            |done, total| events.push((done, total)),
        )
        .unwrap();
    let total = report.applied.len() as u64;
    assert!(total > 0);
    assert_eq!(events.first(), Some(&(0, total)));
    assert_eq!(events.last(), Some(&(total, total)));
    assert!(events.windows(2).all(|pair| pair[0].0 <= pair[1].0));
    assert!(report.index_published);
}

/// Raw checkout creates real symlinks even when Git is configured to write placeholders.
#[test]
fn configured_symlink_placeholders_are_not_written() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path());
    let destination = temp.path().join("clone");
    clone_without_checkout(source.worktree().unwrap(), &destination, false);
    git(&destination, &["config", "core.symlinks", "false"]);
    let repo = Repository::open(&destination).unwrap();
    checkout(&repo, None, tree(source.worktree().unwrap()));
    assert!(
        fs::symlink_metadata(destination.join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(destination.join("link")).unwrap();
    git(
        &destination,
        &[
            "-c",
            "core.symlinks=false",
            "checkout-index",
            "--force",
            "link",
        ],
    );
    assert!(
        !fs::symlink_metadata(destination.join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(destination.join("link")).unwrap(), b"dir/raw");
}
