use rstest::rstest;

use super::*;

struct Fixture {
    root: tempfile::TempDir,
    private: PathBuf,
    common: PathBuf,
    cwd: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        let common = root.path().join("common");
        let cwd = root.path().join("cwd");
        fs::create_dir(&cwd).unwrap();
        repository(&private, b"[core]\nrepositoryFormatVersion=0\n");
        repository(&common, b"[core]\nrepositoryFormatVersion=0\n");
        Self {
            root,
            private,
            common,
            cwd,
        }
    }

    fn location(&self, common: bool, object: Option<&Path>) -> RepositoryLocation {
        RepositoryLocation::at_git_dir_with_storage(
            &self.private,
            common.then_some(self.common.as_path()),
            object,
        )
        .unwrap()
    }

    fn metadata(&self, common: bool, worktree: Option<&Path>) -> RepositoryMetadata {
        self.location(common, None)
            .read_metadata_for_command(&ConfigInputs::default(), &self.cwd, worktree)
            .unwrap()
    }
}

fn repository(path: &Path, config: &[u8]) {
    fs::create_dir(path).unwrap();
    fs::create_dir(path.join("objects")).unwrap();
    fs::create_dir(path.join("refs")).unwrap();
    fs::write(path.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    fs::write(path.join("config"), config).unwrap();
}

#[test]
fn command_layout_common_override_bypasses_unreadable_commondir() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.private.join("commondir")).unwrap();
    let metadata = fixture.metadata(true, None);
    assert_eq!(metadata.common_dir(), canonical(&fixture.common).unwrap());
    assert_eq!(
        metadata.worktree(),
        Some(canonical(&fixture.cwd).unwrap().as_path())
    );
    assert!(!metadata.is_bare());
    assert!(RepositoryLocation::at_git_dir(&fixture.private).is_err());
}

#[test]
fn command_layout_equal_common_override_still_suppresses_common_settings() {
    let fixture = Fixture::new();
    fs::write(
        fixture.private.join("config"),
        b"[core]\nrepositoryFormatVersion=0\nbare=true\nworktree=invalid\n",
    )
    .unwrap();
    let location =
        RepositoryLocation::at_git_dir_with_storage(&fixture.private, Some(&fixture.private), None)
            .unwrap();
    let metadata = location
        .read_metadata_for_command(&ConfigInputs::default(), &fixture.cwd, None)
        .unwrap();
    assert!(!metadata.is_bare());
    assert!(!metadata.has_conflicting_worktree_config());
    assert_eq!(
        metadata.worktree(),
        Some(canonical(&fixture.cwd).unwrap().as_path())
    );
    assert_eq!(
        metadata.config().value("core", None, "bare"),
        Some(Some(b"true".as_slice()))
    );
}

#[rstest]
#[case::cwd(b"", false, false, "cwd")]
#[case::bare(b"bare=true\n", true, false, "")]
#[case::relative(b"worktree=../missing\n", false, false, "missing")]
#[case::conflict(b"bare=true\nworktree=../missing\n", true, true, "")]
fn command_layout_without_common_override(
    #[case] settings: &[u8],
    #[case] bare: bool,
    #[case] conflict: bool,
    #[case] checkout: &str,
) {
    let fixture = Fixture::new();
    let mut config = b"[core]\nrepositoryFormatVersion=0\n".to_vec();
    config.extend_from_slice(settings);
    fs::write(fixture.private.join("config"), config).unwrap();
    let metadata = fixture.metadata(false, None);
    assert_eq!(metadata.is_bare(), bare);
    assert_eq!(metadata.has_conflicting_worktree_config(), conflict);
    let expected = (!checkout.is_empty()).then(|| {
        available_path(
            &canonical(&fixture.private)
                .unwrap()
                .join("..")
                .join(checkout),
        )
        .unwrap()
    });
    assert_eq!(metadata.worktree(), expected.as_deref());
}

