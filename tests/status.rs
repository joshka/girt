//! Original fixtures built with Git's public CLI, with normalization and rename detection disabled.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::index::{Entry, Index, Mode, Stage};
use girt::status::{Baseline, Change, Comparison, Error, Limits, Report, Untracked};
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
        .env("GIT_AUTHOR_NAME", "Status Fixture")
        .env("GIT_AUTHOR_EMAIL", "status@example.invalid")
        .env("GIT_COMMITTER_NAME", "Status Fixture")
        .env("GIT_COMMITTER_EMAIL", "status@example.invalid")
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
fn fixture() -> (tempfile::TempDir, Repository) {
    let temp = tempfile::tempdir().unwrap();
    let repo = Repository::init(temp.path().join("repo"), InitKind::Worktree).unwrap();
    (temp, repo)
}
fn status(repo: &Repository) -> Report {
    repo.raw_status(
        Baseline::Head,
        Untracked::RawFilesWithoutIgnores,
        Limits::default(),
        &AtomicBool::new(false),
    )
    .unwrap()
}
fn set_file(root: &Path, bytes: Option<&[u8]>) {
    if let Some(bytes) = bytes {
        fs::write(root.join("file"), bytes).unwrap();
    } else if root.join("file").exists() {
        fs::remove_file(root.join("file")).unwrap();
    }
}
fn seed(repo: &Repository, initial: Option<&[u8]>, staged: Option<&[u8]>, work: Option<&[u8]>) {
    let root = repo.worktree().unwrap();
    set_file(root, initial);
    git(root, &["add", "-A"]);
    git(root, &["commit", "--allow-empty", "-qm", "baseline"]);
    set_file(root, staged);
    git(root, &["add", "-A"]);
    set_file(root, work);
}

