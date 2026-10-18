//! Original fixtures built with Git's public CLI, with normalization and rename detection disabled.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::index::{Entry, Index, Mode, Stage, Stat, Timestamp};
use girt::status::{
    Baseline, Change, Comparison, Error, Limits, Report, Untracked, WorktreeChange,
};
use girt::{InitKind, ObjectFormat, ObjectId, Repository};
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
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
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
fn fixture_in(format: ObjectFormat) -> (tempfile::TempDir, Repository) {
    let temp = tempfile::tempdir().unwrap();
    git(
        temp.path(),
        &["init", "-q", &format!("--object-format={format}"), "repo"],
    );
    let repo = Repository::open(temp.path().join("repo")).unwrap();
    (temp, repo)
}
fn fixture() -> (tempfile::TempDir, Repository) {
    let temp = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        girt::ObjectFormat::Sha1,
        temp.path().join("repo"),
        InitKind::Worktree,
    )
    .unwrap();
    (temp, repo)
}
fn status(repo: &Repository) -> Report {
    repo.raw_status_with(
        Baseline::Head,
        Untracked::RawFilesWithoutIgnores,
        &girt::pathspec::Pathspec::all(),
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
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (_temp, repo) = fixture_in(format);
    seed(&repo, initial, staged, work);
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    let result = status(&repo);
    let head_tree = git(repo.worktree().unwrap(), &["rev-parse", "HEAD^{tree}"]);
    assert_eq!(
        result.baseline_tree.map(|tree| tree.to_string()).as_deref(),
        Some(std::str::from_utf8(&head_tree).unwrap().trim())
    );
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
    assert_eq!(porcelain(&result), expected);
    assert_identities(&repo, &result);
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!repo.git_dir().join("index.lock").exists());
}
/// Renders girt's report as Git's `--porcelain=v1 -z` line for the single `file` path.
fn porcelain(report: &Report) -> Vec<u8> {
    let staged = report.staged.iter().find(|c| c.path == b"file");
    let unstaged = report.unstaged.iter().find(|c| c.path == b"file");
    let x = match staged {
        None => b' ',
        Some(change) if change.old.is_none() => b'A',
        Some(change) if change.new.is_none() => b'D',
        Some(_) => b'M',
    };
    let y = match unstaged.map(|c| &c.change) {
        None => b' ',
        Some(Change::Deleted) => b'D',
        Some(Change::Modified(_)) => b'M',
        Some(Change::Obstructed { .. }) => panic!("unexpected obstruction"),
        Some(change) => panic!("unsupported change in porcelain test: {change:?}"),
    };
    if (x, y) == (b' ', b' ') {
        return Vec::new();
    }
    [&[x, y, b' '][..], b"file\0"].concat()
}
/// Checks every reported identity against Git's HEAD tree, index and working-file hash.
fn assert_identities(repo: &Repository, report: &Report) {
    let root = repo.worktree().unwrap();
    let id = |args: &[&str]| -> ObjectId {
        std::str::from_utf8(&git(root, args))
            .unwrap()
            .split_whitespace()
            .find(|field| field.len() >= 40)
            .unwrap()
            .parse()
            .unwrap()
    };
    let indexed = || id(&["ls-files", "--stage", "--", "file"]);
    for change in &report.staged {
        assert_eq!(change.path, b"file");
        if let Some(old) = change.old {
            assert_eq!(old.id, id(&["rev-parse", "HEAD:file"]));
        }
        if let Some(new) = change.new {
            assert_eq!(new.id, indexed());
        }
    }
    for change in &report.unstaged {
        assert_eq!(change.path, b"file");
        assert_eq!(change.index.id, indexed());
        if let Change::Modified(value) = &change.change {
            assert_eq!(value.id, id(&["hash-object", "--no-filters", "file"]));
        }
    }
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
fn detached_head_and_explicit_tree_use_verified_storage(
    #[case] packed: bool,
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (_temp, repo) = fixture_in(format);
    seed(&repo, Some(b"old"), Some(b"old"), Some(b"new"));
    let root = repo.worktree().unwrap();
    git(root, &["checkout", "--detach", "-q"]);
    pack(root, packed);
    let head = status(&repo);
    let explicit = repo
        .raw_status_with(
            Baseline::Tree(head.baseline_tree),
            Untracked::Omit,
            &girt::pathspec::Pathspec::all(),
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
    let index = Index::new(
        girt::ObjectFormat::Sha1,
        vec![ours, theirs],
        Default::default(),
    )
    .unwrap();
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
    // Git cached the stat words of the CRLF file but stored the converted blob. A trusted entry
    // would compare equal, as in `git status`, so force a read to observe the raw bytes.
    make_racy(&repo);
    let result = status(&repo);
    assert_eq!(result.untracked_policy, Untracked::RawFilesWithoutIgnores);
    assert_eq!(result.raw_untracked, [b"ignored".to_vec()]);
    assert_eq!(result.unstaged[0].path, b"file");
    let omitted = repo
        .raw_status_with(
            Baseline::Head,
            Untracked::Omit,
            &girt::pathspec::Pathspec::all(),
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
    let index = Index::new(girt::ObjectFormat::Sha1, vec![entry], Default::default()).unwrap();
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
    let result = repo.raw_status_with(
        Baseline::Head,
        Untracked::Omit,
        &girt::pathspec::Pathspec::all(),
        Limits::default(),
        &AtomicBool::new(false),
    );
    assert!(matches!(result, Err(Error::Index(_))));
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), b"invalid");
}

#[test]
fn missing_index_blob_is_not_read_when_working_file_is_missing() {
    let (_temp, repo) = fixture();
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"absent");
    write_index(&repo, vec![Entry::new(b"file".to_vec(), Mode::Regular, id)]);
    let result = repo
        .raw_status_with(
            Baseline::Tree(None),
            Untracked::Omit,
            &girt::pathspec::Pathspec::all(),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(matches!(result.unstaged[0].change, Change::Deleted));
    assert_eq!(result.unstaged[0].index.id, id);
    // Git does not read index blobs for status either.
    assert_eq!(
        git(
            repo.worktree().unwrap(),
            &["status", "--porcelain=v1", "-z", "--untracked-files=no"]
        ),
        b"AD file\0"
    );
}

#[test]
fn cancellation_precedes_any_storage_read() {
    let (_temp, repo) = fixture();
    fs::write(repo.git_dir().join("index"), b"invalid").unwrap();
    assert!(matches!(
        repo.raw_status_with(
            Baseline::Head,
            Untracked::Omit,
            &girt::pathspec::Pathspec::all(),
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
fn index_identity_is_compared_without_reading_its_object() {
    let (_temp, repo) = fixture();
    let tree = girt::Tree::new(girt::ObjectFormat::Sha1, vec![]).unwrap();
    let id = repo.loose_objects().write_tree(&tree).unwrap();
    write_index(&repo, vec![Entry::new(b"file".to_vec(), Mode::Regular, id)]);
    fs::write(repo.worktree().unwrap().join("file"), b"content").unwrap();
    let result = repo
        .raw_status_with(
            Baseline::Tree(None),
            Untracked::Omit,
            &girt::pathspec::Pathspec::all(),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(matches!(
        result.unstaged[0].change,
        Change::Modified(value) if value.id == ObjectId::for_blob(girt::ObjectFormat::Sha1, b"content")
    ));
}

#[test]
fn corrupt_index_blob_does_not_affect_equal_working_content() {
    let (_temp, repo) = fixture();
    seed(&repo, Some(b"old"), Some(b"old"), Some(b"old"));
    let hex = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"old").to_string();
    let path = repo.object_dir().join(&hex[..2]).join(&hex[2..]);
    fs::remove_file(&path).unwrap();
    fs::write(path, b"corrupt").unwrap();
    let result = status(&repo);
    assert!(result.unstaged.is_empty());
    assert!(
        git(
            repo.worktree().unwrap(),
            &["status", "--porcelain=v1", "-z", "--untracked-files=no"]
        )
        .is_empty()
    );
}

/// Replaces the index with these entries, bypassing girt's racy-entry smudging.
fn write_index(repo: &Repository, entries: Vec<Entry>) {
    let index = Index::new(girt::ObjectFormat::Sha1, entries, Default::default()).unwrap();
    fs::write(
        repo.git_dir().join("index"),
        index.encode(Default::default()).unwrap(),
    )
    .unwrap();
}

/// The stat words Git caches for `path`, read with `lstat`.
fn cached_stat(path: &Path) -> Stat {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = fs::symlink_metadata(path).unwrap();
    let time = |seconds: i64, nanoseconds: i64| Timestamp {
        seconds: seconds as u32,
        nanoseconds: nanoseconds as u32,
    };
    Stat {
        ctime: time(metadata.ctime(), metadata.ctime_nsec()),
        mtime: time(metadata.mtime(), metadata.mtime_nsec()),
        device: metadata.dev() as u32,
        inode: metadata.ino() as u32,
        uid: metadata.uid(),
        gid: metadata.gid(),
        size: metadata.size() as u32,
    }
}

fn set_mtime(path: &Path, time: std::time::SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

/// Makes every cached entry racily clean, so status must read each file.
fn make_racy(repo: &Repository) {
    set_mtime(
        &repo.git_dir().join("index"),
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(1),
    );
}

/// When the index was written relative to the file's cached times.
#[derive(Clone, Copy, Debug)]
enum Written {
    /// At the file's mtime: racily clean.
    SameAsMtime,
    /// After the mtime but before the ctime: Git trusts the entry, girt reads it.
    BeforeCtime,
    /// After both times: the cached words are trusted.
    Later,
}

#[rstest]
#[case::racy_mtime(Written::SameAsMtime, Mode::Regular, false, true)]
#[case::racy_ctime(Written::BeforeCtime, Mode::Regular, false, true)]
#[case::trusted(Written::Later, Mode::Regular, false, false)]
#[case::smudged_size(Written::Later, Mode::Regular, true, true)]
#[case::mode_differs(Written::Later, Mode::Executable, false, true)]
fn matching_stat_words_are_trusted_unless_racy(
    #[case] written: Written,
    #[case] mode: Mode,
    #[case] smudged: bool,
    #[case] read: bool,
) {
    let (_temp, repo) = fixture();
    let file = repo.worktree().unwrap().join("file");
    fs::write(&file, b"actual").unwrap();
    let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    set_mtime(&file, old);
    let mut stat = cached_stat(&file);
    if smudged {
        stat.size = 0;
    }
    // The cached identity names other content, as an index with a racily clean entry can.
    let cached = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"cached");
    write_index(
        &repo,
        vec![Entry::new(b"file".to_vec(), mode, cached).with_stat(stat)],
    );
    let index_time = match written {
        Written::SameAsMtime => old,
        Written::BeforeCtime => old + std::time::Duration::from_secs(10),
        Written::Later => std::time::SystemTime::now() + std::time::Duration::from_secs(10),
    };
    set_mtime(&repo.git_dir().join("index"), index_time);
    let result = repo
        .raw_status_with(
            Baseline::Tree(None),
            Untracked::Omit,
            &girt::pathspec::Pathspec::all(),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    if read {
        assert!(matches!(
            result.unstaged[..],
            [WorktreeChange { change: Change::Modified(value), .. }]
                if value.id == ObjectId::for_blob(girt::ObjectFormat::Sha1, b"actual")
        ));
    } else {
        assert!(result.unstaged.is_empty());
    }
    let git_status = git(
        repo.worktree().unwrap(),
        &["status", "--porcelain=v1", "-z", "--untracked-files=no"],
    );
    // Git compares only the mtime against the index time, so it trusts the ctime case.
    let git_reads = read && !matches!(written, Written::BeforeCtime);
    let expected: &[u8] = if git_reads {
        b"AM file\0"
    } else {
        b"A  file\0"
    };
    assert_eq!(git_status, expected);
}

#[test]
fn missing_baseline_tree_is_not_an_empty_tree() {
    let (_temp, repo) = fixture();
    assert!(matches!(
        repo.raw_status_with(
            Baseline::Tree(Some(ObjectId::for_blob(
                girt::ObjectFormat::Sha1,
                b"absent"
            ))),
            Untracked::Omit,
            &girt::pathspec::Pathspec::all(),
            Limits::default(),
            &AtomicBool::new(false)
        ),
        Err(Error::Tree(_))
    ));
}

#[rstest]
#[case::object_bytes(Limits::default().with_max_object_bytes(0))]
#[case::tree_reads(Limits::default().with_trees(girt::TreeCompareLimits::default().with_max_trees(0)))]
#[case::index_bytes(Limits::default().with_index(girt::index::Limits::default().with_max_bytes(0)))]
fn workflow_enforces_composed_resource_bounds(#[case] limits: Limits) {
    let (_temp, repo) = fixture();
    // A staged change makes the baseline tree differ from the index, so it must be read.
    seed(&repo, Some(b"old"), Some(b"new"), Some(b"new"));
    assert!(
        repo.raw_status_with(
            Baseline::Head,
            Untracked::Omit,
            &girt::pathspec::Pathspec::all(),
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
        girt::ObjectFormat::Sha1,
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
        repo.raw_status_with(
            Baseline::Head,
            Untracked::Omit,
            &girt::pathspec::Pathspec::all(),
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

#[rstest]
#[case::aggregate(5, 8, Some("worktree bytes"))]
#[case::per_file(4, 10, Some("file bytes"))]
#[case::exact(5, 10, None)]
fn distinguishes_worktree_and_file_byte_limits(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(false, true)] links: bool,
    #[case] file_bytes: usize,
    #[case] total_bytes: usize,
    #[case] expected: Option<&str>,
) {
    let (_temp, repo) = fixture_in(format);
    let root = repo.worktree().unwrap();
    for path in ["a", "b"] {
        if links {
            symlink("12345", root.join(path)).unwrap();
        } else {
            fs::write(root.join(path), b"12345").unwrap();
        }
    }
    git(root, &["add", "a", "b"]);
    git(root, &["commit", "-qm", "Original five-byte entries"]);
    assert!(git(root, &["status", "--porcelain=v1", "--untracked-files=no"]).is_empty());
    make_racy(&repo);
    let result = repo.raw_status_with(
        Baseline::Head,
        Untracked::Omit,
        &girt::pathspec::Pathspec::all(),
        Limits::default()
            .with_max_file_bytes(file_bytes)
            .with_max_worktree_bytes(total_bytes),
        &AtomicBool::new(false),
    );
    match expected {
        Some(label) => match result {
            Err(Error::Limit(actual)) => assert_eq!(actual, label),
            other => panic!("expected {label} limit, got {other:?}"),
        },
        None => assert!(result.unwrap().unstaged.is_empty()),
    }
}

/// Git accepts Unicode paths on macOS where raw status and checkout explicitly refuse them.
#[cfg(target_os = "macos")]
#[test]
fn macos_unicode_worktree_paths_are_explicitly_refused() {
    let (_root, repo) = fixture_in(ObjectFormat::Sha1);
    let root = repo.worktree().unwrap();
    fs::write(root.join("café.txt"), b"unicode path\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "unicode audit"]);
    assert!(git(root, &["status", "--porcelain=v1", "-z"]).is_empty());
    let result = repo.raw_status_with(
        Baseline::Head,
        Untracked::RawFilesWithoutIgnores,
        &girt::pathspec::Pathspec::all(),
        Limits::default(),
        &AtomicBool::new(false),
    );
    assert!(matches!(
        result,
        Err(girt::status::Error::Unsupported { .. })
    ));
    let tree: ObjectId = std::str::from_utf8(&git(root, &["rev-parse", "HEAD^{tree}"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    git(root, &["read-tree", "-m", "-u", "HEAD", "HEAD"]);
    assert!(
        repo.checkout_tree_with(
            Some(tree),
            Some(tree),
            girt::checkout::Limits::default(),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert_eq!(fs::read(root.join("café.txt")).unwrap(), b"unicode path\n");
}

/// Git reports intent-to-add as an unstaged addition; girt currently refuses the index flag.
#[test]
fn intent_to_add_status_is_explicitly_refused() {
    let (_root, repo) = fixture_in(ObjectFormat::Sha1);
    let root = repo.worktree().unwrap();
    fs::write(root.join("intent"), b"intent audit\n").unwrap();
    git(root, &["add", "--intent-to-add", "intent"]);
    assert_eq!(
        git(root, &["status", "--porcelain=v1", "-z"]),
        b" A intent\0"
    );
    assert!(matches!(
        repo.raw_status_with(
            Baseline::Head,
            Untracked::RawFilesWithoutIgnores,
            &girt::pathspec::Pathspec::all(),
            Limits::default(),
            &AtomicBool::new(false)
        ),
        Err(girt::status::Error::Unsupported { .. })
    ));
}

/// Raw status does not apply Git's configured executable-bit override.
#[test]
fn configured_filemode_override_is_not_applied() {
    use std::os::unix::fs::PermissionsExt;
    let (_root, repo) = fixture_in(ObjectFormat::Sha1);
    seed(&repo, Some(b"same"), Some(b"same"), Some(b"same"));
    let root = repo.worktree().unwrap();
    fs::set_permissions(root.join("file"), fs::Permissions::from_mode(0o755)).unwrap();
    git(root, &["config", "core.filemode", "false"]);
    let repo = Repository::open(root).unwrap();
    assert!(
        git(
            root,
            &[
                "-c",
                "core.filemode=false",
                "status",
                "--porcelain=v1",
                "-z"
            ]
        )
        .is_empty()
    );
    assert_eq!(status(&repo).unstaged.len(), 1);
}

/// Git can represent a symlink as an ordinary file when core.symlinks is false.
#[test]
fn configured_symlink_override_is_not_applied() {
    let (_root, repo) = fixture_in(ObjectFormat::Sha1);
    let root = repo.worktree().unwrap();
    symlink("target", root.join("file")).unwrap();
    git(root, &["add", "file"]);
    git(root, &["commit", "-m", "symlink policy audit"]);
    fs::remove_file(root.join("file")).unwrap();
    fs::write(root.join("file"), b"target").unwrap();
    git(root, &["config", "core.symlinks", "false"]);
    let repo = Repository::open(root).unwrap();
    assert!(
        git(
            root,
            &[
                "-c",
                "core.symlinks=false",
                "status",
                "--porcelain=v1",
                "-z"
            ]
        )
        .is_empty()
    );
    assert_eq!(status(&repo).unstaged.len(), 1);
}

/// Git treats an unambiguous case alias as the same path when core.ignoreCase is true.
#[cfg(target_os = "macos")]
#[test]
fn case_alias_status_requires_git_policy() {
    let (_root, repo) = fixture_in(ObjectFormat::Sha1);
    seed(&repo, Some(b"same"), Some(b"same"), Some(b"same"));
    let root = repo.worktree().unwrap();
    fs::rename(root.join("file"), root.join("FILE")).unwrap();
    if !root.join("file").exists() {
        eprintln!("skipping case-alias comparison: the fixture volume is case-sensitive");
        return;
    }
    assert!(
        git(
            root,
            &[
                "-c",
                "core.ignorecase=true",
                "status",
                "--porcelain=v1",
                "-z"
            ]
        )
        .is_empty()
    );
    git(root, &["config", "core.ignorecase", "true"]);
    let repo = Repository::open(root).unwrap();
    let observation = repo.raw_status_with(
        Baseline::Head,
        Untracked::RawFilesWithoutIgnores,
        &girt::pathspec::Pathspec::all(),
        Limits::default(),
        &AtomicBool::new(false),
    );
    let observation = observation.unwrap();
    assert_eq!(observation.unstaged.len(), 1);
    assert_eq!(observation.raw_untracked, [b"FILE".to_vec()]);
}

/// Colons and backslashes are ordinary name bytes on macOS and Linux, as they are to Git.
#[rstest]
#[case::colon("colon:name")]
#[case::backslash("back\\slash")]
#[case::nested("dir:x/a\\b")]
fn valid_posix_path_characters_match_git_status(#[case] path: &str) {
    let (_root, repo) = fixture_in(ObjectFormat::Sha1);
    let root = repo.worktree().unwrap();
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(root.join(parent)).unwrap();
    }
    fs::write(root.join(path), b"path audit\n").unwrap();
    git(root, &["add", "--", path]);
    git(root, &["commit", "-m", "path audit"]);
    assert!(git(root, &["status", "--porcelain=v1", "-z"]).is_empty());
    let report = repo
        .raw_status_with(
            Baseline::Head,
            Untracked::RawFilesWithoutIgnores,
            &girt::pathspec::Pathspec::all(),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.staged.is_empty());
    assert!(report.unstaged.is_empty());
    assert!(report.raw_untracked.is_empty());

    fs::write(root.join(path), b"changed\n").unwrap();
    assert_eq!(
        git(root, &["status", "--porcelain=v1", "-z"]),
        format!(" M {path}\0").as_bytes()
    );
    let report = repo
        .raw_status(Baseline::Head, Untracked::RawFilesWithoutIgnores)
        .unwrap();
    assert_eq!(report.unstaged.len(), 1);
    assert_eq!(report.unstaged[0].path, path.as_bytes());
}