#[test]
fn command_layout_private_config_and_explicit_worktree_precedence() {
    let fixture = Fixture::new();
    fs::write(
        fixture.common.join("config"),
        b"[core]\nrepositoryFormatVersion=0\nbare=true\n[extensions]\nworktreeConfig=true\n",
    )
    .unwrap();
    fs::write(
        fixture.private.join("config.worktree"),
        b"[core]\nbare=false\nworktree=../configured\n",
    )
    .unwrap();
    let configured = fixture.metadata(true, None);
    assert_eq!(
        configured.worktree(),
        Some(
            available_path(&canonical(&fixture.private).unwrap().join("../configured"))
                .unwrap()
                .as_path()
        )
    );
    fs::write(
        fixture.private.join("config.worktree"),
        b"[core]\nbare=true\nworktree=../configured\n",
    )
    .unwrap();
    let destination = fixture.root.path().join("not-created");
    let overridden = fixture.metadata(true, Some(&destination));
    assert!(!overridden.is_bare());
    assert!(!overridden.has_conflicting_worktree_config());
    assert_eq!(
        overridden.worktree(),
        Some(available_path(&destination).unwrap().as_path())
    );
    assert!(!destination.exists());
}

#[test]
fn command_layout_objects_propagate_to_metadata_and_opening() {
    let fixture = Fixture::new();
    let objects = fixture.root.path().join("external-objects");
    fs::create_dir(&objects).unwrap();
    fs::remove_dir(fixture.private.join("objects")).unwrap();
    let location = fixture.location(false, Some(&objects));
    let metadata = location
        .read_metadata_for_command(&ConfigInputs::default(), &fixture.cwd, None)
        .unwrap();
    let normal_metadata = location
        .read_metadata_with_config(&ConfigInputs::default())
        .unwrap();
    let repo = location.open_with_config(&ConfigInputs::default()).unwrap();
    assert_eq!(metadata.object_dir(), objects);
    assert_eq!(normal_metadata.object_dir(), objects);
    assert_eq!(repo.object_dir(), objects);
    assert!(
        RepositoryLocation::at_git_dir(&fixture.private)
            .unwrap()
            .read_metadata_with_config(&ConfigInputs::default())
            .is_err()
    );
}

#[rstest]
#[case::missing("missing")]
#[case::file("file")]
fn command_layout_invalid_selected_objects_do_not_fall_back(#[case] name: &str) {
    let fixture = Fixture::new();
    fs::write(fixture.root.path().join("file"), b"not a directory").unwrap();
    let objects = fixture.root.path().join(name);
    let location = fixture.location(false, Some(&objects));
    assert!(
        location
            .read_metadata_for_command(&ConfigInputs::default(), &fixture.cwd, None)
            .is_err()
    );
}

#[test]
fn command_layout_preserves_normal_linked_backlink_validation() {
    let fixture = Fixture::new();
    fs::write(fixture.private.join("commondir"), b"../common\n").unwrap();
    fs::write(fixture.private.join("gitdir"), b"invalid\0\n").unwrap();
    let location = fixture.location(false, None);
    assert!(
        location
            .read_metadata_with_config(&ConfigInputs::default())
            .is_err()
    );
    let metadata = location
        .read_metadata_for_command(&ConfigInputs::default(), &fixture.cwd, None)
        .unwrap();
    assert_eq!(
        metadata.worktree(),
        Some(canonical(&fixture.cwd).unwrap().as_path())
    );
}

#[test]
fn command_layout_missing_version_is_explicitly_unsupported() {
    let fixture = Fixture::new();
    fs::write(fixture.private.join("config"), b"[core]\nbare=true\n").unwrap();
    let location = fixture.location(false, None);
    assert!(matches!(
        location.read_metadata_for_command(&ConfigInputs::default(), &fixture.cwd, None),
        Err(OpenError::Unsupported { .. })
    ));
    assert!(
        location
            .read_metadata_with_config(&ConfigInputs::default())
            .unwrap()
            .is_bare()
    );
}

