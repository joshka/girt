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

#[rstest]
fn explicit_include_placement_preserves_metadata_and_layer_scopes(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    use girt::config::{ConfigFile, ConfigScope, IncludePlacement};

    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let source = |name: &str| {
        let directory = root.path().join(name);
        fs::create_dir(&directory).unwrap();
        for child in ["a", "b"] {
            fs::write(
                directory.join(child),
                format!("[policy]\nvalue={name}-{child}\n"),
            )
            .unwrap();
        }
        format!(
            "[include]\npath={}\npath={}\n",
            directory.join("a").display(),
            directory.join("b").display()
        )
    };
    let mut inputs = ConfigInputs::default();
    for (name, scope) in [
        ("system", ConfigScope::System),
        ("global", ConfigScope::Global),
    ] {
        let path = root.path().join(format!("{name}-config"));
        fs::write(&path, source(name)).unwrap();
        inputs.files.push(ConfigFile {
            path,
            scope,
            optional: false,
        });
    }
    let common_config = repo.common_dir().join("config");
    let mut bytes = fs::read(&common_config).unwrap();
    bytes.extend_from_slice(b"[extensions]\nworktreeConfig=true\n");
    bytes.extend_from_slice(source("local").as_bytes());
    fs::write(&common_config, &bytes).unwrap();
    fs::write(repo.git_dir().join("config.worktree"), source("worktree")).unwrap();
    inputs.environment = Some(Config::parse(source("environment").as_bytes()).unwrap());
    inputs.command = Some(Config::parse(b"[policy]\nvalue=command\n").unwrap());
    let location = RepositoryLocation::at_git_dir(repo.git_dir()).unwrap();
    let placed = location
        .read_metadata_with_config_and_include_placement(
            &inputs,
            IncludePlacement::AfterSectionReverse,
        )
        .unwrap();
    let normal = location.read_metadata_with_config(&inputs).unwrap();
    let opened = location.open_with_config(&inputs).unwrap();
    let command = location
        .read_metadata_for_command(&inputs, root.path(), None)
        .unwrap();
    let values = |config: &Config| {
        config
            .entries()
            .iter()
            .filter(|entry| entry.section == b"policy")
            .map(|entry| {
                (
                    entry.value.clone().unwrap(),
                    entry.origin.as_ref().unwrap().scope,
                )
            })
            .collect::<Vec<_>>()
    };
    let mut expected = Vec::new();
    for (name, scope) in [
        ("system", ConfigScope::System),
        ("global", ConfigScope::Global),
        ("local", ConfigScope::Local),
        ("worktree", ConfigScope::Worktree),
        ("environment", ConfigScope::Environment),
    ] {
        for child in ["b", "a"] {
            expected.push((format!("{name}-{child}").into_bytes(), scope));
        }
    }
    expected.push((b"command".to_vec(), ConfigScope::Command));
    assert_eq!(values(placed.config()), expected);
    for pair in expected[..10].as_chunks_mut::<2>().0 {
        pair.swap(0, 1);
    }
    assert_eq!(values(normal.config()), expected);
    assert_eq!(values(opened.config()), expected);
    assert_eq!(values(command.config()), expected);
    assert_eq!(placed.object_format(), normal.object_format());
    assert_eq!(placed.reference_backend(), normal.reference_backend());
    assert_eq!(placed.worktree(), normal.worktree());
    assert_eq!(placed.is_bare(), normal.is_bare());
    assert_eq!(fs::read(common_config).unwrap(), bytes);
}

#[rstest]
fn metadata_include_placement_preserves_first_error_and_origin(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    use girt::config::IncludePlacement;

    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let mut config = fs::read(repo.common_dir().join("config")).unwrap();
    config.extend_from_slice(b"[include]\npath=a\npath=b\n");
    fs::write(repo.common_dir().join("config"), config).unwrap();
    fs::write(repo.common_dir().join("a"), "[include]\npath=c\n").unwrap();
    fs::write(repo.common_dir().join("b"), "[broken-b").unwrap();
    fs::write(repo.common_dir().join("c"), "[broken-c").unwrap();
    let location = RepositoryLocation::at_git_dir(repo.git_dir()).unwrap();
    let inputs = ConfigInputs::default();
    let OpenError::Resolve(normal) = location.read_metadata_with_config(&inputs).unwrap_err()
    else {
        panic!("expected include failure")
    };
    let OpenError::Resolve(placed) = location
        .read_metadata_with_config_and_include_placement(
            &inputs,
            IncludePlacement::AfterSectionReverse,
        )
        .unwrap_err()
    else {
        panic!("expected include failure")
    };
    assert_eq!(normal.location, placed.location);
    assert_eq!(normal.included_from, placed.included_from);
    assert_eq!(placed.location.path, Some(repo.common_dir().join("c")));
    assert_eq!(placed.included_from.len(), 2);
}
