//! Public layout and shallow contracts using independently generated Git fixtures in both formats.
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use girt::{
    Commit, HistoryError, HistoryLimits, ObjectFormat, ObjectId, OpenError, PackLimits, ReadLimits,
    Repository, ShallowError, WorktreeError, WorktreeState,
};
use rstest::rstest;
#[path = "support/layout_git.rs"]
mod layout_git;
use layout_git::git;

fn canonical(path: impl AsRef<Path>) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}
fn init(path: &Path, format: ObjectFormat) {
    std::fs::create_dir_all(path).unwrap();
    git(
        path,
        &[
            "init",
            "--template=",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
}
fn id(bytes: Vec<u8>) -> ObjectId {
    std::str::from_utf8(&bytes).unwrap().trim().parse().unwrap()
}
fn commit(path: &Path, message: &str) -> ObjectId {
    git(
        path,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            message,
        ],
        b"",
    );
    id(git(path, &["rev-parse", "HEAD"], b""))
}
fn linked(path: &Path, format: ObjectFormat) -> Repository {
    init(path, format);
    commit(path, "base");
    git(path, &["worktree", "add", "--detach", "linked"], b"");
    Repository::open(path).unwrap()
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn linked_inventory_preserves_missing_identity_and_private_state(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = linked(root.path(), format);
    let child = repo.open_worktree(root.path().join("linked")).unwrap();
    let private = child.git_dir().to_path_buf();
    assert_ne!(child.git_dir(), repo.git_dir());
    assert_eq!(child.common_dir(), repo.common_dir());
    assert_eq!(child.object_dir(), repo.object_dir());
    assert!(matches!(
        repo.worktrees(1, &AtomicBool::new(false)).unwrap()[0].state,
        WorktreeState::Available
    ));
    std::fs::remove_dir_all(root.path().join("linked")).unwrap();
    let entries = repo.worktrees(1, &AtomicBool::new(false)).unwrap();
    assert_eq!(entries[0].git_dir, private);
    assert!(matches!(entries[0].state, WorktreeState::Missing));
    let missing = Repository::open(&entries[0].git_dir).unwrap();
    assert_eq!(missing.common_dir(), repo.common_dir());
    assert!(missing.git_dir().join("HEAD").is_file());
    assert!(matches!(
        repo.worktrees(0, &AtomicBool::new(false)),
        Err(WorktreeError::Limit)
    ));
    assert!(matches!(
        repo.worktrees(1, &AtomicBool::new(true)),
        Err(WorktreeError::Cancelled)
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn relative_layout_survives_whole_repository_move(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original");
    let repo = linked(&original, format);
    std::fs::write(
        original.join("linked/.git"),
        b"gitdir: ../.git/worktrees/linked\n",
    )
    .unwrap();
    std::fs::write(
        repo.git_dir().join("worktrees/linked/gitdir"),
        b"../../../linked/.git\n",
    )
    .unwrap();
    let moved = root.path().join("moved");
    std::fs::rename(&original, &moved).unwrap();
    let child = Repository::discover(moved.join("linked")).unwrap();
    assert_eq!(
        child.worktree(),
        Some(canonical(moved.join("linked")).as_path())
    );
    assert_eq!(child.common_dir(), canonical(moved.join(".git")));
    assert!(matches!(
        child.worktrees(10, &AtomicBool::new(false)).unwrap()[0].state,
        WorktreeState::Available
    ));
    let observed = git(
        &moved.join("linked"),
        &["rev-parse", "--show-toplevel"],
        b"",
    );
    assert_eq!(
        canonical(std::str::from_utf8(&observed).unwrap().trim()),
        child.worktree().unwrap()
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn moved_checkout_is_visible_then_git_repair_restores_links(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = linked(root.path(), format);
    std::fs::rename(root.path().join("linked"), root.path().join("moved")).unwrap();
    assert!(matches!(
        repo.worktrees(10, &AtomicBool::new(false)).unwrap()[0].state,
        WorktreeState::Missing
    ));
    assert!(Repository::open(root.path().join("moved")).is_err());
    git(root.path(), &["worktree", "repair", "moved"], b"");
    assert_eq!(
        repo.open_worktree(root.path().join("moved"))
            .unwrap()
            .common_dir(),
        repo.common_dir()
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn shared_settings_and_direct_private_bootstrap_match_git(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = linked(root.path(), format);
    git(root.path(), &["config", "core.worktree", ".."], b"");
    let child = Repository::open(root.path().join("linked")).unwrap();
    assert_eq!(
        child.worktree(),
        Some(canonical(root.path().join("linked")).as_path())
    );
    git(
        root.path(),
        &["config", "extensions.worktreeConfig", "true"],
        b"",
    );
    std::fs::write(
        child.git_dir().join("config.worktree"),
        b"[include]\npath=private.inc\n",
    )
    .unwrap();
    std::fs::write(
        child.git_dir().join("private.inc"),
        b"[core]\nworktree=../../..\n",
    )
    .unwrap();
    let child = Repository::open(root.path().join("linked")).unwrap();
    let observed = git(
        &root.path().join("linked"),
        &["rev-parse", "--show-toplevel"],
        b"",
    );
    assert_eq!(
        child.worktree(),
        Some(canonical(std::str::from_utf8(&observed).unwrap().trim()).as_path())
    );
    assert_eq!(
        child.worktree(),
        Some(canonical(repo.git_dir().join("worktrees")).as_path())
    );
    std::fs::write(
        child.git_dir().join("config.worktree"),
        b"[core]\nworktree=../../..\n",
    )
    .unwrap();
    let child = Repository::open(root.path().join("linked")).unwrap();
    assert_eq!(child.worktree(), repo.worktree());
    assert_eq!(
        Repository::open(root.path()).unwrap().git_dir(),
        repo.git_dir()
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn unrelated_checkout_is_structured_error(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    init(&root.path().join("one"), format);
    init(&root.path().join("two"), format);
    let repo = Repository::open(root.path().join("one")).unwrap();
    assert!(matches!(
        repo.open_worktree(root.path().join("two")),
        Err(OpenError::Unrelated(_))
    ));
}

#[cfg(unix)]
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn dotgit_directory_symlink_retains_worktree_and_alias_identity(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), format);
    std::fs::rename(root.path().join(".git"), root.path().join("metadata")).unwrap();
    std::os::unix::fs::symlink("metadata", root.path().join(".git")).unwrap();
    let repo = Repository::open(root.path().join(".git")).unwrap();
    assert_eq!(repo.worktree(), Some(canonical(root.path()).as_path()));
    assert_eq!(repo.git_dir(), canonical(root.path().join("metadata")));
    assert_eq!(
        Repository::discover(root.path()).unwrap().git_dir(),
        repo.git_dir()
    );
    git(root.path(), &["rev-parse", "--show-toplevel"], b"");
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn depth_clone_freezes_boundaries_and_refreshes_after_git_deepening(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    init(&source, format);
    let first = commit(&source, "one");
    let second = commit(&source, "two");
    let tip = commit(&source, "three");
    git(
        root.path(),
        &["clone", "--no-local", "--depth=1", "source", "clone"],
        b"",
    );
    let clone = root.path().join("clone");
    let mut repo = Repository::open(&clone).unwrap();
    let old = repo.objects(PackLimits::default()).unwrap();
    assert_eq!(repo.shallow_roots().iter().collect::<Vec<_>>(), vec![tip]);
    assert_eq!(
        old.walk(&[tip], HistoryLimits::default()).unwrap(),
        vec![tip]
    );
    let raw = old.read(tip, ReadLimits::default()).unwrap().unwrap();
    assert_eq!(
        Commit::parse(format, raw.data()).unwrap().parents(),
        &[second]
    );
    assert!(
        matches!(old.walk(&[second], HistoryLimits::default()), Err(HistoryError::Missing(id)) if id == second)
    );
    git(&clone, &["fetch", "--deepen=1"], b"");
    repo.refresh_shallow(1024, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        repo.shallow_roots().iter().collect::<Vec<_>>(),
        vec![second]
    );
    assert_eq!(
        old.walk(&[tip], HistoryLimits::default()).unwrap(),
        vec![tip]
    );
    let refreshed = repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        refreshed.walk(&[tip], HistoryLimits::default()).unwrap(),
        vec![tip, second]
    );
    assert_eq!(git(&clone, &["rev-list", "--count", "HEAD"], b""), b"2\n");
    git(&clone, &["fetch", "--unshallow"], b"");
    let reopened = Repository::open(&clone).unwrap();
    assert!(reopened.shallow_roots().is_empty());
    assert_eq!(
        reopened
            .objects(PackLimits::default())
            .unwrap()
            .walk(&[tip], HistoryLimits::default())
            .unwrap(),
        vec![tip, second, first]
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn missing_nondeclared_parent_is_error_and_failed_refresh_preserves_snapshot(
    #[case] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), format);
    let parent = commit(root.path(), "parent");
    let tip = commit(root.path(), "tip");
    let mut repo = Repository::open(root.path()).unwrap();
    let hex = parent.to_string();
    std::fs::remove_file(repo.object_dir().join(&hex[..2]).join(&hex[2..])).unwrap();
    assert!(
        matches!(repo.objects(PackLimits::default()).unwrap().walk(&[tip], HistoryLimits::default()), Err(HistoryError::Missing(id)) if id == parent)
    );
    std::fs::write(repo.common_dir().join("shallow"), format!("{tip}\n{tip}\n")).unwrap();
    repo.refresh_shallow(1024, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        repo.objects(PackLimits::default())
            .unwrap()
            .walk(&[tip], HistoryLimits::default())
            .unwrap(),
        vec![tip]
    );
    std::fs::write(repo.common_dir().join("shallow"), b"bad\n").unwrap();
    assert!(matches!(
        repo.refresh_shallow(1024, &AtomicBool::new(false)),
        Err(ShallowError::InvalidRoot { line: 1 })
    ));
    assert!(repo.shallow_roots().contains(tip));
    assert!(matches!(
        Repository::open(root.path()),
        Err(OpenError::Shallow(_))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn declarations_do_not_hide_missing_or_wrong_kind_boundary_objects(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), format);
    let repo = Repository::open(root.path()).unwrap();
    let blob = repo.loose_objects().write_blob(b"not a commit").unwrap();
    let missing = ObjectId::for_blob(format, b"absent");
    std::fs::write(
        repo.common_dir().join("shallow"),
        format!("{blob}\n{missing}\n"),
    )
    .unwrap();
    let repo = Repository::open(root.path()).unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    assert!(
        matches!(objects.walk(&[blob], HistoryLimits::default()), Err(HistoryError::NotCommit(id)) if id == blob)
    );
    assert!(
        matches!(objects.walk(&[missing], HistoryLimits::default()), Err(HistoryError::Missing(id)) if id == missing)
    );
    assert!(matches!(
        girt::fetch::KnownHistory::new(
            &objects,
            &[],
            girt::fetch::FetchLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(girt::fetch::FetchError::Unsupported(_))
    ));
}

#[cfg(unix)]
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn inaccessible_checkout_retains_openable_metadata(#[case] format: ObjectFormat) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let repo = linked(root.path(), format);
    let path = root.path().join("linked");
    let permissions = std::fs::metadata(&path).unwrap().permissions();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).unwrap();
    let entries = repo.worktrees(10, &AtomicBool::new(false)).unwrap();
    let metadata = Repository::open(&entries[0].git_dir);
    // Restore before assertions so an assertion failure never leaves an inaccessible fixture.
    std::fs::set_permissions(&path, permissions).unwrap();
    assert!(
        matches!(&entries[0].state, WorktreeState::Inaccessible(OpenError::Io { source, .. }) if source.kind() == std::io::ErrorKind::PermissionDenied)
    );
    assert_eq!(metadata.unwrap().common_dir(), repo.common_dir());
}

#[cfg(unix)]
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn symlink_gitfile_resolves_relative_target_at_caller_location(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), format);
    std::fs::rename(root.path().join(".git"), root.path().join("metadata")).unwrap();
    std::fs::create_dir(root.path().join("indirection")).unwrap();
    std::fs::write(root.path().join("indirection/file"), b"gitdir: metadata\n").unwrap();
    std::os::unix::fs::symlink("indirection/file", root.path().join(".git")).unwrap();
    let repo = Repository::open(root.path().join(".git")).unwrap();
    assert_eq!(repo.worktree(), Some(canonical(root.path()).as_path()));
    assert_eq!(
        Repository::open(root.path()).unwrap().git_dir(),
        repo.git_dir()
    );
    git(root.path(), &["rev-parse", "--absolute-git-dir"], b"");
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn malformed_registration_does_not_hide_other_metadata(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = linked(root.path(), format);
    std::fs::create_dir(repo.git_dir().join("worktrees/broken")).unwrap();
    let entries = repo.worktrees(10, &AtomicBool::new(false)).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].git_dir.file_name().unwrap(), "broken");
    assert!(matches!(entries[0].state, WorktreeState::Invalid(_)));
    assert!(matches!(entries[1].state, WorktreeState::Available));
}

#[cfg(target_os = "linux")]
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn non_utf8_gitfile_and_worktree_paths_preserve_os_bytes(#[case] format: ObjectFormat) {
    use std::os::unix::ffi::OsStrExt;
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join(std::ffi::OsStr::from_bytes(b"work-\xff"));
    init(&work, format);
    std::fs::rename(
        work.join(".git"),
        root.path().join(std::ffi::OsStr::from_bytes(b"meta-\xfe")),
    )
    .unwrap();
    std::fs::write(work.join(".git"), b"gitdir: ../meta-\xfe\n").unwrap();
    let repo = Repository::open(&work).unwrap();
    assert_eq!(repo.worktree(), Some(canonical(&work).as_path()));
    assert_eq!(
        Repository::discover(&work).unwrap().common_dir(),
        repo.common_dir()
    );
    git(&work, &["rev-parse", "--show-toplevel"], b"");
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn shallow_boundaries_apply_to_ancestry_and_merge_bases_without_changing_bytes(
    #[case] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), format);
    let parent = commit(root.path(), "parent");
    let tip = commit(root.path(), "tip");
    let repo = Repository::open(root.path()).unwrap();
    let before = repo
        .objects(PackLimits::default())
        .unwrap()
        .read(tip, ReadLimits::default())
        .unwrap()
        .unwrap();
    std::fs::write(repo.common_dir().join("shallow"), format!("{tip}\n")).unwrap();
    let repo = Repository::open(root.path()).unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    assert!(
        !objects
            .is_ancestor(parent, tip, HistoryLimits::default())
            .unwrap()
    );
    assert!(
        objects
            .merge_bases(parent, tip, HistoryLimits::default())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        objects.read(tip, ReadLimits::default()).unwrap().unwrap(),
        before
    );
    assert_eq!(
        git(root.path(), &["rev-list", "--count", "HEAD"], b""),
        b"1\n"
    );
    assert!(matches!(
        objects.walk(
            &[tip],
            HistoryLimits {
                max_commits: 0,
                ..HistoryLimits::default()
            }
        ),
        Err(HistoryError::Limit(_))
    ));
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn git_directory_case_alias_uses_filesystem_identity(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), format);
    // The native CI fixture volume is case-insensitive; a case-sensitive volume must run the
    // ordinary identity cases instead, rather than claiming a case alias exists.
    let alias = root.path().join(".GIT");
    if !alias.exists() {
        eprintln!("case-alias fixture unavailable: test volume is case-sensitive");
        return;
    }
    let repo = Repository::open(&alias).unwrap();
    assert_eq!(repo.worktree(), Some(canonical(root.path()).as_path()));
    assert_eq!(repo.common_dir(), canonical(root.path().join(".git")));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn null_shallow_declaration_is_retained_without_inventing_an_object(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), format);
    let tip = commit(root.path(), "root");
    let repo = Repository::open(root.path()).unwrap();
    let null = ObjectId::null(format);
    std::fs::write(repo.common_dir().join("shallow"), format!("{null}\n")).unwrap();
    let repo = Repository::open(root.path()).unwrap();
    assert!(repo.shallow_roots().contains(null));
    let objects = repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        objects.walk(&[tip], HistoryLimits::default()).unwrap(),
        vec![tip]
    );
    assert_eq!(
        git(root.path(), &["rev-list", "--count", "HEAD"], b""),
        b"1\n"
    );
    assert!(
        matches!(objects.walk(&[null], HistoryLimits::default()), Err(HistoryError::Missing(id)) if id == null)
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn linked_shallow_metadata_is_shared_and_relative_extension_is_recognized(
    #[case] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = linked(root.path(), format);
    let child = Repository::open(root.path().join("linked")).unwrap();
    let tip = id(git(root.path(), &["rev-parse", "HEAD"], b""));
    git(
        root.path(),
        &["config", "core.repositoryFormatVersion", "1"],
        b"",
    );
    git(
        root.path(),
        &["config", "extensions.relativeWorktrees", "true"],
        b"",
    );
    std::fs::write(repo.common_dir().join("shallow"), format!("{tip}\n")).unwrap();
    std::fs::write(
        child.git_dir().join("shallow"),
        b"private file must not be read\n",
    )
    .unwrap();
    let child = Repository::open(root.path().join("linked")).unwrap();
    assert_eq!(child.shallow_roots().iter().collect::<Vec<_>>(), vec![tip]);
    assert_eq!(child.object_format(), format);
    assert_eq!(
        std::fs::read(child.git_dir().join("shallow")).unwrap(),
        b"private file must not be read\n"
    );
}

#[cfg(target_os = "macos")]
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn normalization_aliases_share_common_directory_identity(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let composed = root.path().join("caf\u{e9}");
    let decomposed = root.path().join("cafe\u{301}");
    init(&composed, format);
    let repo = Repository::open(&composed).unwrap();
    let alias = repo.open_worktree(&decomposed).unwrap();
    assert_eq!(repo.common_dir(), alias.common_dir());
    assert_eq!(repo.worktree(), alias.worktree());
    git(&decomposed, &["rev-parse", "--show-toplevel"], b"");
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn replaced_checkout_directory_is_invalid_without_losing_registration(
    #[case] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = linked(root.path(), format);
    std::fs::remove_dir_all(root.path().join("linked")).unwrap();
    std::fs::write(root.path().join("linked"), b"replacement file").unwrap();
    let entries = repo.worktrees(10, &AtomicBool::new(false)).unwrap();
    assert!(matches!(entries[0].state, WorktreeState::Invalid(_)));
    assert_eq!(
        Repository::open(&entries[0].git_dir).unwrap().common_dir(),
        repo.common_dir()
    );
    assert_eq!(
        std::fs::read(root.path().join("linked")).unwrap(),
        b"replacement file"
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn separate_metadata_opens_without_inventing_a_checkout(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--template=",
            &format!("--object-format={format}"),
            "--separate-git-dir=metadata",
            "checkout",
        ],
        b"",
    );
    let blob = id(git(
        &root.path().join("checkout"),
        &["hash-object", "-w", "--stdin"],
        b"independent metadata",
    ));
    let repo = Repository::open(root.path().join("metadata")).unwrap();
    assert!(!repo.is_bare());
    assert_eq!(repo.worktree(), None);
    let checkout = repo.open_worktree(root.path().join("checkout")).unwrap();
    assert!(!checkout.is_bare());
    assert_eq!(
        checkout.worktree(),
        Some(canonical(root.path().join("checkout")).as_path())
    );
    assert_eq!(
        git(
            root.path(),
            &["--git-dir=metadata", "rev-parse", "--is-bare-repository"],
            b""
        ),
        b"false\n"
    );
    std::fs::remove_dir_all(root.path().join("checkout")).unwrap();
    let reopened = Repository::open(repo.git_dir()).unwrap();
    assert!(!reopened.is_bare());
    assert_eq!(reopened.worktree(), None);
    assert_eq!(
        reopened.loose_objects().read_blob(blob, 1024).unwrap(),
        b"independent metadata"
    );
}

#[rstest]
#[case::lf(b"\n")]
#[case::crlf(b"\r\n")]
#[case::unterminated(b"")]
#[case::hex_suffix(b"aaaaaaaaaaaaaaaaaaaaaaaa\n")]
#[case::text_suffix(b" trailing-junk\n")]
#[case::nul_suffix(b"\0junk\n")]
#[case::binary_suffix(b"\xff\n")]
fn shallow_prefix_matches_git_history(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] suffix: &[u8],
) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), format);
    let parent = commit(root.path(), "parent");
    let tip = commit(root.path(), "tip");
    let mut bytes = tip.to_string().into_bytes();
    bytes.extend_from_slice(suffix);
    let path = root.path().join(".git/shallow");
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(
        git(root.path(), &["rev-list", "--parents", "HEAD"], b""),
        format!("{tip}\n").as_bytes()
    );
    let repo = Repository::open(root.path()).unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        objects.walk(&[tip], HistoryLimits::default()).unwrap(),
        vec![tip]
    );
    let raw = objects.read(tip, ReadLimits::default()).unwrap().unwrap();
    assert_eq!(
        Commit::parse(format, raw.data()).unwrap().parents(),
        &[parent]
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[rstest]
#[case::short(1, b"", 1)]
#[case::nonhex(1, b"z\n", 1)]
#[case::blank(0, b"\n\n", 2)]
fn invalid_shallow_prefix_matches_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] truncate: usize,
    #[case] suffix: &[u8],
    #[case] line: usize,
) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), format);
    let tip = commit(root.path(), "tip");
    let mut bytes = tip.to_string().into_bytes();
    bytes.truncate(bytes.len() - truncate);
    bytes.extend_from_slice(suffix);
    std::fs::write(root.path().join(".git/shallow"), &bytes).unwrap();
    assert!(
        !layout_git::attempt(root.path(), &["rev-list", "--parents", "HEAD"], b"")
            .status
            .success()
    );
    assert!(
        matches!(Repository::open(root.path()), Err(OpenError::Shallow(ShallowError::InvalidRoot { line: actual })) if actual == line)
    );
}
