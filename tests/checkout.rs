//! Original fixtures use Git's public CLI; no upstream implementation or tests are inputs.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::io::Write as _;
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
    let output = git_output(root, args, locks);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn git_output(root: &Path, args: &[&str], locks: &str) -> std::process::Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
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
        .unwrap()
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

/// Colons and backslashes are ordinary name bytes on macOS and Linux, as they are to Git.
#[test]
fn posix_colon_and_backslash_names_check_out_like_git() {
    let temp = tempfile::tempdir().unwrap();
    git(temp.path(), &["init", "-q", "source"]);
    let source = temp.path().join("source");
    fs::create_dir(source.join("dir:x")).unwrap();
    for path in ["colon:name", "back\\slash", "dir:x/a\\b"] {
        fs::write(source.join(path), path).unwrap();
    }
    git(&source, &["add", "."]);
    git(&source, &["commit", "-qm", "posix names"]);
    let destination = temp.path().join("clone");
    clone_without_checkout(&source, &destination, false);
    let repo = Repository::open(&destination).unwrap();
    checkout(&repo, None, tree(&source));
    assert_eq!(git(&destination, &["status", "--porcelain=v1", "-z"]), b"");
    for path in ["colon:name", "back\\slash", "dir:x/a\\b"] {
        assert_eq!(fs::read(destination.join(path)).unwrap(), path.as_bytes());
    }
}

/// Checkout refuses exactly the metadata-name aliases that Git refuses under the same
/// `core.protectNTFS` and `core.protectHFS` settings, and writes nothing when it refuses.
#[rstest]
fn metadata_name_refusals_match_git(
    #[values(
        ".GIT",
        "git~1",
        "GIT~1",
        ".git ",
        ".git.",
        ".git::$INDEX_ALLOCATION",
        "aux",
        "c:d",
        ".gitx"
    )]
    name: &str,
    #[values(
        None,
        Some("core.protectNTFS=false"),
        Some("core.protectHFS=false"),
        Some("core.protectNTFS=true"),
        Some("core.protectHFS=true")
    )]
    setting: Option<&str>,
) {
    let temp = tempfile::tempdir().unwrap();
    let oracle = temp.path().join("oracle");
    let subject = temp.path().join("subject");
    let mut trees = Vec::new();
    for root in [&oracle, &subject] {
        git(temp.path(), &["init", "-q", root.to_str().unwrap()]);
        if let Some((key, value)) = setting.and_then(|setting| setting.split_once('=')) {
            git(root, &["config", key, value]);
        }
        let blob = git(root, &["hash-object", "-w", "--stdin"]);
        let blob = std::str::from_utf8(&blob).unwrap().trim();
        let subtree = mktree(root, &format!("100644 blob {blob}\tconfig\0"));
        trees.push(mktree(root, &format!("040000 tree {subtree}\t{name}\0")));
    }
    let git_accepts = git_output(&oracle, &["read-tree", "--reset", "-u", &trees[0]], "0")
        .status
        .success();
    let repo = Repository::open(&subject).unwrap();
    let result = repo.checkout_tree_with(
        None,
        Some(trees[1].parse().unwrap()),
        Limits::default(),
        &AtomicBool::new(false),
    );
    assert_eq!(result.is_ok(), git_accepts, "{name:?} with {setting:?}");
    if !git_accepts {
        let entries: Vec<_> = fs::read_dir(&subject)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, [".git"]);
    }
}

fn mktree(root: &Path, entries: &str) -> String {
    git_stdin(root, &["mktree", "-z"], entries)
}

