use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::atomic::Ordering;

use rstest::rstest;

use super::*;
use crate::InitKind;

fn fixture(path: &[u8], content: &[u8]) -> (tempfile::TempDir, Repository, Vec<index::Entry>) {
    let temp = tempfile::tempdir().unwrap();
    let repo = Repository::init(temp.path().join("repo"), InitKind::Worktree).unwrap();
    let file = repo.worktree().unwrap().join(OsStr::from_bytes(path));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, content).unwrap();
    let entries = vec![index::Entry::new(
        path.to_vec(),
        index::Mode::Regular,
        ObjectId::for_blob(b"old"),
    )];
    (temp, repo, entries)
}
fn observe(
    repo: &Repository,
    entries: &[index::Entry],
    limits: Limits,
) -> Result<Observation, Error> {
    scan(
        repo,
        repo.worktree().unwrap(),
        entries,
        Untracked::Omit,
        limits,
        &AtomicBool::new(false),
    )
}

#[rstest]
#[case::equal(b"old", false)]
#[case::different(b"new", true)]
#[case::empty(b"", true)]
fn compares_bytes_without_stat_shortcuts(#[case] bytes: &[u8], #[case] changed: bool) {
    let (_temp, repo, mut entries) = fixture(b"file", bytes);
    entries[0].assume_valid = true;
    let result = observe(&repo, &entries, Limits::default()).unwrap();
    assert_eq!(!result.changes.is_empty(), changed);
}

#[rstest]
#[case::absolute(b"/etc/passwd")]
#[case::parent(b"../file")]
#[case::dot(b"a/./file")]
#[case::metadata(b"a/.GIT/config")]
#[case::nul(b"a\0b")]
#[case::backslash(b"a\\b")]
#[case::drive(b"C:file")]
#[case::empty_component(b"a//b")]
fn rejects_paths_before_platform_conversion(#[case] path: &[u8]) {
    assert!(matches!(
        validate_path(path),
        Err(Error::Unsupported { .. })
    ));
}

#[test]
fn detects_same_size_content_change_with_restored_mtime() {
    let (_temp, repo, entries) = fixture(b"file", b"old");
    let path = repo.worktree().unwrap().join("file");
    let time = fs::metadata(&path).unwrap().modified().unwrap();
    fs::write(&path, b"new").unwrap();
    File::open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
    assert_eq!(
        observe(&repo, &entries, Limits::default())
            .unwrap()
            .changes
            .len(),
        1
    );
}

