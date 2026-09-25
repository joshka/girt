//! Original fixtures generated through Git's public CLI; no upstream implementation/test source.
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use girt::index::{Entry, Error, Index, Limits, Mode, Stage, StorageError};
use girt::{InitKind, ObjectId, Repository, Tree};
use rstest::rstest;

fn command(root: &Path, args: &[&str]) -> Command {
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
            "core.fsmonitor=false",
            "-c",
            "index.version=2",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Index Fixture")
        .env("GIT_AUTHOR_EMAIL", "index@example.invalid")
        .env("GIT_COMMITTER_NAME", "Index Fixture")
        .env("GIT_COMMITTER_EMAIL", "index@example.invalid")
        .env("LC_ALL", "C");
    command
}
fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = command(root, args).output().unwrap();
    success(output)
}
fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn input(root: &Path, args: &[&str], bytes: &[u8]) -> Vec<u8> {
    let mut child = command(root, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    success(child.wait_with_output().unwrap())
}
fn repository() -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(root.path().join("repo"), InitKind::Worktree).unwrap();
    (root, repo)
}
fn seed(repo: &Repository) {
    let root = repo.worktree().unwrap();
    fs::write(root.join("file"), b"hello\n").unwrap();
    git(root, &["add", "file"]);
}

#[test]
fn reads_git_stat_flags_and_roundtrips_exactly() {
    let (_root, repo) = repository();
    seed(&repo);
    let root = repo.worktree().unwrap();
    git(root, &["update-index", "--assume-unchanged", "file"]);
    let original = fs::read(repo.git_dir().join("index")).unwrap();
    let index = repo.read_index(Limits::default()).unwrap().unwrap();
    let entry = &index.entries()[0];
    assert_eq!(entry.path, b"file");
    assert_eq!(
        entry.id,
        ObjectId::for_blob(girt::ObjectFormat::Sha1, b"hello\n")
    );
    assert_eq!(entry.mode, Mode::Regular);
    assert_eq!(entry.stage, Stage::Normal);
    assert!(entry.assume_valid);
    let expected = format!(
        "file\n  ctime: {}:{}\n  mtime: {}:{}\n  dev: {}\tino: {}\n  uid: {}\tgid: {}\n  size: {}\tflags: 8000\n",
        entry.stat.ctime.seconds,
        entry.stat.ctime.nanoseconds,
        entry.stat.mtime.seconds,
        entry.stat.mtime.nanoseconds,
        entry.stat.device,
        entry.stat.inode,
        entry.stat.uid,
        entry.stat.gid,
        entry.stat.size
    );
    assert_eq!(git(root, &["ls-files", "--debug"]), expected.as_bytes());
    assert_eq!(index.encode(Limits::default()).unwrap(), original);
    repo.edit_index(Limits::default())
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(git(root, &["ls-files", "--debug"]), expected.as_bytes());
}

#[rstest]
#[case::regular(Mode::Regular, "100644")]
#[case::executable(Mode::Executable, "100755")]
#[case::symlink(Mode::Symlink, "120000")]
#[case::gitlink(Mode::Gitlink, "160000")]
fn git_reads_written_modes_and_writes_expected_tree(#[case] mode: Mode, #[case] spelling: &str) {
    let (_root, repo) = repository();
    let root = repo.worktree().unwrap();
    let id = repo
        .loose_objects()
        .unwrap()
        .write_blob(b"payload")
        .unwrap();
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    edit.replace_entries(vec![Entry::new(b"leaf".to_vec(), mode, id)])
        .unwrap();
    edit.commit().unwrap();
    assert_eq!(
        git(root, &["ls-files", "--stage"]),
        format!("{spelling} {id} 0\tleaf\n").as_bytes()
    );
    // Git does not resolve gitlink targets in this superproject; --missing-ok also makes that
    // explicit. Tree bytes independently verify index-to-object compatibility for every mode.
    let tree_id = String::from_utf8(git(root, &["write-tree", "--missing-ok"])).unwrap();
    let payload = git(root, &["cat-file", "tree", tree_id.trim()]);
    let tree = Tree::parse(&payload).unwrap();
    tree.validate().unwrap();
    assert_eq!(tree.entries()[0].id, id);
    assert_eq!(tree.entries()[0].name, b"leaf");
    assert_eq!(tree.id().to_string(), tree_id.trim());
    assert!(
        repo.read_index(Limits::default())
            .unwrap()
            .unwrap()
            .extensions()
            .iter()
            .any(|e| e.signature() == *b"TREE")
    );
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    edit.replace_entries(Vec::new()).unwrap();
    edit.commit().unwrap();
    assert_eq!(
        git(root, &["write-tree"]),
        b"4b825dc642cb6eb9a060e54bf8d69288fbee4904\n"
    );
}

