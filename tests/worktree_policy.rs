//! Original explicit orphan-creation policy fixtures; no ambient configuration or umask mutation.
use std::fs;

use girt::index::{Index, Limits};
use girt::refs::{Backend, RefName};
use girt::{
    InitKind, ObjectFormat, OrphanWorktreeOptions, Repository, SharedPermissions,
    WorktreeDurability, WorktreeLinkStyle,
};
use rstest::rstest;

fn repository(format: ObjectFormat, backend: Backend) -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        format,
        root.path().join("main"),
        InitKind::Worktree,
        backend,
    )
    .unwrap();
    (root, repo)
}
fn branch() -> RefName {
    RefName::new(b"refs/heads/policy-fixture").unwrap()
}

#[rstest]
#[case::none(false, false)]
#[case::index(true, false)]
#[case::references(false, true)]
#[case::both(true, true)]
fn selected_durability_creates_both_formats_and_backends(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(Backend::Files, Backend::Reftable)] backend: Backend,
    #[case] index: bool,
    #[case] references: bool,
) {
    let (root, repo) = repository(format, backend);
    let linked = repo
        .create_orphan_worktree_with_options(
            root.path().join("linked"),
            &branch(),
            4,
            OrphanWorktreeOptions {
                durability: WorktreeDurability { index, references },
                link_style: WorktreeLinkStyle::Absolute,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(linked.object_format(), format);
    assert_eq!(linked.reference_backend(), backend);
    assert!(
        linked
            .read_index(Limits::default())
            .unwrap()
            .unwrap()
            .entries()
            .is_empty()
    );
    assert!(!linked.git_dir().join("index.lock").exists());
    assert!(
        linked
            .references()
            .unwrap()
            .read(&branch())
            .unwrap()
            .is_none()
    );
}

#[rstest]
fn redirected_index_and_captured_private_config(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(Backend::Files, Backend::Reftable)] backend: Backend,
) {
    let (root, repo) = repository(format, backend);
    let config_path = repo.common_dir().join("config");
    let mut config = fs::read(&config_path).unwrap();
    config.extend_from_slice(b"\n[extensions]\nworktreeConfig=true\n");
    fs::write(&config_path, &config).unwrap();
    let source_private = b"[core]\nbare=false\n[fixture]\nsource=unchanged\n";
    fs::write(repo.git_dir().join("config.worktree"), source_private).unwrap();
    let captured = b"[fixture]\ncaptured=chosen\n";
    let selected = root.path().join("alternate.index");
    fs::write(
        &selected,
        Index::empty(format).encode(Limits::default()).unwrap(),
    )
    .unwrap();
    let linked = repo
        .create_orphan_worktree_with_options(
            root.path().join("linked"),
            &branch(),
            4,
            OrphanWorktreeOptions {
                index_path: Some(selected.clone()),
                private_config: Some(captured.to_vec()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(!linked.git_dir().join("index").exists());
    assert!(
        Index::parse(format, &fs::read(selected).unwrap(), Limits::default())
            .unwrap()
            .entries()
            .is_empty()
    );
    assert_eq!(
        fs::read(linked.git_dir().join("config.worktree")).unwrap(),
        captured
    );
    assert_eq!(
        fs::read(repo.git_dir().join("config.worktree")).unwrap(),
        source_private
    );
    assert_eq!(fs::read(config_path).unwrap(), config);
}

#[cfg(unix)]
#[rstest]
fn exact_permissions_touch_only_selected_metadata(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(Backend::Files, Backend::Reftable)] backend: Backend,
) {
    use std::os::unix::fs::PermissionsExt;
    let (root, repo) = repository(format, backend);
    let ordinary = root.path().join("ordinary");
    fs::write(&ordinary, b"ordinary umask").unwrap();
    let ordinary_mode = fs::metadata(ordinary).unwrap().permissions().mode() & 0o777;
    let linked = repo
        .create_orphan_worktree_with_options(
            root.path().join("linked"),
            &branch(),
            4,
            OrphanWorktreeOptions {
                shared_permissions: SharedPermissions::Exact(0o660),
                private_config: Some(b"[fixture]\nvalue=retained\n".to_vec()),
                ..Default::default()
            },
        )
        .unwrap();
    let mode = |path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(linked.git_dir().join("HEAD")), 0o660);
    assert_eq!(mode(linked.git_dir().join("index")), 0o660);
    assert_eq!(mode(repo.common_dir().join("worktrees")), 0o770);
    assert_eq!(mode(linked.git_dir().join("refs")), 0o770);
    assert_eq!(mode(linked.git_dir().join("commondir")), ordinary_mode);
    assert_eq!(mode(linked.git_dir().join("gitdir")), ordinary_mode);
    assert_eq!(
        mode(linked.git_dir().join("config.worktree")),
        ordinary_mode
    );
    assert_eq!(mode(root.path().join("linked/.git")), ordinary_mode);
    if backend == Backend::Reftable {
        assert_eq!(mode(linked.git_dir().join("reftable")), 0o770);
        assert_eq!(mode(linked.git_dir().join("reftable/initial.ref")), 0o660);
        assert_eq!(mode(linked.git_dir().join("reftable/tables.list")), 0o660);
    }
}

#[rstest]
#[case::no_owner_write(0o444)]
#[case::out_of_range(0o1660)]
fn invalid_exact_mode_refuses_before_creation(#[case] mode: u16) {
    let (root, repo) = repository(ObjectFormat::Sha1, Backend::Files);
    assert!(
        repo.create_orphan_worktree_with_options(
            root.path().join("linked"),
            &branch(),
            4,
            OrphanWorktreeOptions {
                shared_permissions: SharedPermissions::Exact(mode),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(!repo.common_dir().join("worktrees").exists());
    assert!(!root.path().join("linked").exists());
}

#[rstest]
fn invalid_redirected_index_is_retained_with_registration(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (root, repo) = repository(format, Backend::Files);
    let selected = root.path().join("alternate.index");
    fs::write(&selected, b"invalid original").unwrap();
    let error = repo
        .create_orphan_worktree_with_options(
            root.path().join("linked"),
            &branch(),
            4,
            OrphanWorktreeOptions {
                index_path: Some(selected.clone()),
                ..Default::default()
            },
        )
        .unwrap_err();
    let girt::CreateWorktreeError::IndexStorage {
        registration: Some(registration),
        ..
    } = error
    else {
        panic!("unexpected error: {error}")
    };
    assert!(registration.is_dir());
    assert_eq!(fs::read(selected).unwrap(), b"invalid original");
    assert!(!root.path().join("alternate.index.lock").exists());
    assert!(!root.path().join("linked").exists());
}