#[rstest]
#[case::clean(Some(b"old".as_slice()), Some(b"old".as_slice()), Some(b"old".as_slice()), b"", 0, 0)]
#[case::unstaged(Some(b"old".as_slice()), Some(b"old".as_slice()), Some(b"new".as_slice()), b" M file\0", 0, 1)]
#[case::staged(Some(b"old".as_slice()), Some(b"new".as_slice()), Some(b"new".as_slice()), b"M  file\0", 1, 0)]
#[case::both(Some(b"old".as_slice()), Some(b"new".as_slice()), Some(b"end".as_slice()), b"MM file\0", 1, 1)]
#[case::added(None, Some(b"new".as_slice()), Some(b"new".as_slice()), b"A  file\0", 1, 0)]
#[case::added_changed(None, Some(b"new".as_slice()), Some(b"end".as_slice()), b"AM file\0", 1, 1)]
#[case::removed(Some(b"old".as_slice()), None, None, b"D  file\0", 1, 0)]
#[case::missing(Some(b"old".as_slice()), Some(b"old".as_slice()), None, b" D file\0", 0, 1)]
#[case::added_missing(None, Some(b"new".as_slice()), None, b"AD file\0", 1, 1)]
fn agrees_with_git_staged_and_unstaged(
    #[case] initial: Option<&[u8]>,
    #[case] staged: Option<&[u8]>,
    #[case] work: Option<&[u8]>,
    #[case] expected: &[u8],
    #[case] staged_count: usize,
    #[case] unstaged_count: usize,
) {
    let (_temp, repo) = fixture();
    seed(&repo, initial, staged, work);
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    let result = status(&repo);
    assert_eq!(result.staged.len(), staged_count);
    assert_eq!(result.unstaged.len(), unstaged_count);
    assert_eq!(result.comparison, Comparison::RawBytesAndPosixModes);
    assert_eq!(
        git(
            repo.worktree().unwrap(),
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--no-renames"
            ]
        ),
        expected
    );
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[test]
fn unborn_without_index_is_empty_and_does_not_create_storage() {
    let (_temp, repo) = fixture();
    fs::write(repo.worktree().unwrap().join("raw"), b"new").unwrap();
    let result = status(&repo);
    assert!(result.index_missing);
    assert_eq!(result.baseline_tree, None);
    assert!(result.staged.is_empty());
    assert_eq!(result.raw_untracked, [b"raw".to_vec()]);
    assert!(!repo.git_dir().join("index").exists());
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[test]
fn unborn_staged_file_is_addition() {
    let (_temp, repo) = fixture();
    fs::write(repo.worktree().unwrap().join("file"), b"new").unwrap();
    git(repo.worktree().unwrap(), &["add", "file"]);
    let result = status(&repo);
    assert_eq!(result.staged[0].old, None);
    assert_eq!(
        result.staged[0].new.unwrap().id,
        ObjectId::for_blob(girt::ObjectFormat::Sha1, b"new")
    );
    assert!(result.unstaged.is_empty());
}

#[test]
fn missing_index_with_existing_head_reports_deletions_and_raw_files() {
    let (_temp, repo) = fixture();
    seed(&repo, Some(b"old"), Some(b"old"), Some(b"old"));
    fs::remove_file(repo.git_dir().join("index")).unwrap();
    let result = status(&repo);
    assert_eq!(result.staged[0].new, None);
    assert!(result.index_missing);
    assert_eq!(result.raw_untracked, [b"file".to_vec()]);
}

#[rstest]
#[case::loose(false)]
#[case::packed(true)]
fn detached_head_and_explicit_tree_use_verified_storage(#[case] packed: bool) {
    let (_temp, repo) = fixture();
    seed(&repo, Some(b"old"), Some(b"old"), Some(b"new"));
    let root = repo.worktree().unwrap();
    git(root, &["checkout", "--detach", "-q"]);
    pack(root, packed);
    let head = status(&repo);
    let explicit = repo
        .raw_status(
            Baseline::Tree(head.baseline_tree),
            Untracked::Omit,
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(head.unstaged, explicit.unstaged);
    assert_eq!(head.staged, explicit.staged);
}
fn pack(root: &Path, packed: bool) {
    if packed {
        git(root, &["repack", "-ad"]);
        git(root, &["prune-packed"]);
    }
}

#[test]
fn conflicts_are_explicit_and_not_staged_deletions() {
    let (_temp, repo) = fixture();
    seed(&repo, Some(b"old"), Some(b"old"), Some(b"old"));
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"old");
    let mut ours = Entry::new(b"file".to_vec(), Mode::Regular, id);
    ours.stage = Stage::Ours;
    let mut theirs = ours.clone();
    theirs.stage = Stage::Theirs;
    let index = Index::new(vec![ours, theirs], Default::default()).unwrap();
    fs::write(
        repo.git_dir().join("index"),
        index.encode(Default::default()).unwrap(),
    )
    .unwrap();
    let result = status(&repo);
    assert_eq!(result.unmerged.len(), 2);
    assert!(result.staged.is_empty());
    assert!(result.unstaged.is_empty());
    assert_eq!(
        git(
            repo.worktree().unwrap(),
            &["status", "--porcelain=v1", "-z"]
        ),
        b"AA file\0"
    );
}

#[test]
fn assume_valid_does_not_claim_unverified_content_is_clean() {
    let (_temp, repo) = fixture();
    seed(&repo, Some(b"old"), Some(b"old"), Some(b"new"));
    git(
        repo.worktree().unwrap(),
        &["update-index", "--assume-unchanged", "file"],
    );
    assert_eq!(status(&repo).unstaged.len(), 1);
    assert_eq!(
        git(
            repo.worktree().unwrap(),
            &["status", "--porcelain=v1", "-z"]
        ),
        b""
    );
}

#[test]
fn raw_policy_exposes_ignored_files_and_normalization_difference() {
    let (_temp, repo) = fixture();
    let root = repo.worktree().unwrap();
    fs::write(root.join(".gitignore"), b"ignored\n").unwrap();
    fs::write(root.join(".gitattributes"), b"file text eol=lf\n").unwrap();
    fs::write(root.join("ignored"), b"private").unwrap();
    fs::write(root.join("file"), b"line\r\n").unwrap();
    git(root, &["add", "file", ".gitignore", ".gitattributes"]);
    let result = status(&repo);
    assert_eq!(result.untracked_policy, Untracked::RawFilesWithoutIgnores);
    assert_eq!(result.raw_untracked, [b"ignored".to_vec()]);
    assert_eq!(result.unstaged[0].path, b"file");
    let omitted = repo
        .raw_status(
            Baseline::Head,
            Untracked::Omit,
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(omitted.raw_untracked.is_empty());
    assert_eq!(omitted.untracked_policy, Untracked::Omit);
}

#[test]
fn gitlinks_are_explicitly_unchecked_and_never_traversed() {
    let (_temp, repo) = fixture();
    let root = repo.worktree().unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/secret"), b"not scanned").unwrap();
    let entry = Entry::new(
        b"sub".to_vec(),
        Mode::Gitlink,
        ObjectId::for_blob(girt::ObjectFormat::Sha1, b"not a local commit"),
    );
    let index = Index::new(vec![entry], Default::default()).unwrap();
    fs::write(
        repo.git_dir().join("index"),
        index.encode(Default::default()).unwrap(),
    )
    .unwrap();
    let result = status(&repo);
    assert_eq!(result.unchecked_gitlinks.len(), 1);
    assert!(result.raw_untracked.is_empty());
    assert!(result.unstaged.is_empty());
}

#[test]
fn symlink_type_change_matches_git() {
    let (_temp, repo) = fixture();
    seed(&repo, Some(b"old"), Some(b"old"), None);
    symlink("old", repo.worktree().unwrap().join("file")).unwrap();
    let result = status(&repo);
    assert!(
        matches!(result.unstaged[0].change, Change::Modified(value) if value.mode == girt::EntryMode::Symlink)
    );
    assert_eq!(
        git(
            repo.worktree().unwrap(),
            &["status", "--porcelain=v1", "-z"]
        ),
        b" T file\0"
    );
}

#[test]
fn file_directory_replacement_preserves_both_staged_paths() {
    let (_temp, repo) = fixture();
    seed(&repo, Some(b"old"), Some(b"old"), None);
    let root = repo.worktree().unwrap();
    fs::create_dir(root.join("file")).unwrap();
    fs::write(root.join("file/child"), b"new").unwrap();
    git(root, &["add", "-A"]);
    let result = status(&repo);
    assert_eq!(result.staged.len(), 2);
    assert_eq!(result.staged[0].path, b"file");
    assert_eq!(result.staged[0].new, None);
    assert_eq!(result.staged[1].path, b"file/child");
    assert_eq!(result.staged[1].old, None);
    assert!(result.unstaged.is_empty());
}

#[test]
fn directory_obstruction_is_not_clean() {
    let (_temp, repo) = fixture();
    seed(&repo, Some(b"old"), Some(b"old"), None);
    fs::create_dir(repo.worktree().unwrap().join("file")).unwrap();
    let result = status(&repo);
    assert_eq!(
        result.unstaged[0].change,
        Change::Obstructed {
            path: b"file".to_vec()
        }
    );
}

#[test]
fn invalid_index_is_an_error_without_writes() {
    let (_temp, repo) = fixture();
    fs::write(repo.git_dir().join("index"), b"invalid").unwrap();
    let result = repo.raw_status(
        Baseline::Head,
        Untracked::Omit,
        Limits::default(),
        &AtomicBool::new(false),
    );
    assert!(matches!(result, Err(Error::Index(_))));
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), b"invalid");
}