fn git_stdin(root: &Path, args: &[&str], input: &str) -> String {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Writes `files` (path, content) as a tree in `root`'s object store, through the index so nested
/// and colliding paths need no working files.
fn tree_of(root: &Path, files: &[(&str, &str)]) -> String {
    let mut info = String::new();
    for (path, content) in files {
        let blob = git_stdin(root, &["hash-object", "-w", "--stdin"], content);
        info.push_str(&format!("100644 {blob}\t{path}\0"));
    }
    git_stdin(root, &["update-index", "-z", "--index-info"], &info);
    let tree = String::from_utf8(git(root, &["write-tree"])).unwrap();
    fs::remove_file(root.join(".git/index")).unwrap();
    tree.trim().to_owned()
}

/// Every path below `root` other than `.git`, with file contents.
fn snapshot(root: &Path) -> Vec<(Vec<u8>, Option<Vec<u8>>)> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .as_os_str()
                .as_bytes()
                .to_vec();
            if relative == b".git" {
                continue;
            }
            if path.is_dir() {
                found.push((relative, None));
                pending.push(path);
            } else {
                found.push((relative, Some(fs::read(&path).unwrap())));
            }
        }
    }
    found.sort();
    found
}

/// Girt writes the same files as Git for one tree, and Git reports the same status for both.
fn assert_checks_out_like_git(files: &[(&str, &str)]) -> girt::checkout::Report {
    let temp = tempfile::tempdir().unwrap();
    let oracle = temp.path().join("oracle");
    let subject = temp.path().join("subject");
    let mut trees = Vec::new();
    for root in [&oracle, &subject] {
        git(temp.path(), &["init", "-q", root.to_str().unwrap()]);
        trees.push(tree_of(root, files));
    }
    assert_eq!(trees[0], trees[1]);
    git(&oracle, &["read-tree", "--reset", "-u", &trees[0]]);
    let repo = Repository::open(&subject).unwrap();
    let report = repo
        .checkout_tree_with(
            None,
            Some(trees[1].parse().unwrap()),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.index_published);
    assert_eq!(snapshot(&subject), snapshot(&oracle));
    assert_eq!(
        git(&subject, &["ls-files", "--stage", "-z"]),
        git(&oracle, &["ls-files", "--stage", "-z"])
    );
    assert_eq!(
        git(&subject, &["status", "--porcelain=v1", "-z"]),
        git(&oracle, &["status", "--porcelain=v1", "-z"])
    );
    report
}

#[test]
fn unicode_names_check_out_like_git() {
    let report = assert_checks_out_like_git(&[
        ("caf\u{e9}.txt", "composed"),
        ("d\u{e9}cor/cafe\u{301}", "decomposed"),
        ("\u{65e5}\u{672c}/\u{c9}t\u{e9}", "unicode case"),
    ]);
    assert!(report.collisions.is_empty());
}

/// On a case-sensitive Linux volume every path is written; on a default macOS volume one path
/// of each set is, as Git chooses. Either way the files and status match Git's.
#[test]
fn colliding_paths_check_out_like_git() {
    let report = assert_checks_out_like_git(&[
        ("Dir/f", "upper dir"),
        ("X.h", "upper"),
        ("caf\u{e9}", "composed"),
        ("cafe\u{301}", "decomposed"),
        ("dir/f", "lower dir"),
        ("x.h", "lower"),
        ("xt_CONNMARK.h", "same"),
        ("xt_connmark.h", "same"),
    ]);
    if cfg!(target_os = "linux") {
        assert!(report.collisions.is_empty());
    } else {
        assert!(!report.collisions.is_empty());
    }
}