#[test]
fn git_conflict_stages_and_byte_paths_roundtrip() {
    let (_root, repo) = repository();
    let root = repo.worktree().unwrap();
    let id = repo.loose_objects().unwrap().write_blob(b"data").unwrap();
    let mut records =
        format!("100644 {id} 1\tconflict\0100755 {id} 3\tconflict\0120000 {id} 0\t").into_bytes();
    records.extend_from_slice(b"non-utf8-\xff\0");
    input(root, &["update-index", "-z", "--index-info"], &records);
    let index = repo.read_index(Limits::default()).unwrap().unwrap();
    assert_eq!(
        index.entries().iter().map(|e| e.stage).collect::<Vec<_>>(),
        [Stage::Base, Stage::Theirs, Stage::Normal]
    );
    assert_eq!(index.entries()[2].path, b"non-utf8-\xff");
    repo.edit_index(Limits::default())
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(git(root, &["ls-files", "--stage", "-z"]), records);
}

#[test]
fn git_long_paths_need_no_filesystem_materialization() {
    let (_root, repo) = repository();
    let root = repo.worktree().unwrap();
    let id = repo.loose_objects().unwrap().write_blob(b"data").unwrap();
    let mut records = format!("100644 {id} 0\t").into_bytes();
    records.extend_from_slice(&vec![b'x'; 5000]);
    records.push(0);
    input(root, &["update-index", "-z", "--index-info"], &records);
    let index = repo.read_index(Limits::default()).unwrap().unwrap();
    assert_eq!(index.entries()[0].path.len(), 5000);
    repo.edit_index(Limits::default())
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(git(root, &["ls-files", "--stage", "-z"]), records);
}

