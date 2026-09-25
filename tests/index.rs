//! Original fixtures generated through Git's public CLI; no upstream implementation/test source.
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use girt::index::{Entry, Index, Limits, Mode, Stage, StorageError};
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
fn repository(format: girt::ObjectFormat) -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Worktree).unwrap();
    (root, repo)
}
fn seed(repo: &Repository) {
    let root = repo.worktree().unwrap();
    fs::write(root.join("file"), b"hello\n").unwrap();
    git(root, &["add", "file"]);
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn reads_git_stat_flags_and_roundtrips_exactly(#[case] format: girt::ObjectFormat) {
    let (_root, repo) = repository(format);
    seed(&repo);
    let root = repo.worktree().unwrap();
    git(root, &["update-index", "--assume-unchanged", "file"]);
    let original = fs::read(repo.git_dir().join("index")).unwrap();
    let index = repo.read_index(Limits::default()).unwrap().unwrap();
    let entry = &index.entries()[0];
    assert_eq!(entry.path, b"file");
    assert_eq!(entry.id, ObjectId::for_blob(format, b"hello\n"));
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
#[case::regular_sha1(girt::ObjectFormat::Sha1, Mode::Regular, "100644")]
#[case::regular_sha256(girt::ObjectFormat::Sha256, Mode::Regular, "100644")]
#[case::executable_sha1(girt::ObjectFormat::Sha1, Mode::Executable, "100755")]
#[case::executable_sha256(girt::ObjectFormat::Sha256, Mode::Executable, "100755")]
#[case::symlink_sha1(girt::ObjectFormat::Sha1, Mode::Symlink, "120000")]
#[case::symlink_sha256(girt::ObjectFormat::Sha256, Mode::Symlink, "120000")]
#[case::gitlink_sha1(girt::ObjectFormat::Sha1, Mode::Gitlink, "160000")]
#[case::gitlink_sha256(girt::ObjectFormat::Sha256, Mode::Gitlink, "160000")]
fn git_reads_written_modes_and_writes_expected_tree(
    #[case] format: girt::ObjectFormat,
    #[case] mode: Mode,
    #[case] spelling: &str,
) {
    let (_root, repo) = repository(format);
    let root = repo.worktree().unwrap();
    let id = repo.loose_objects().write_blob(b"payload").unwrap();
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
    let tree = Tree::parse(format, &payload).unwrap();
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
        format!("{}\n", Tree::new(format, vec![]).unwrap().id()).as_bytes()
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn git_conflict_stages_and_byte_paths_roundtrip(#[case] format: girt::ObjectFormat) {
    let (_root, repo) = repository(format);
    let root = repo.worktree().unwrap();
    let id = repo.loose_objects().write_blob(b"data").unwrap();
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

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn git_long_paths_need_no_filesystem_materialization(#[case] format: girt::ObjectFormat) {
    let (_root, repo) = repository(format);
    let root = repo.worktree().unwrap();
    let id = repo.loose_objects().write_blob(b"data").unwrap();
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
#[case::v4_sha1(girt::ObjectFormat::Sha1, &["update-index", "--index-version=4"], 4)]
#[case::v4_sha256(girt::ObjectFormat::Sha256, &["update-index", "--index-version=4"], 4)]
#[case::skip_worktree_sha1(girt::ObjectFormat::Sha1, &["update-index", "--skip-worktree", "file"], 3)]
#[case::skip_worktree_sha256(girt::ObjectFormat::Sha256, &["update-index", "--skip-worktree", "file"], 3)]
#[case::intent_to_add_sha1(girt::ObjectFormat::Sha1, &["add", "-N", "new"], 3)]
#[case::intent_to_add_sha256(girt::ObjectFormat::Sha256, &["add", "-N", "new"], 3)]
fn git_flags_and_versions_roundtrip_without_changes(
    #[case] format: girt::ObjectFormat,

    #[case] args: &[&str],
    #[case] version: u32,
) {
    let (_root, repo) = repository(format);
    seed(&repo);
    let root = repo.worktree().unwrap();
    fs::write(root.join("new"), b"new").unwrap();
    git(root, args);
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    let edit = repo.edit_index(Limits::default()).unwrap();
    assert_eq!(edit.index().version() as u32, version);
    assert_eq!(edit.index().encode(Limits::default()).unwrap(), before);
    let observed = git(root, &["ls-files", "--stage", "--debug"]);
    edit.commit().unwrap();
    assert_eq!(git(root, &["ls-files", "--stage", "--debug"]), observed);
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn split_index_is_resolved_and_preserved(#[case] format: girt::ObjectFormat) {
    let (_root, repo) = repository(format);
    seed(&repo);
    git(repo.worktree().unwrap(), &["update-index", "--split-index"]);
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    let edit = repo.edit_index(Limits::default()).unwrap();
    assert_eq!(edit.index().entries()[0].path, b"file");
    assert!(edit.index().shared_index_id().is_some());
    edit.commit().unwrap();
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn linked_worktree_uses_its_own_index(#[case] format: girt::ObjectFormat) {
    let (root, repo) = repository(format);
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

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn separate_gitdir_uses_resolved_index_location(#[case] format: girt::ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let worktree = root.path().join("worktree");
    let metadata = root.path().join("metadata");
    git(
        root.path(),
        &[
            "init",
            &format!("--object-format={format}"),
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
        format!("{}\n", Tree::new(format, vec![]).unwrap().id()).as_bytes()
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn git_lock_protocol_excludes_both_writer_directions(#[case] format: girt::ObjectFormat) {
    let (_root, repo) = repository(format);
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

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn rewritten_index_does_not_hide_same_stat_content_changes(#[case] format: girt::ObjectFormat) {
    let (_root, repo) = repository(format);
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

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn git_empty_index_matches_pure_encoder(#[case] format: girt::ObjectFormat) {
    let (_root, repo) = repository(format);
    git(repo.worktree().unwrap(), &["read-tree", "--empty"]);
    let bytes = fs::read(repo.git_dir().join("index")).unwrap();
    let parsed = Index::parse(format, &bytes, Limits::default()).unwrap();
    assert!(parsed.entries().is_empty());
    assert_eq!(parsed.encode(Limits::default()).unwrap(), bytes);
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn git_resolve_undo_information_survives_entry_edits(#[case] format: girt::ObjectFormat) {
    let (_root, repo) = repository(format);
    seed(&repo);
    let root = repo.worktree().unwrap();
    let id = ObjectId::for_blob(format, b"hello\n");
    let records = format!(
        "0 {}\tfile\n100644 {id} 1\tfile\n100644 {id} 2\tfile\n",
        ObjectId::null(format)
    );
    input(root, &["update-index", "--index-info"], records.as_bytes());
    git(root, &["add", "file"]);
    let before = git(root, &["ls-files", "--resolve-undo"]);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    assert!(
        edit.index()
            .extensions()
            .iter()
            .any(|e| e.signature() == *b"REUC")
    );
    edit.replace_entries(Vec::new()).unwrap();
    edit.commit().unwrap();
    assert_eq!(git(root, &["ls-files", "--resolve-undo"]), before);
    assert!(!git(root, &["ls-files", "--resolve-undo"]).is_empty());
}

#[rstest]
#[case::v2_sha1(
    girt::ObjectFormat::Sha1,
    girt::index::Version::V2,
    "--index-version=2"
)]
#[case::v2_sha256(
    girt::ObjectFormat::Sha256,
    girt::index::Version::V2,
    "--index-version=2"
)]
#[case::v3_sha1(
    girt::ObjectFormat::Sha1,
    girt::index::Version::V3,
    "--index-version=3"
)]
#[case::v3_sha256(
    girt::ObjectFormat::Sha256,
    girt::index::Version::V3,
    "--index-version=3"
)]
#[case::v4_sha1(
    girt::ObjectFormat::Sha1,
    girt::index::Version::V4,
    "--index-version=4"
)]
#[case::v4_sha256(
    girt::ObjectFormat::Sha256,
    girt::index::Version::V4,
    "--index-version=4"
)]
fn versions_preserve_git_conflicts_and_long_byte_paths(
    #[case] format: girt::ObjectFormat,
    #[case] version: girt::index::Version,
    #[case] option: &str,
) {
    let (_root, repo) = repository(format);
    seed(&repo);
    let root = repo.worktree().unwrap();
    let id = ObjectId::for_blob(format, b"hello\n");
    let mut records = format!(
        "100755 {id} 1\tconflict\0\
        100644 {id} 2\tconflict\0\
        120000 {id} 3\tconflict\0\
        100644 {id} 0\t"
    )
    .into_bytes();
    records.extend_from_slice(&vec![b'x'; 5000]);
    records.extend_from_slice(
        format!(
            "\0\
        100644 {id} 0\t"
        )
        .as_bytes(),
    );
    records.extend_from_slice(b"\xff\0");
    input(root, &["update-index", "-z", "--index-info"], &records);
    git(root, &["update-index", option]);
    let before = git(root, &["ls-files", "--stage", "-z"]);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    // Git may choose v2 for a requested v3 without extended flags. Explicit girt conversion
    // tests all three output versions regardless of that optimization.
    edit.set_version(version).unwrap();
    let mut entries = edit.index().entries().to_vec();
    entries
        .iter_mut()
        .find(|e| e.path == b"file")
        .unwrap()
        .assume_valid = true;
    edit.replace_entries(entries).unwrap();
    edit.commit().unwrap();
    assert_eq!(git(root, &["ls-files", "--stage", "-z"]), before);
    assert_eq!(
        repo.read_index(Limits::default())
            .unwrap()
            .unwrap()
            .version(),
        version
    );
}

#[rstest]
#[case::v3_sha1(girt::ObjectFormat::Sha1, girt::index::Version::V3)]
#[case::v3_sha256(girt::ObjectFormat::Sha256, girt::index::Version::V3)]
#[case::v4_sha1(girt::ObjectFormat::Sha1, girt::index::Version::V4)]
#[case::v4_sha256(girt::ObjectFormat::Sha256, girt::index::Version::V4)]
fn git_observes_girt_extended_flag_edits(
    #[case] format: girt::ObjectFormat,
    #[case] version: girt::index::Version,
) {
    let (_root, repo) = repository(format);
    seed(&repo);
    let root = repo.worktree().unwrap();
    fs::write(root.join("new"), b"new").unwrap();
    git(root, &["add", "-N", "new"]);
    git(root, &["update-index", "--skip-worktree", "file"]);
    let expected = git(root, &["ls-files", "--debug"]);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    assert!(edit.index().entries()[0].skip_worktree);
    assert!(edit.index().entries()[1].intent_to_add);
    let original = edit.index().entries().to_vec();
    let mut cleared = original.clone();
    cleared[0].skip_worktree = false;
    cleared[1].intent_to_add = false;
    edit.replace_entries(cleared).unwrap();
    edit.replace_entries(original).unwrap();
    edit.set_version(version).unwrap();
    edit.commit().unwrap();
    assert_eq!(git(root, &["ls-files", "--debug"]), expected);
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn sparse_index_is_preserved_then_expanded_without_materializing(
    #[case] format: girt::ObjectFormat,
) {
    let (_root, repo) = repository(format);
    let root = repo.worktree().unwrap();
    fs::create_dir(root.join("inside")).unwrap();
    fs::create_dir(root.join("outside")).unwrap();
    fs::write(root.join("inside/a"), b"a").unwrap();
    fs::write(root.join("outside/b"), b"b").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "original sparse fixture"]);
    git(
        root,
        &[
            "sparse-checkout",
            "set",
            "--cone",
            "--sparse-index",
            "inside",
        ],
    );
    let observed = git(root, &["ls-files", "--sparse", "--stage"]);
    assert!(String::from_utf8_lossy(&observed).contains("040000"));
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    let edit = repo.edit_index(Limits::default()).unwrap();
    assert_eq!(edit.index().entries()[1].mode, Mode::SparseDirectory);
    edit.commit().unwrap();
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    let objects = repo.objects(girt::PackLimits::default()).unwrap();
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    edit.expand_sparse(
        &objects,
        girt::index::SparseLimits::default(),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    edit.commit().unwrap();
    assert!(!root.join("outside").exists());
    let observed = git(root, &["ls-files", "--stage"]);
    assert!(String::from_utf8_lossy(&observed).contains("outside/b"));
    let expanded = repo.read_index(Limits::default()).unwrap().unwrap();
    assert!(
        expanded
            .entries()
            .iter()
            .any(|e| e.path == b"outside/b" && e.skip_worktree)
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[rstest]
#[case::intent_sha1(girt::ObjectFormat::Sha1, true, false)]
#[case::intent_sha256(girt::ObjectFormat::Sha256, true, false)]
#[case::skip_sha1(girt::ObjectFormat::Sha1, false, true)]
#[case::skip_sha256(girt::ObjectFormat::Sha256, false, true)]
fn raw_workflows_refuse_extended_flags_without_mutation(
    #[case] format: girt::ObjectFormat,
    #[case] intent: bool,
    #[case] skip: bool,
) {
    let (_root, repo) = repository(format);
    seed(&repo);
    let root = repo.worktree().unwrap();
    let tree = String::from_utf8(git(root, &["write-tree"])).unwrap();
    let tree = ObjectId::from_hex(format, tree.trim()).unwrap();
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    let mut entries = edit.index().entries().to_vec();
    entries[0].intent_to_add = intent;
    entries[0].skip_worktree = skip;
    edit.replace_entries(entries).unwrap();
    edit.commit().unwrap();
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    assert!(matches!(
        repo.raw_status(
            girt::status::Baseline::Tree(Some(tree)),
            girt::status::Untracked::Omit,
            girt::status::Limits::default(),
            &cancel
        ),
        Err(girt::status::Error::Unsupported { .. })
    ));
    let failure = repo
        .checkout_tree(Some(tree), None, girt::checkout::Limits::default(), &cancel)
        .unwrap_err();
    assert!(matches!(
        *failure.cause,
        girt::checkout::Error::Refused { .. }
    ));
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert_eq!(fs::read(root.join("file")).unwrap(), b"hello\n");
    assert!(!repo.git_dir().join("index.lock").exists());
}

fn split_fixture(format: girt::ObjectFormat) -> (tempfile::TempDir, Repository) {
    let (root, repo) = repository(format);
    let work = repo.worktree().unwrap();
    fs::write(work.join("a"), b"old a").unwrap();
    fs::write(work.join("b"), b"old b").unwrap();
    fs::write(work.join("c"), b"old c").unwrap();
    git(work, &["add", "."]);
    git(work, &["update-index", "--split-index"]);
    git(work, &["config", "splitIndex.maxPercentChange", "100"]);
    fs::write(work.join("a"), b"new a").unwrap();
    fs::write(work.join("d"), b"new d").unwrap();
    git(work, &["add", "a", "d"]);
    git(work, &["update-index", "--force-remove", "b"]);
    git(work, &["update-index", "--assume-unchanged", "c"]);
    (root, repo)
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn split_deletion_replacement_addition_and_full_publication(#[case] format: girt::ObjectFormat) {
    let (_root, repo) = split_fixture(format);
    let work = repo.worktree().unwrap();
    let expected = git(work, &["ls-files", "--stage"]);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    let index = edit.index();
    assert_eq!(
        index
            .entries()
            .iter()
            .map(|e| e.path.as_slice())
            .collect::<Vec<_>>(),
        [b"a", b"c", b"d"]
    );
    assert_eq!(index.entries()[0].id, ObjectId::for_blob(format, b"new a"));
    assert!(index.entries()[1].assume_valid);
    let shared = repo
        .git_dir()
        .join(format!("sharedindex.{}", index.shared_index_id().unwrap()));
    let original_shared = fs::read(&shared).unwrap();
    edit.set_version(girt::index::Version::V4).unwrap();
    assert!(edit.index().shared_index_id().is_none());
    edit.commit().unwrap();
    assert_eq!(git(work, &["ls-files", "--stage"]), expected);
    assert_eq!(fs::read(shared).unwrap(), original_shared);
}

#[rstest]
#[case::missing(girt::ObjectFormat::Sha1, false)]
#[case::corrupt(girt::ObjectFormat::Sha1, true)]
#[case::missing_sha256(girt::ObjectFormat::Sha256, false)]
#[case::corrupt_sha256(girt::ObjectFormat::Sha256, true)]
fn split_dependency_failure_releases_lock(
    #[case] format: girt::ObjectFormat,
    #[case] corrupt: bool,
) {
    let (_root, repo) = split_fixture(format);
    let index = repo.read_index(Limits::default()).unwrap().unwrap();
    let shared = repo
        .git_dir()
        .join(format!("sharedindex.{}", index.shared_index_id().unwrap()));
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    damage_shared(&shared, corrupt);
    assert!(repo.edit_index(Limits::default()).is_err());
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!repo.git_dir().join("index.lock").exists());
}
fn damage_shared(path: &Path, corrupt: bool) {
    if corrupt {
        fs::write(path, b"broken").unwrap();
    } else {
        fs::remove_file(path).unwrap();
    }
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn split_publication_detects_changed_dependency(#[case] format: girt::ObjectFormat) {
    let (_root, repo) = split_fixture(format);
    let edit = repo.edit_index(Limits::default()).unwrap();
    let shared = repo.git_dir().join(format!(
        "sharedindex.{}",
        edit.index().shared_index_id().unwrap()
    ));
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    fs::remove_file(&shared).unwrap();
    assert!(matches!(edit.commit(), Err(StorageError::Changed(path)) if path == shared));
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn split_colocation_preserves_head_preconditions_and_git_lock(#[case] format: girt::ObjectFormat) {
    use girt::refs::{Expected, RefName, Reflog};
    let (_root, repo) = split_fixture(format);
    let refs = repo.references().unwrap();
    let head = refs.read(&RefName::new(b"HEAD").unwrap()).unwrap().unwrap();
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    let mut edit = repo.edit_colocation(Limits::default()).unwrap();
    assert!(
        !command(
            repo.worktree().unwrap(),
            &["update-index", "--no-split-index"]
        )
        .output()
        .unwrap()
        .status
        .success()
    );
    edit.index_mut().replace_entries(vec![]).unwrap();
    assert!(
        edit.commit(head.clone(), Expected::Absent, Reflog::Preserve)
            .is_err()
    );
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
    let edit = repo.edit_colocation(Limits::default()).unwrap();
    edit.commit(head.clone(), Expected::Value(head), Reflog::Preserve)
        .unwrap();
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn split_aggregate_byte_limit_includes_shared_file(#[case] format: girt::ObjectFormat) {
    let (_root, repo) = split_fixture(format);
    let index = repo.read_index(Limits::default()).unwrap().unwrap();
    let main = fs::read(repo.git_dir().join("index")).unwrap();
    let shared = fs::read(
        repo.git_dir()
            .join(format!("sharedindex.{}", index.shared_index_id().unwrap())),
    )
    .unwrap();
    let short = Limits {
        max_bytes: main.len() + shared.len() - 1,
        ..Limits::default()
    };
    assert!(repo.edit_index(short).is_err());
    let exact = Limits {
        max_bytes: main.len() + shared.len(),
        ..Limits::default()
    };
    assert!(repo.read_index(exact).unwrap().is_some());
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[rstest]
#[case::v2_sha1(girt::ObjectFormat::Sha1, "2")]
#[case::v3_sha1(girt::ObjectFormat::Sha1, "3")]
#[case::v4_sha1(girt::ObjectFormat::Sha1, "4")]
#[case::v2_sha256(girt::ObjectFormat::Sha256, "2")]
#[case::v3_sha256(girt::ObjectFormat::Sha256, "3")]
#[case::v4_sha256(girt::ObjectFormat::Sha256, "4")]
fn split_versions_flags_and_entry_edit(#[case] format: girt::ObjectFormat, #[case] version: &str) {
    let (_root, repo) = split_fixture(format);
    let root = repo.worktree().unwrap();
    git(
        root,
        &["update-index", &format!("--index-version={version}")],
    );
    git(root, &["update-index", "--skip-worktree", "c"]);
    let before = git(root, &["ls-files", "--stage"]);
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    let mut entries = edit.index().entries().to_vec();
    assert!(entries[1].skip_worktree);
    entries[1].assume_valid = false;
    edit.replace_entries(entries).unwrap();
    assert!(edit.index().shared_index_id().is_none());
    edit.commit().unwrap();
    assert_eq!(git(root, &["ls-files", "--stage"]), before);
    assert!(
        repo.read_index(Limits::default())
            .unwrap()
            .unwrap()
            .entries()[1]
            .skip_worktree
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn sparse_colocation_preserves_directories_after_other_edits(#[case] format: girt::ObjectFormat) {
    use girt::refs::{Expected, RefName, Reflog};
    let (_root, repo) = repository(format);
    let leaf = repo.loose_objects().write_blob(b"x").unwrap();
    let tree = Tree::new(
        format,
        vec![girt::TreeEntry {
            name: b"x".to_vec(),
            mode: girt::EntryMode::Blob,
            id: leaf,
        }],
    )
    .unwrap();
    let id = repo.loose_objects().write_tree(&tree).unwrap();
    let mut directory = Entry::new(b"outside/".to_vec(), Mode::SparseDirectory, id);
    directory.skip_worktree = true;
    let refs = repo.references().unwrap();
    let head = refs.read(&RefName::new(b"HEAD").unwrap()).unwrap().unwrap();
    let mut edit = repo.edit_colocation(Limits::default()).unwrap();
    edit.index_mut()
        .replace_entries(vec![directory.clone()])
        .unwrap();
    edit.commit(
        head.clone(),
        Expected::Value(head.clone()),
        Reflog::Preserve,
    )
    .unwrap();
    let mut edit = repo.edit_colocation(Limits::default()).unwrap();
    edit.index_mut()
        .replace_entries(vec![
            directory,
            Entry::new(b"file".to_vec(), Mode::Regular, leaf),
        ])
        .unwrap();
    edit.index_mut()
        .set_version(girt::index::Version::V4)
        .unwrap();
    edit.commit(head.clone(), Expected::Value(head), Reflog::Preserve)
        .unwrap();
    let observed = git(repo.worktree().unwrap(), &["ls-files", "--stage"]);
    assert!(String::from_utf8_lossy(&observed).contains("outside/x"));
    assert!(!repo.worktree().unwrap().join("outside").exists());
}