#[rstest]
#[case(ObjectFormat::Sha1, false)]
#[case(ObjectFormat::Sha256, false)]
#[case(ObjectFormat::Sha1, true)]
#[case(ObjectFormat::Sha256, true)]
fn configured_checkout_symlinks_match_git(#[case] format: ObjectFormat, #[case] native: bool) {
    let temp = tempfile::tempdir().unwrap();
    let source = source_in(temp.path(), format);
    let source_root = source.worktree().unwrap();
    let target = tree(source_root);
    let destination = temp.path().join("configured");
    clone_without_checkout(source_root, &destination, false);
    let setting = if native { "true" } else { "false" };
    git(&destination, &["config", "core.symlinks", setting]);
    let repo = Repository::open(&destination).unwrap();

    let report = repo.checkout_tree_configured(None, Some(target)).unwrap();
    assert!(report.index_published);
    assert_eq!(
        fs::symlink_metadata(destination.join("link"))
            .unwrap()
            .is_symlink(),
        native
    );
    if native {
        assert_eq!(
            fs::read_link(destination.join("link")).unwrap(),
            Path::new("dir/raw")
        );
    } else {
        assert_eq!(fs::read(destination.join("link")).unwrap(), b"dir/raw");
    }
    let override_setting = format!("core.symlinks={setting}");
    assert!(
        git(
            &destination,
            &["-c", &override_setting, "status", "--porcelain=v1", "-z"]
        )
        .is_empty()
    );
    assert_eq!(
        git(&destination, &["ls-files", "--stage", "-z"]),
        git(source_root, &["ls-files", "--stage", "-z"])
    );
    repo.checkout_tree_configured(Some(target), Some(target))
        .unwrap();

    let oracle = temp.path().join("oracle");
    clone_without_checkout(source_root, &oracle, false);
    git(
        &oracle,
        &[
            "-c",
            &override_setting,
            "read-tree",
            "--reset",
            "-u",
            "HEAD",
        ],
    );
    assert_eq!(
        fs::symlink_metadata(oracle.join("link"))
            .unwrap()
            .is_symlink(),
        native
    );
    if !native {
        assert_eq!(
            fs::read(oracle.join("link")).unwrap(),
            fs::read(destination.join("link")).unwrap()
        );
        fs::write(destination.join("link"), b"local target").unwrap();
        git(
            &destination,
            &["update-index", "--assume-unchanged", "link"],
        );
        let index = fs::read(repo.git_dir().join("index")).unwrap();
        let failure = repo
            .checkout_tree_configured(Some(target), None)
            .unwrap_err();
        assert!(failure.report.applied.is_empty());
        assert_eq!(fs::read(destination.join("link")).unwrap(), b"local target");
        assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), index);
    }
}

#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn configured_checkout_type_transitions_and_layered_config(#[case] format: ObjectFormat) {
    use girt::config::{Config, ConfigInputs};
    let temp = tempfile::tempdir().unwrap();
    let source = source_in(temp.path(), format);
    let root = source.worktree().unwrap();
    let link_tree = tree(root);
    fs::remove_file(root.join("link")).unwrap();
    fs::write(root.join("link"), b"ordinary file").unwrap();
    git(root, &["add", "link"]);
    git(root, &["commit", "-qm", "replace link with file"]);
    let file_tree = tree(root);
    let destination = temp.path().join("transitions");
    clone_without_checkout(root, &destination, false);
    let default_repo = Repository::open(&destination).unwrap();
    default_repo
        .checkout_tree_configured(None, Some(link_tree))
        .unwrap();
    assert!(
        fs::symlink_metadata(destination.join("link"))
            .unwrap()
            .is_symlink()
    );
    default_repo
        .checkout_tree_configured(Some(link_tree), None)
        .unwrap();

    let inputs = ConfigInputs::default()
        .with_command(Some(Config::parse(b"[core]\nsymlinks=false\n").unwrap()));
    let repo = Repository::open_with_config(&destination, &inputs).unwrap();

    repo.checkout_tree_configured(None, Some(link_tree))
        .unwrap();
    assert!(
        !fs::symlink_metadata(destination.join("link"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(fs::read(destination.join("link")).unwrap(), b"dir/raw");
    repo.checkout_tree_configured(Some(link_tree), Some(file_tree))
        .unwrap();
    assert_eq!(
        fs::read(destination.join("link")).unwrap(),
        b"ordinary file"
    );
    repo.checkout_tree_configured(Some(file_tree), Some(link_tree))
        .unwrap();
    assert_eq!(fs::read(destination.join("link")).unwrap(), b"dir/raw");
    assert!(
        git(
            &destination,
            &["-c", "core.symlinks=false", "diff", "--exit-code"]
        )
        .is_empty()
    );
    repo.checkout_tree_configured(Some(link_tree), None)
        .unwrap();
    assert!(!destination.join("link").exists());
}