#[test]
fn command_layout_rejects_unanchored_and_empty_paths() {
    let fixture = Fixture::new();
    assert!(RepositoryLocation::at_git_dir_with_storage("relative", None, None).is_err());
    assert!(
        RepositoryLocation::at_git_dir_with_storage(&fixture.private, Some(Path::new("")), None)
            .is_err()
    );
    assert!(
        RepositoryLocation::at_git_dir_with_storage(
            &fixture.private,
            None,
            Some(Path::new("relative"))
        )
        .is_err()
    );
    let location = fixture.location(false, None);
    assert!(
        location
            .read_metadata_for_command(&ConfigInputs::default(), Path::new("relative"), None)
            .is_err()
    );
    assert!(
        location
            .read_metadata_for_command(&ConfigInputs::default(), &fixture.cwd, Some(Path::new("")))
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn command_layout_retains_logical_private_alias_for_includes() {
    let fixture = Fixture::new();
    let alias = fixture.root.path().join("alias");
    std::os::unix::fs::symlink(&fixture.private, &alias).unwrap();
    let included = fixture.root.path().join("included");
    fs::write(&included, b"[fixture]\nvalue=matched\n").unwrap();
    let config = format!(
        "[core]\nrepositoryFormatVersion=0\n[includeIf \"gitdir:{}\"]\npath={}\n",
        alias.display(),
        included.display()
    );
    fs::write(fixture.private.join("config"), config).unwrap();
    let location = RepositoryLocation::at_git_dir_with_storage(&alias, None, None).unwrap();
    let metadata = location
        .read_metadata_for_command(&ConfigInputs::default(), &fixture.cwd, None)
        .unwrap();
    assert_eq!(
        metadata.config().value("fixture", None, "value"),
        Some(Some(b"matched".as_slice()))
    );
}

/// Original versioned repositories compare explicit-command layout with installed Git.
#[rstest]
#[case::default(false, b"", false)]
#[case::bare(false, b"bare=true\n", true)]
#[case::configured(false, b"worktree=../configured\n", false)]
#[case::conflict(false, b"bare=true\nworktree=../configured\n", true)]
#[case::common_ignores_bare(true, b"bare=true\n", false)]
#[case::common_ignores_path(true, b"worktree=../configured\n", false)]
fn command_layout_matches_git(#[case] common: bool, #[case] settings: &[u8], #[case] bare: bool) {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.path().join("configured")).unwrap();
    let mut bytes = b"[core]\nrepositoryFormatVersion=0\n".to_vec();
    bytes.extend_from_slice(settings);
    fs::write(fixture.private.join("config"), &bytes).unwrap();
    fs::write(fixture.common.join("config"), &bytes).unwrap();
    let metadata = fixture.metadata(common, None);
    let output = git_layout(&fixture, common, "--is-bare-repository");
    assert!(output.status.success());
    assert_eq!(output.stdout, format!("{bare}\n").as_bytes());
    assert_eq!(metadata.is_bare(), bare);
    assert_eq!(
        metadata.has_conflicting_worktree_config(),
        !output.stderr.is_empty()
    );
}

fn git_layout(fixture: &Fixture, common: bool, flag: &str) -> std::process::Output {
    let empty = fixture.root.path().join("empty-global");
    fs::write(&empty, []).unwrap();
    let mut command = std::process::Command::new("git");
    command
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", empty)
        .arg("--git-dir")
        .arg(&fixture.private)
        .args(["rev-parse", flag])
        .current_dir(&fixture.cwd);
    if common {
        command.env("GIT_COMMON_DIR", &fixture.common);
    }
    command.output().unwrap()
}

#[test]
fn command_layout_rejects_relative_config_sources_before_reading() {
    let fixture = Fixture::new();
    let inputs = ConfigInputs {
        files: vec![ConfigFile {
            path: "relative-config".into(),
            scope: ConfigScope::Global,
            optional: true,
        }],
        ..Default::default()
    };
    assert!(
        fixture
            .location(false, None)
            .read_metadata_for_command(&inputs, &fixture.cwd, None)
            .is_err()
    );
}

#[test]
fn command_layout_self_pointing_commondir_is_still_redirected() {
    let fixture = Fixture::new();
    fs::write(fixture.private.join("commondir"), b".\n").unwrap();
    fs::write(
        fixture.private.join("config"),
        b"[core]\nrepositoryFormatVersion=0\nbare=true\n",
    )
    .unwrap();
    let metadata = fixture.metadata(false, None);
    assert!(!metadata.is_bare());
    assert_eq!(
        metadata.worktree(),
        Some(canonical(&fixture.cwd).unwrap().as_path())
    );
    assert!(
        fixture
            .location(false, None)
            .read_metadata_with_config(&ConfigInputs::default())
            .unwrap()
            .is_bare()
    );
    let output = git_layout(&fixture, false, "--is-bare-repository");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"false\n");
}