#[rstest]
#[case::v4(&["update-index", "--index-version=4"], 4)]
#[case::skip_worktree(&["update-index", "--skip-worktree", "file"], 3)]
#[case::intent_to_add(&["add", "-N", "new"], 3)]
fn git_unsupported_flags_and_versions_fail_without_writing(
    #[case] args: &[&str],
    #[case] version: u32,
) {
    let (_root, repo) = repository();
    seed(&repo);
    let root = repo.worktree().unwrap();
    fs::write(root.join("new"), b"new").unwrap();
    git(root, args);
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    assert!(
        matches!(repo.edit_index(Limits::default()), Err(StorageError::Format { source: Error::Version(v), .. }) if v == version)
    );
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[test]
fn split_index_is_refused_without_writing() {
    let (_root, repo) = repository();
    seed(&repo);
    git(repo.worktree().unwrap(), &["update-index", "--split-index"]);
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    // Split replacement entries can have empty names; rejection may precede the link extension.
    assert!(repo.edit_index(Limits::default()).is_err());
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
}

#[test]
fn linked_worktree_uses_its_own_index() {
    let (root, repo) = repository();
    seed(&repo);
    git(
        repo.worktree().unwrap(),
        &["commit", "-m", "original fixture"],
    );
    let linked = root.path().join("linked");
    git(
        repo.worktree().unwrap(),
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let other = Repository::open(&linked).unwrap();
    let common_before = fs::read(repo.git_dir().join("index")).unwrap();
    assert_ne!(other.git_dir(), other.common_dir());
    assert_eq!(
        other
            .read_index(Limits::default())
            .unwrap()
            .unwrap()
            .entries()[0]
            .path,
        b"file"
    );
    let mut edit = other.edit_index(Limits::default()).unwrap();
    edit.replace_entries(Vec::new()).unwrap();
    edit.commit().unwrap();
    assert!(git(&linked, &["ls-files"]).is_empty());
    assert_eq!(
        fs::read(repo.git_dir().join("index")).unwrap(),
        common_before
    );
}

#[test]
fn separate_gitdir_uses_resolved_index_location() {
    let root = tempfile::tempdir().unwrap();
    let worktree = root.path().join("worktree");
    let metadata = root.path().join("metadata");
    git(
        root.path(),
        &[
            "init",
            "--separate-git-dir",
            metadata.to_str().unwrap(),
            worktree.to_str().unwrap(),
        ],
    );
    let repo = Repository::open(&worktree).unwrap();
    repo.edit_index(Limits::default())
        .unwrap()
        .commit()
        .unwrap();
    assert!(metadata.join("index").is_file());
    assert_eq!(
        git(&worktree, &["write-tree"]),
        b"4b825dc642cb6eb9a060e54bf8d69288fbee4904\n"
    );
}

#[test]
fn git_lock_protocol_excludes_both_writer_directions() {
    let (_root, repo) = repository();
    seed(&repo);
    let root = repo.worktree().unwrap();
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    let edit = repo.edit_index(Limits::default()).unwrap();
    assert!(
        !command(root, &["read-tree", "--empty"])
            .output()
            .unwrap()
            .status
            .success()
    );
    drop(edit);
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    fs::write(repo.git_dir().join("index.lock"), b"foreign").unwrap();
    assert!(matches!(
        repo.edit_index(Limits::default()),
        Err(StorageError::Locked(_))
    ));
    assert_eq!(
        fs::read(repo.git_dir().join("index.lock")).unwrap(),
        b"foreign"
    );
}

#[test]
fn rewritten_index_does_not_hide_same_stat_content_changes() {
    let (_root, repo) = repository();
    let root = repo.worktree().unwrap();
    let file_path = root.join("file");
    let original_mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    fs::write(&file_path, b"hello\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&file_path)
        .unwrap()
        .set_modified(original_mtime)
        .unwrap();
    git(root, &["add", "file"]);
    // Make the original index racy at a historical timestamp. An ordinary rewrite timestamp
    // would now be strictly newer, making this test sensitive without sleeping.
    fs::File::options()
        .write(true)
        .open(repo.git_dir().join("index"))
        .unwrap()
        .set_modified(original_mtime)
        .unwrap();
    // Isolate the mtime/size cache behavior using documented Git configuration, then restore
    // the exact original mtime after an equal-size content edit. No sleeps or timing races.
    git(root, &["config", "core.trustctime", "false"]);
    git(root, &["config", "core.checkstat", "minimal"]);
    fs::write(&file_path, b"other\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&file_path)
        .unwrap()
        .set_modified(original_mtime)
        .unwrap();
    repo.edit_index(Limits::default())
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(git(root, &["diff-files", "--name-only"]), b"file\n");
}

#[test]
fn git_empty_index_matches_pure_encoder() {
    let (_root, repo) = repository();
    git(repo.worktree().unwrap(), &["read-tree", "--empty"]);
    let bytes = fs::read(repo.git_dir().join("index")).unwrap();
    let parsed = Index::parse(&bytes, Limits::default()).unwrap();
    assert!(parsed.entries().is_empty());
    assert_eq!(parsed.encode(Limits::default()).unwrap(), bytes);
}

#[test]
fn git_resolve_undo_information_blocks_destructive_edits() {
    let (_root, repo) = repository();
    seed(&repo);
    let root = repo.worktree().unwrap();
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"hello\n");
    let records = format!(
        "0 {}\tfile\n100644 {id} 1\tfile\n100644 {id} 2\tfile\n",
        "0".repeat(40)
    );
    input(root, &["update-index", "--index-info"], records.as_bytes());
    git(root, &["add", "file"]);
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    assert!(
        edit.index()
            .extensions()
            .iter()
            .any(|e| e.signature() == *b"REUC")
    );
    assert_eq!(
        edit.replace_entries(Vec::new()),
        Err(Error::ExtensionPreventsEdit(*b"REUC"))
    );
    edit.commit().unwrap();
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!git(root, &["ls-files", "--resolve-undo"]).is_empty());
}