#[test]
fn missing_index_blob_is_an_error_even_when_working_file_is_missing() {
    let (_temp, repo) = fixture();
    let index = Index::new(
        vec![Entry::new(
            b"file".to_vec(),
            Mode::Regular,
            ObjectId::for_blob(girt::ObjectFormat::Sha1, b"absent"),
        )],
        Default::default(),
    )
    .unwrap();
    fs::write(
        repo.git_dir().join("index"),
        index.encode(Default::default()).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        repo.raw_status(
            Baseline::Head,
            Untracked::Omit,
            Limits::default(),
            &AtomicBool::new(false)
        ),
        Err(Error::InvalidObject { .. })
    ));
}

#[test]
fn cancellation_precedes_any_storage_read() {
    let (_temp, repo) = fixture();
    fs::write(repo.git_dir().join("index"), b"invalid").unwrap();
    assert!(matches!(
        repo.raw_status(
            Baseline::Head,
            Untracked::Omit,
            Limits::default(),
            &AtomicBool::new(true)
        ),
        Err(Error::Cancelled)
    ));
}

#[test]
fn linked_worktree_uses_its_own_head_and_index() {
    let (temp, repo) = fixture();
    seed(&repo, Some(b"old"), Some(b"old"), Some(b"old"));
    let linked = temp.path().join("linked");
    git(
        repo.worktree().unwrap(),
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    fs::write(linked.join("file"), b"linked").unwrap();
    git(&linked, &["add", "file"]);
    let other = Repository::open(&linked).unwrap();
    assert_eq!(status(&other).staged.len(), 1);
    assert!(status(&repo).staged.is_empty());
    assert!(status(&other).raw_untracked.is_empty());
}

#[test]
fn separate_gitdir_inside_worktree_is_a_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "--separate-git-dir", "metadata", "."]);
    let repo = Repository::open(&root).unwrap();
    fs::write(root.join("file"), b"new").unwrap();
    git(&root, &["add", "file"]);
    let result = status(&repo);
    assert_eq!(result.staged.len(), 1);
    assert_eq!(result.boundaries, [b"metadata".to_vec()]);
    assert!(result.raw_untracked.is_empty());
}