#[test]
fn detects_mode_only_change() {
    let (_temp, repo, entries) = fixture(b"file", b"old");
    fs::set_permissions(
        repo.worktree().unwrap().join("file"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let result = observe(&repo, &entries, Limits::default()).unwrap();
    assert_eq!(
        result.changes[0].change,
        Change::Modified(TreeValue {
            id: entries[0].id,
            mode: EntryMode::Executable
        })
    );
}

#[test]
fn reads_symlink_target_without_following_it() {
    let (_temp, repo, mut entries) = fixture(b"link", b"old");
    let path = repo.worktree().unwrap().join("link");
    fs::remove_file(&path).unwrap();
    symlink("/does/not/exist", &path).unwrap();
    entries[0].mode = index::Mode::Symlink;
    entries[0].id = ObjectId::for_blob(b"/does/not/exist");
    assert!(
        observe(&repo, &entries, Limits::default())
            .unwrap()
            .changes
            .is_empty()
    );
}

#[test]
fn never_follows_symlink_ancestor() {
    let (temp, repo, entries) = fixture(b"dir/file", b"old");
    fs::create_dir(temp.path().join("outside")).unwrap();
    fs::write(temp.path().join("outside/file"), b"old").unwrap();
    fs::remove_dir_all(repo.worktree().unwrap().join("dir")).unwrap();
    symlink(
        temp.path().join("outside"),
        repo.worktree().unwrap().join("dir"),
    )
    .unwrap();
    let result = observe(&repo, &entries, Limits::default()).unwrap();
    assert_eq!(
        result.changes[0].change,
        Change::Obstructed {
            path: b"dir".to_vec()
        }
    );
}

#[test]
fn never_follows_substituted_root_ancestor() {
    let (temp, repo, entries) = fixture(b"file", b"old");
    fs::rename(repo.worktree().unwrap(), temp.path().join("moved")).unwrap();
    symlink(temp.path().join("moved"), repo.worktree().unwrap()).unwrap();
    assert!(matches!(
        observe(&repo, &entries, Limits::default()),
        Err(Error::Io { .. })
    ));
}

#[rstest]
#[case::file_limit(Limits { max_file_bytes: 2, ..Limits::default() })]
#[case::total_bytes(Limits { max_worktree_bytes: 2, ..Limits::default() })]
#[case::entries(Limits { max_directory_entries: 0, ..Limits::default() })]
#[case::paths(Limits { max_path_bytes: 0, ..Limits::default() })]
#[case::depth(Limits { max_depth: 0, ..Limits::default() })]
fn enforces_scan_limits(#[case] limits: Limits) {
    let (_temp, repo, entries) = fixture(b"dir/file", b"old");
    assert!(matches!(
        observe(&repo, &entries, limits),
        Err(Error::Limit(_))
    ));
}

#[rstest]
#[case::exact(3)]
#[case::larger(4)]
fn accepts_file_byte_limit(#[case] max: usize) {
    let (_temp, repo, entries) = fixture(b"file", b"old");
    let limits = Limits {
        max_file_bytes: max,
        max_worktree_bytes: max,
        ..Limits::default()
    };
    assert!(observe(&repo, &entries, limits).unwrap().changes.is_empty());
}

#[test]
fn reports_change_observed_after_content_read() {
    let (_temp, repo, entries) = fixture(b"file", b"old");
    let root = repo.worktree().unwrap();
    let result = scan_with_hook(
        &repo,
        root,
        &entries,
        Untracked::Omit,
        Limits::default(),
        &AtomicBool::new(false),
        &mut |_| fs::write(root.join("file"), b"different").unwrap(),
    );
    assert!(matches!(result, Err(Error::Changed(path)) if path == b"file"));
}

#[test]
fn reports_directory_mutation_during_scan() {
    let (_temp, repo, entries) = fixture(b"file", b"old");
    let root = repo.worktree().unwrap();
    // Ensure the injected write changes metadata even within one filesystem clock tick.
    fs::File::open(root)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();
    let result = scan_with_hook(
        &repo,
        root,
        &entries,
        Untracked::Omit,
        Limits::default(),
        &AtomicBool::new(false),
        &mut |_| fs::write(root.join("new-file"), b"new").unwrap(),
    );
    assert!(matches!(result, Err(Error::Changed(path)) if path.is_empty()));
}

#[test]
fn cancellation_during_read_returns_no_partial_report() {
    let (_temp, repo, entries) = fixture(b"file", b"old");
    let cancel = AtomicBool::new(false);
    let result = scan_with_hook(
        &repo,
        repo.worktree().unwrap(),
        &entries,
        Untracked::Omit,
        Limits::default(),
        &cancel,
        &mut |_| cancel.store(true, Ordering::Relaxed),
    );
    assert!(matches!(result, Err(Error::Cancelled)));
}

#[test]
fn skips_nested_repository_and_bare_metadata() {
    let (_temp, repo, entries) = fixture(b"file", b"old");
    let root = repo.worktree().unwrap();
    Repository::init(root.join("nested"), InitKind::Worktree).unwrap();
    Repository::init(root.join("bare"), InitKind::Bare).unwrap();
    let result = scan(
        &repo,
        root,
        &entries,
        Untracked::RawFilesWithoutIgnores,
        Limits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(result.boundaries, [b"bare".to_vec(), b"nested".to_vec()]);
    assert!(result.untracked.is_empty());
}

#[test]
fn tracks_exact_case_without_alias_lookup() {
    let (_temp, repo, mut entries) = fixture(b"file", b"old");
    entries[0].path = b"FILE".to_vec();
    let result = observe(&repo, &entries, Limits::default()).unwrap();
    assert_eq!(result.changes[0].change, Change::Deleted);
}

#[cfg(target_os = "macos")]
#[test]
fn rejects_non_ascii_paths_on_normalizing_platform() {
    assert!(matches!(
        validate_path(b"caf\xc3\xa9"),
        Err(Error::Unsupported { .. })
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn preserves_non_utf8_names_on_linux() {
    let (_temp, repo, entries) = fixture(b"file-\xff", b"new");
    let result = observe(&repo, &entries, Limits::default()).unwrap();
    assert_eq!(result.changes[0].path, b"file-\xff");
}

#[test]
fn special_file_is_an_obstruction_without_blocking_read() {
    let (_temp, repo, entries) = fixture(b"file", b"old");
    let path = repo.worktree().unwrap().join("file");
    fs::remove_file(&path).unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let result = observe(&repo, &entries, Limits::default()).unwrap();
    assert_eq!(
        result.changes[0].change,
        Change::Obstructed {
            path: b"file".to_vec()
        }
    );
}

#[test]
fn replaced_parent_is_detected_without_following_new_symlink() {
    let (temp, repo, entries) = fixture(b"dir/file", b"old");
    let root = repo.worktree().unwrap();
    let result = scan_with_hook(
        &repo,
        root,
        &entries,
        Untracked::Omit,
        Limits::default(),
        &AtomicBool::new(false),
        &mut |_| {
            fs::rename(root.join("dir"), temp.path().join("moved")).unwrap();
            symlink(repo.git_dir(), root.join("dir")).unwrap();
        },
    );
    assert!(matches!(result, Err(Error::Changed(_))));
}

#[test]
fn tracked_path_below_nested_repository_is_obstructed() {
    let (_temp, repo, entries) = fixture(b"dir/file", b"old");
    fs::write(
        repo.worktree().unwrap().join("dir/.git"),
        b"gitdir: /outside",
    )
    .unwrap();
    let result = observe(&repo, &entries, Limits::default()).unwrap();
    assert_eq!(
        result.changes[0].change,
        Change::Obstructed {
            path: b"dir".to_vec()
        }
    );
    assert_eq!(result.boundaries, [b"dir".to_vec()]);
}

#[rstest]
#[case::exact(3, true)]
#[case::too_small(2, false)]
fn symlink_payload_is_bounded(#[case] max: usize, #[case] succeeds: bool) {
    let (_temp, repo, mut entries) = fixture(b"link", b"old");
    let path = repo.worktree().unwrap().join("link");
    fs::remove_file(&path).unwrap();
    symlink("old", &path).unwrap();
    entries[0].mode = index::Mode::Symlink;
    let result = observe(
        &repo,
        &entries,
        Limits {
            max_file_bytes: max,
            ..Limits::default()
        },
    );
    assert_eq!(result.is_ok(), succeeds);
}

#[test]
fn tracked_only_obstruction_does_not_descend_into_directory() {
    let (_temp, repo, entries) = fixture(b"file", b"old");
    let path = repo.worktree().unwrap().join("file");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let result = observe(
        &repo,
        &entries,
        Limits {
            max_depth: 0,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        result.changes[0].change,
        Change::Obstructed {
            path: b"file".to_vec()
        }
    );
}
