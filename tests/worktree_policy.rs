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
fn invalid_redirected_index_refuses_before_registration(
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
        registration: None, ..
    } = error
    else {
        panic!("unexpected error: {error}")
    };
    assert!(!repo.common_dir().join("worktrees").exists());
    assert_eq!(fs::read(selected).unwrap(), b"invalid original");
    assert!(!root.path().join("alternate.index.lock").exists());
    assert!(!root.path().join("linked").exists());
}

#[rstest]
#[case::malformed(false)]
#[case::directory(true)]
fn metadata_creation_ignores_shallow_and_default_index(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(Backend::Files, Backend::Reftable)] backend: Backend,
    #[case] directory: bool,
) {
    let (root, repo) = repository(format, backend);
    for name in ["shallow", "index"] {
        let path = repo.git_dir().join(name);
        if directory {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, b"invalid original").unwrap();
        }
    }
    assert!(Repository::open(root.path().join("main")).is_err());
    let metadata = girt::RepositoryLocation::at_git_dir(repo.git_dir())
        .unwrap()
        .read_metadata_with_config(&girt::config::ConfigInputs::default())
        .unwrap();
    let linked = metadata
        .create_orphan_worktree_with_options(
            root.path().join("linked"),
            &branch(),
            4,
            OrphanWorktreeOptions::default(),
        )
        .unwrap();
    let observed = linked
        .read_metadata_with_config(&girt::config::ConfigInputs::default())
        .unwrap();
    assert_eq!(observed.object_format(), format);
    assert_eq!(observed.reference_backend(), backend);
    let index = fs::read(linked.git_dir().join("index")).unwrap();
    assert_eq!(
        index,
        Index::empty(format).encode(Limits::default()).unwrap()
    );
    for name in ["shallow", "index"] {
        let path = repo.git_dir().join(name);
        if directory {
            assert!(path.is_dir());
        } else {
            assert_eq!(fs::read(path).unwrap(), b"invalid original");
        }
    }
    assert!(
        linked
            .open_with_config(&girt::config::ConfigInputs::default())
            .is_err()
    );
}

#[rstest]
fn full_repository_creation_still_reopens_shallow(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(Backend::Files, Backend::Reftable)] backend: Backend,
) {
    let (root, repo) = repository(format, backend);
    fs::write(repo.common_dir().join("shallow"), b"invalid original").unwrap();
    let error = repo
        .create_orphan_worktree(root.path().join("linked"), &branch(), 4)
        .unwrap_err();
    let girt::CreateWorktreeError::Open { registration, .. } = error else {
        panic!("{error}")
    };
    assert!(registration.join("index").is_file());
    assert!(root.path().join("linked/.git").is_file());
}

#[rstest]
#[case::malformed("malformed")]
#[case::directory("directory")]
#[case::locked("locked")]
fn metadata_selected_index_refuses_before_publication(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(Backend::Files, Backend::Reftable)] backend: Backend,
    #[case] kind: &str,
) {
    let (root, repo) = repository(format, backend);
    let selected = root.path().join("selected");
    match kind {
        "directory" => fs::create_dir(&selected).unwrap(),
        "locked" => fs::write(root.path().join("selected.lock"), b"other owner").unwrap(),
        _ => fs::write(&selected, b"invalid original").unwrap(),
    }
    let metadata = girt::RepositoryLocation::at_git_dir(repo.git_dir())
        .unwrap()
        .read_metadata_with_config(&girt::config::ConfigInputs::default())
        .unwrap();
    let error = metadata
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
    assert!(matches!(
        error,
        girt::CreateWorktreeError::IndexStorage {
            registration: None,
            ..
        }
    ));
    assert!(!repo.common_dir().join("worktrees").exists());
    assert!(!root.path().join("linked").exists());
    match kind {
        "directory" => assert!(selected.is_dir()),
        "locked" => assert_eq!(
            fs::read(root.path().join("selected.lock")).unwrap(),
            b"other owner"
        ),
        _ => assert_eq!(fs::read(selected).unwrap(), b"invalid original"),
    }
    if kind != "locked" {
        assert!(!root.path().join("selected.lock").exists());
    }
}