#[test]
fn wrong_kind_index_object_is_an_error() {
    let (_temp, repo) = fixture();
    let tree = girt::Tree::new(vec![]).unwrap();
    let id = repo.loose_objects().unwrap().write_tree(&tree).unwrap();
    let index = Index::new(
        vec![Entry::new(b"file".to_vec(), Mode::Regular, id)],
        Default::default(),
    )
    .unwrap();
    fs::write(
        repo.git_dir().join("index"),
        index.encode(Default::default()).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        repo.raw_status(
            Baseline::Tree(None),
            Untracked::Omit,
            Limits::default(),
            &AtomicBool::new(false)
        ),
        Err(Error::InvalidObject {
            reason: "wrong object kind",
            ..
        })
    ));
}

#[test]
fn corrupt_object_is_not_hidden_by_equal_working_content() {
    let (_temp, repo) = fixture();
    seed(&repo, Some(b"old"), Some(b"old"), Some(b"old"));
    let hex = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"old").to_string();
    let path = repo.object_dir().join(&hex[..2]).join(&hex[2..]);
    fs::remove_file(&path).unwrap();
    fs::write(path, b"corrupt").unwrap();
    assert!(matches!(
        repo.raw_status(
            Baseline::Head,
            Untracked::Omit,
            Limits::default(),
            &AtomicBool::new(false)
        ),
        Err(Error::Object { .. })
    ));
}

#[test]
fn missing_baseline_tree_is_not_an_empty_tree() {
    let (_temp, repo) = fixture();
    assert!(matches!(
        repo.raw_status(
            Baseline::Tree(Some(ObjectId::for_blob(
                girt::ObjectFormat::Sha1,
                b"absent"
            ))),
            Untracked::Omit,
            Limits::default(),
            &AtomicBool::new(false)
        ),
        Err(Error::Tree(_))
    ));
}

#[rstest]
#[case::object_bytes(Limits { max_object_bytes: 0, ..Limits::default() })]
#[case::tree_reads(Limits { trees: girt::TreeCompareLimits { max_trees: 0, ..Default::default() }, ..Limits::default() })]
#[case::index_bytes(Limits { index: girt::index::Limits { max_bytes: 0, ..Default::default() }, ..Limits::default() })]
fn workflow_enforces_composed_resource_bounds(#[case] limits: Limits) {
    let (_temp, repo) = fixture();
    seed(&repo, Some(b"old"), Some(b"old"), Some(b"old"));
    assert!(
        repo.raw_status(
            Baseline::Head,
            Untracked::Omit,
            limits,
            &AtomicBool::new(false)
        )
        .is_err()
    );
}

#[test]
fn unsafe_index_bytes_do_not_reach_filesystem_traversal() {
    let (_temp, repo) = fixture();
    let index = Index::new(
        vec![Entry::new(
            b".GIT/config".to_vec(),
            Mode::Regular,
            ObjectId::for_blob(girt::ObjectFormat::Sha1, b"missing"),
        )],
        Default::default(),
    )
    .unwrap();
    fs::write(
        repo.git_dir().join("index"),
        index.encode(Default::default()).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        repo.raw_status(
            Baseline::Head,
            Untracked::Omit,
            Limits::default(),
            &AtomicBool::new(false)
        ),
        Err(Error::Unsupported { .. })
    ));
}

#[test]
fn raw_observation_does_not_modify_any_repository_file() {
    let (_temp, repo) = fixture();
    seed(&repo, Some(b"old"), Some(b"new"), Some(b"end"));
    let before = snapshot(repo.worktree().unwrap());
    let result = status(&repo);
    assert_eq!(
        result.staged[0].old.unwrap().id,
        ObjectId::for_blob(girt::ObjectFormat::Sha1, b"old")
    );
    assert_eq!(
        result.staged[0].new.unwrap().id,
        ObjectId::for_blob(girt::ObjectFormat::Sha1, b"new")
    );
    assert!(
        matches!(result.unstaged[0].change, Change::Modified(value) if value.id == ObjectId::for_blob(girt::ObjectFormat::Sha1, b"end"))
    );
    assert_eq!(snapshot(repo.worktree().unwrap()), before);
}

type FileSnapshot = Vec<(std::path::PathBuf, Vec<u8>, std::time::SystemTime)>;
fn snapshot(root: &Path) -> FileSnapshot {
    let mut values = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = fs::symlink_metadata(&path).unwrap();
        if metadata.is_dir() {
            values.extend(snapshot(&path));
        } else {
            values.push((
                path.clone(),
                fs::read(path).unwrap(),
                metadata.modified().unwrap(),
            ));
        }
    }
    values.sort();
    values
}
