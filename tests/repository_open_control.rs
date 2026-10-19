//! Bounded opening preserves layout/configuration without publishing a partial handle.
use std::sync::atomic::AtomicBool;

use girt::refs::Backend;
use girt::{InitKind, ObjectFormat, OpenError, OpenLimits, OpenOptions, Repository};
use rstest::rstest;

#[rstest]
#[case(ObjectFormat::Sha1, Backend::Files, InitKind::Bare)]
#[case(ObjectFormat::Sha256, Backend::Files, InitKind::Bare)]
#[case(ObjectFormat::Sha1, Backend::Reftable, InitKind::Bare)]
#[case(ObjectFormat::Sha256, Backend::Reftable, InitKind::Bare)]
#[case(ObjectFormat::Sha1, Backend::Files, InitKind::Worktree)]
#[case(ObjectFormat::Sha256, Backend::Files, InitKind::Worktree)]
#[case(ObjectFormat::Sha1, Backend::Reftable, InitKind::Worktree)]
#[case(ObjectFormat::Sha256, Backend::Reftable, InitKind::Worktree)]
fn metadata_matches_legacy(
    #[case] format: ObjectFormat,
    #[case] backend: Backend,
    #[case] kind: InitKind,
) {
    let root = tempfile::tempdir().unwrap();
    let original =
        Repository::init_with_backend(format, root.path().join("repo"), kind, backend).unwrap();
    let opened = OpenOptions::default()
        .open(root.path().join("repo"), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(opened.git_dir(), original.git_dir());
    assert_eq!(opened.common_dir(), original.common_dir());
    assert_eq!(opened.worktree(), original.worktree());
    assert_eq!(opened.object_dir(), original.object_dir());
    assert_eq!(opened.object_format(), original.object_format());
    assert_eq!(opened.reference_backend(), original.reference_backend());
    let entries = |repo: &Repository| {
        repo.config()
            .entries()
            .iter()
            .map(|entry| {
                (
                    entry.section.clone(),
                    entry.subsection.clone(),
                    entry.name.clone(),
                    entry.value.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(entries(&opened), entries(&original));
}

#[test]
fn pre_cancelled_open_does_not_touch_the_path() {
    assert!(matches!(
        OpenOptions::default().open("does-not-exist", &AtomicBool::new(true)),
        Err(OpenError::Cancelled)
    ));
}

#[test]
fn repeated_head_observations_share_one_budget() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    let head = std::fs::read(repo.git_dir().join("HEAD")).unwrap();
    let exact = head.len() * 3;
    let limits = OpenLimits::default().with_metadata_bytes(exact);
    assert!(
        OpenOptions::default()
            .with_limits(limits)
            .open(repo.git_dir(), &AtomicBool::new(false))
            .is_ok()
    );
    let limits = limits.with_metadata_bytes(exact - 1);
    assert!(matches!(
        OpenOptions::default()
            .with_limits(limits)
            .open(repo.git_dir(), &AtomicBool::new(false)),
        Err(OpenError::Limit(_))
    ));
    assert_eq!(std::fs::read(repo.git_dir().join("HEAD")).unwrap(), head);
}

#[test]
fn oversized_head_is_refused_before_parsing() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    std::fs::write(repo.git_dir().join("HEAD"), [b'x'; 128]).unwrap();
    let options = OpenOptions::default().with_limits(OpenLimits::default().with_metadata_bytes(16));
    assert!(matches!(
        options.open(repo.git_dir(), &AtomicBool::new(false)),
        Err(OpenError::Limit(_))
    ));
}

#[test]
fn shallow_bytes_have_an_independent_limit() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    std::fs::write(
        repo.git_dir().join("shallow"),
        b"1111111111111111111111111111111111111111\n",
    )
    .unwrap();
    let options = OpenOptions::default().with_limits(OpenLimits::default().with_shallow_bytes(1));
    assert!(matches!(
        options.open(repo.git_dir(), &AtomicBool::new(false)),
        Err(OpenError::Shallow(girt::ShallowError::Limit))
    ));
}

#[path = "support/maintenance_git.rs"]
mod oracle;

fn linked_fixture() -> (tempfile::TempDir, std::path::PathBuf, Repository) {
    let root = tempfile::tempdir().unwrap();
    oracle::git(root.path(), &["init", "-q", "main"], b"");
    let main = root.path().join("main");
    oracle::git(
        &main,
        &[
            "-c",
            "maintenance.auto=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "initial",
        ],
        b"",
    );
    oracle::git(&main, &["worktree", "add", "-q", "../linked"], b"");
    let path = root.path().join("linked");
    let linked = Repository::open(&path).unwrap();
    (root, path, linked)
}

#[test]
fn linked_layout_matches_legacy() {
    let (_root, path, legacy) = linked_fixture();
    let opened = OpenOptions::default()
        .open(path, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(opened.git_dir(), legacy.git_dir());
    assert_eq!(opened.common_dir(), legacy.common_dir());
    assert_eq!(opened.worktree(), legacy.worktree());
    assert_eq!(opened.head().unwrap().id, legacy.head().unwrap().id);
}

#[rstest]
#[case::gitfile(0)]
#[case::commondir(1)]
#[case::backlink(2)]
fn oversized_indirections_share_the_metadata_budget(#[case] kind: usize) {
    let (_root, path, legacy) = linked_fixture();
    let target = match kind {
        0 => path.join(".git"),
        1 => legacy.git_dir().join("commondir"),
        2 => legacy.git_dir().join("gitdir"),
        _ => unreachable!(),
    };
    let mut bytes = std::fs::read(&target).unwrap();
    bytes.resize(1024 * 1024, b'\n');
    std::fs::write(&target, &bytes).unwrap();
    let options =
        OpenOptions::default().with_limits(OpenLimits::default().with_metadata_bytes(4096));
    assert!(matches!(
        options.open(&path, &AtomicBool::new(false)),
        Err(OpenError::Limit(_))
    ));
    assert_eq!(std::fs::read(target).unwrap(), bytes);
}

#[test]
fn separate_git_directory_matches_legacy() {
    let root = tempfile::tempdir().unwrap();
    let storage = root.path().join("storage.git");
    oracle::git(
        root.path(),
        &[
            "init",
            "-q",
            "--separate-git-dir",
            storage.to_str().unwrap(),
            "checkout",
        ],
        b"",
    );
    let path = root.path().join("checkout");
    let legacy = Repository::open(&path).unwrap();
    let opened = OpenOptions::default()
        .open(&path, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(opened.git_dir(), legacy.git_dir());
    assert_eq!(opened.worktree(), legacy.worktree());
}

#[test]
fn bootstrap_configuration_has_its_own_limit() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    let limits = OpenLimits::default()
        .with_configuration(girt::config::ResolveLimits::default().with_bytes(1));
    assert!(matches!(
        OpenOptions::default()
            .with_limits(limits)
            .open(repo.git_dir(), &AtomicBool::new(false)),
        Err(OpenError::Resolve(_))
    ));
}

#[test]
fn reftable_branch_context_uses_selected_stack_limits() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
        Backend::Reftable,
    )
    .unwrap();
    let limits = OpenLimits::default()
        .with_references(girt::refs::reftable::StackLimits::default().with_tables(0));
    assert!(matches!(
        OpenOptions::default()
            .with_limits(limits)
            .open(repo.git_dir(), &AtomicBool::new(false)),
        Err(OpenError::References(_))
    ));
}

#[test]
fn nonregular_head_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    std::fs::remove_file(repo.git_dir().join("HEAD")).unwrap();
    std::fs::create_dir(repo.git_dir().join("HEAD")).unwrap();
    assert!(matches!(
        OpenOptions::default().open(repo.git_dir(), &AtomicBool::new(false)),
        Err(OpenError::Unsupported { .. })
    ));
}

#[test]
fn malformed_gitfile_keeps_its_diagnostic_and_bytes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("checkout")).unwrap();
    let path = root.path().join("checkout/.git");
    std::fs::write(&path, b"not a gitfile\n").unwrap();
    assert!(matches!(
        OpenOptions::default().open(root.path().join("checkout"), &AtomicBool::new(false)),
        Err(OpenError::Malformed { .. })
    ));
    assert_eq!(std::fs::read(path).unwrap(), b"not a gitfile\n");
}