#[rstest]
fn metadata_reference_refusals_precede_creation(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(false, true)] existing: bool,
) {
    let (root, repo) = repository(format, Backend::Files);
    fs::write(repo.common_dir().join("packed-refs"), b"invalid original\n").unwrap();
    if existing {
        fs::write(
            repo.common_dir().join("refs/heads/policy-fixture"),
            format!("{}\n", "1".repeat(format.digest_len() * 2)),
        )
        .unwrap();
    }
    let metadata = girt::RepositoryLocation::at_git_dir(repo.git_dir())
        .unwrap()
        .read_metadata_with_config(&girt::config::ConfigInputs::default())
        .unwrap();
    let error = metadata
        .create_orphan_worktree_with_options(
            root.path().join("linked"),
            &branch(),
            4,
            OrphanWorktreeOptions::default(),
        )
        .unwrap_err();
    if existing {
        assert!(matches!(error, girt::CreateWorktreeError::ExistingBranch));
    } else {
        assert!(matches!(error, girt::CreateWorktreeError::References(_)));
    }
    assert!(!repo.common_dir().join("worktrees").exists());
    assert!(!root.path().join("linked").exists());
}

#[rstest]
fn selected_index_lock_is_released_when_registration_fails(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(Backend::Files, Backend::Reftable)] backend: Backend,
) {
    let (root, repo) = repository(format, backend);
    let selected = root.path().join("selected");
    let original = Index::empty(format).encode(Limits::default()).unwrap();
    fs::write(&selected, &original).unwrap();
    fs::create_dir_all(repo.common_dir().join("worktrees/linked")).unwrap();
    let error = repo
        .create_orphan_worktree_with_options(
            root.path().join("linked"),
            &branch(),
            1,
            OrphanWorktreeOptions {
                index_path: Some(selected.clone()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(error, girt::CreateWorktreeError::Limit));
    assert_eq!(fs::read(selected).unwrap(), original);
    assert!(!root.path().join("selected.lock").exists());
    assert!(!root.path().join("linked").exists());
}

#[test]
fn command_storage_selection_creates_metadata_worktree_without_reading_shallow() {
    let (root, private) = repository(ObjectFormat::Sha1, Backend::Files);
    let common = Repository::init_with_backend(
        ObjectFormat::Sha1,
        root.path().join("selected-common"),
        InitKind::Bare,
        Backend::Files,
    )
    .unwrap();
    let objects = root.path().join("selected-objects");
    fs::rename(common.object_dir(), &objects).unwrap();
    fs::create_dir(private.git_dir().join("commondir")).unwrap();
    let shallow = b"invalid shallow boundary\n";
    fs::write(common.common_dir().join("shallow"), shallow).unwrap();
    let inputs = girt::config::ConfigInputs::default();
    let location = girt::RepositoryLocation::at_git_dir_with_storage(
        private.git_dir(),
        Some(common.common_dir()),
        Some(&objects),
    )
    .unwrap();
    let metadata = location
        .read_metadata_for_command(&inputs, root.path(), None)
        .unwrap();
    assert_eq!(metadata.object_dir(), objects);
    let destination = root.path().join("linked");
    let linked = metadata
        .create_orphan_worktree_with_options(
            &destination,
            &branch(),
            4,
            OrphanWorktreeOptions::default(),
        )
        .unwrap();
    assert_eq!(linked.common_dir(), common.common_dir());
    assert_eq!(
        linked.git_dir(),
        common.common_dir().join("worktrees/linked")
    );
    assert_eq!(
        fs::read(linked.git_dir().join("HEAD")).unwrap(),
        b"ref: refs/heads/policy-fixture\n"
    );
    let from_checkout = girt::RepositoryLocation::at_git_dir(destination.join(".git")).unwrap();
    assert_eq!(from_checkout.git_dir(), linked.git_dir());
    assert_eq!(
        fs::read(common.common_dir().join("shallow")).unwrap(),
        shallow
    );
    assert!(!common.common_dir().join("objects").exists());
    assert!(!private.common_dir().join("worktrees").exists());
    assert!(private.git_dir().join("commondir").is_dir());
}
