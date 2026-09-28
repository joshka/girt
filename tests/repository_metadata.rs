//! Original fixtures distinguish bootstrap validation from storage observation.
use std::fs;
use std::io::Write as _;

use girt::config::ConfigInputs;
use girt::{Config, InitKind, ObjectFormat, OpenError, Repository, RepositoryLocation};
use rstest::rstest;

#[rstest]
#[case::malformed(b"invalid\n" as &[u8], 8)]
#[case::oversized(b"", 16 * 1024 * 1024 + 1)]
fn metadata_does_not_read_shallow_files(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] bytes: &[u8],
    #[case] size: u64,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let mut shallow = fs::File::create(repo.common_dir().join("shallow")).unwrap();
    shallow.write_all(bytes).unwrap();
    shallow.set_len(size).unwrap();
    let location = RepositoryLocation::at_git_dir(repo.git_dir()).unwrap();
    let metadata = location
        .read_metadata_with_config(&ConfigInputs::default())
        .unwrap();
    assert_eq!(metadata.git_dir(), repo.git_dir());
    assert_eq!(metadata.common_dir(), repo.common_dir());
    assert_eq!(metadata.object_dir(), repo.object_dir());
    assert_eq!(metadata.worktree(), None);
    assert!(metadata.is_bare());
    assert_eq!(metadata.format_version(), repo.format_version());
    assert_eq!(metadata.object_format(), format);
    assert_eq!(metadata.reference_backend(), repo.reference_backend());
    assert!(matches!(
        location.open_with_config(&ConfigInputs::default()),
        Err(OpenError::Shallow(_))
    ));
}

#[rstest]
fn metadata_does_not_open_shallow_directory(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    fs::create_dir(repo.common_dir().join("shallow")).unwrap();
    let location = RepositoryLocation::at_git_dir(repo.git_dir()).unwrap();
    assert!(
        location
            .read_metadata_with_config(&ConfigInputs::default())
            .is_ok()
    );
    assert!(matches!(
        location.open_with_config(&ConfigInputs::default()),
        Err(OpenError::Shallow(_))
    ));
}

#[rstest]
#[case::syntax(b"[broken" as &[u8])]
#[case::format(b"[core]\nrepositoryformatversion=9\n")]
#[case::object_format(b"[core]\nrepositoryformatversion=1\n[extensions]\nobjectformat=unknown\n")]
#[case::bare(b"[core]\nbare=invalid\n")]
#[case::worktree(b"[core]\nbare=true\nworktree=checkout\n")]
#[case::extension(b"[extensions]\nunknown=true\n")]
fn bootstrap_errors_precede_shallow_errors(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] invalid_config: &[u8],
) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(repo.common_dir().join("config"))
        .unwrap()
        .write_all(invalid_config)
        .unwrap();
    fs::write(repo.common_dir().join("shallow"), b"invalid\n").unwrap();
    let location = RepositoryLocation::at_git_dir(repo.git_dir()).unwrap();
    let metadata_error = location
        .read_metadata_with_config(&ConfigInputs::default())
        .unwrap_err();
    let open_error = location
        .open_with_config(&ConfigInputs::default())
        .unwrap_err();
    assert!(!matches!(metadata_error, OpenError::Shallow(_)));
    assert_eq!(metadata_error.to_string(), open_error.to_string());
}

#[rstest]
fn bootstrap_validates_layout_before_config(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("HEAD", "objects", "refs")] marker: &str,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    fs::rename(repo.git_dir().join(marker), repo.git_dir().join("removed")).unwrap();
    fs::write(repo.common_dir().join("config"), b"[broken").unwrap();
    let location = RepositoryLocation::at_git_dir(repo.git_dir()).unwrap();
    let error = location
        .read_metadata_with_config(&ConfigInputs::default())
        .unwrap_err();
    assert!(matches!(error, OpenError::Malformed { .. }));
    assert_eq!(
        error.to_string(),
        location
            .open_with_config(&ConfigInputs::default())
            .unwrap_err()
            .to_string()
    );
}

#[rstest]
fn metadata_configuration_is_a_snapshot_with_explicit_overrides(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Worktree).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(repo.common_dir().join("config"))
        .unwrap()
        .write_all(b"[include]\npath=included\n")
        .unwrap();
    let included = repo.common_dir().join("included");
    fs::write(&included, b"[demo]\nvalue=included\nother=snapshot\n").unwrap();
    let inputs = ConfigInputs {
        command: Some(
            Config::parse(b"[demo]\nvalue=override\n[extensions]\nobjectformat=unknown\n").unwrap(),
        ),
        ..Default::default()
    };
    let location = RepositoryLocation::at_git_dir(repo.git_dir()).unwrap();
    let metadata = location.read_metadata_with_config(&inputs).unwrap();
    fs::write(&included, b"[demo]\nother=changed\n").unwrap();
    assert_eq!(
        metadata.config().value("demo", None, "value"),
        Some(Some(b"override".as_slice()))
    );
    assert_eq!(
        metadata.config().value("demo", None, "other"),
        Some(Some(b"snapshot".as_slice()))
    );
    assert_eq!(metadata.object_format(), format);
    assert_eq!(metadata.worktree(), repo.worktree());
    assert!(!metadata.is_bare());
}

#[rstest]
fn metadata_ignores_corrupt_objects_and_index(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let id = repo.loose_objects().write_blob(b"object").unwrap();
    let hex = id.to_string();
    fs::write(
        repo.object_dir().join(&hex[..2]).join(&hex[2..]),
        b"corrupt",
    )
    .unwrap();
    fs::write(repo.git_dir().join("index"), b"corrupt").unwrap();
    let location = RepositoryLocation::at_git_dir(repo.git_dir()).unwrap();
    assert!(
        location
            .read_metadata_with_config(&ConfigInputs::default())
            .is_ok()
    );
    let opened = location.open_with_config(&ConfigInputs::default()).unwrap();
    assert!(
        opened
            .objects(girt::PackLimits::default())
            .unwrap()
            .read(id, girt::ReadLimits::default())
            .is_err()
    );
}

#[rstest]
fn reftable_bootstrap_retains_head_context_and_explicit_open_limits(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        format,
        root.path().join("repo"),
        InitKind::Bare,
        girt::refs::Backend::Reftable,
    )
    .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(repo.common_dir().join("config"))
        .unwrap()
        .write_all(b"[includeIf \"onbranch:main\"]\npath=branch\n")
        .unwrap();
    fs::write(
        repo.common_dir().join("branch"),
        b"[demo]\nvalue=reftable\n",
    )
    .unwrap();
    let location = RepositoryLocation::at_git_dir(repo.git_dir()).unwrap();
    let metadata = location
        .read_metadata_with_config(&ConfigInputs::default())
        .unwrap();
    assert_eq!(metadata.reference_backend(), girt::refs::Backend::Reftable);
    assert_eq!(
        metadata.config().value("demo", None, "value"),
        Some(Some(b"reftable".as_slice()))
    );
    let limits = girt::refs::reftable::StackLimits {
        tables: 0,
        ..Default::default()
    };
    assert!(matches!(
        Repository::open_with_config_and_reference_limits(
            repo.git_dir(),
            &ConfigInputs::default(),
            limits
        ),
        Err(OpenError::References(_))
    ));
}
