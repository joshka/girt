//! Independently authored filesystem fixtures for exact metadata selection.
use std::collections::BTreeMap;
use std::fs;

use girt::config::ConfigInputs;
use girt::{InitKind, ObjectFormat, OpenError, Repository, RepositoryLocation};
use rstest::rstest;

#[rstest]
fn exact_directory_does_not_select_nested_repository(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let outer = Repository::init(format, root.path().join("outer"), InitKind::Bare).unwrap();
    let location = RepositoryLocation::at_git_dir(outer.git_dir()).unwrap();
    let nested = Repository::init(format, outer.git_dir().join(".git"), InitKind::Bare).unwrap();
    fs::write(nested.git_dir().join("config"), b"[broken").unwrap();
    let opened = location.open_with_config(&ConfigInputs::default()).unwrap();
    assert_eq!(opened.git_dir(), outer.git_dir());
    assert_eq!(opened.object_format(), format);
    assert!(Repository::open(outer.git_dir()).is_err());
    let repeated = RepositoryLocation::at_git_dir(outer.git_dir()).unwrap();
    assert_eq!(repeated, location);
}

#[rstest]
fn ordinary_metadata_and_checkout_are_distinct(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let repository =
        Repository::init(format, root.path().join("checkout"), InitKind::Worktree).unwrap();
    let location = RepositoryLocation::at_git_dir(repository.git_dir()).unwrap();
    let opened = location.open_with_config(&ConfigInputs::default()).unwrap();
    assert_eq!(opened.worktree(), repository.worktree());
    let checkout = RepositoryLocation::at_git_dir(repository.worktree().unwrap()).unwrap();
    assert_ne!(checkout.git_dir(), location.git_dir());
    assert!(checkout.open_with_config(&ConfigInputs::default()).is_err());
}

#[rstest]
#[case("config", b"[broken" as &[u8])]
#[case("shallow", b"broken\n")]
#[case("HEAD", b"broken\n")]
fn location_defers_content_validation(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] file: &str,
    #[case] content: &[u8],
) {
    let root = tempfile::tempdir().unwrap();
    let repository = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    fs::write(repository.git_dir().join(file), content).unwrap();
    let location = RepositoryLocation::at_git_dir(repository.git_dir()).unwrap();
    assert_eq!(location.common_dir(), repository.common_dir());
    assert!(location.open_with_config(&ConfigInputs::default()).is_err());
}

#[rstest]
fn location_defers_object_directory_validation(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let repository = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    fs::remove_dir_all(repository.git_dir().join("objects")).unwrap();
    fs::write(repository.git_dir().join("objects"), b"not a directory").unwrap();
    let location = RepositoryLocation::at_git_dir(repository.git_dir()).unwrap();
    assert!(location.open_with_config(&ConfigInputs::default()).is_err());
}

#[rstest]
fn gitfile_and_commondir_selection_survive_indirection_changes(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let common = Repository::init(format, root.path().join("common"), InitKind::Bare).unwrap();
    let private = root.path().join("private");
    let checkout = root.path().join("checkout");
    fs::create_dir(&private).unwrap();
    fs::create_dir(&checkout).unwrap();
    fs::write(private.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    fs::write(private.join("commondir"), b"../common\n").unwrap();
    fs::write(private.join("gitdir"), b"../checkout/.git\n").unwrap();
    fs::write(checkout.join(".git"), b"gitdir: ../private\n").unwrap();
    let location = RepositoryLocation::at_git_dir(checkout.join(".git")).unwrap();
    assert_eq!(location.git_dir(), fs::canonicalize(&private).unwrap());
    assert_eq!(location.common_dir(), common.git_dir());
    let direct = RepositoryLocation::at_git_dir(&private).unwrap();
    assert_eq!(
        direct
            .open_with_config(&ConfigInputs::default())
            .unwrap()
            .worktree(),
        Some(fs::canonicalize(&checkout).unwrap().as_path())
    );
    // The stored selection must not be redirected by later edits to commondir.
    fs::write(private.join("commondir"), b"../missing\n").unwrap();
    let opened = location.open_with_config(&ConfigInputs::default()).unwrap();
    assert_eq!(opened.common_dir(), common.git_dir());
    assert_eq!(opened.object_format(), format);
}

#[rstest]
#[case(b"invalid" as &[u8])]
#[case(b"gitdir: \n")]
#[case(b"gitdir: missing\n")]
#[case(b"gitdir: target\0\n")]
fn malformed_gitfiles_are_rejected(#[case] bytes: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pointer");
    fs::write(&path, bytes).unwrap();
    assert!(matches!(
        RepositoryLocation::at_git_dir(&path),
        Err(OpenError::Malformed { .. })
    ));
}

#[rstest]
#[case(b"\n" as &[u8])]
#[case(b"missing\n")]
#[case(b"target\nextra\n")]
fn malformed_commondir_is_rejected(#[case] bytes: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("commondir"), bytes).unwrap();
    assert!(matches!(
        RepositoryLocation::at_git_dir(root.path()),
        Err(OpenError::Malformed { .. })
    ));
}

#[test]
fn missing_exact_location_is_not_found() {
    let root = tempfile::tempdir().unwrap();
    assert!(matches!(
        RepositoryLocation::at_git_dir(root.path().join("missing")),
        Err(OpenError::NotFound(_))
    ));
}

#[cfg(unix)]
#[rstest]
fn logical_alias_includes_and_explicit_environment_are_retained(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let repository = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(repository.git_dir(), &alias).unwrap();
    let inherited = root.path().join("inherited");
    let included = root.path().join("included");
    fs::write(&included, b"[demo]\nvalue=alias\n").unwrap();
    fs::write(
        &inherited,
        format!(
            "[includeIf \"gitdir:{}\"]\npath={}\n",
            alias.display(),
            included.display()
        ),
    )
    .unwrap();
    let mut inputs = ConfigInputs::default();
    inputs.files.push(girt::config::ConfigFile {
        path: inherited,
        scope: girt::config::ConfigScope::Global,
        optional: false,
    });
    let before: BTreeMap<_, _> = std::env::vars_os().collect();
    let location = RepositoryLocation::at_git_dir(&alias).unwrap();
    let opened = location.open_with_config(&inputs).unwrap();
    assert_eq!(
        opened.config().value("demo", None, "value"),
        Some(Some(b"alias".as_slice()))
    );
    assert_eq!(location.git_dir(), repository.git_dir());
    assert_eq!(before, std::env::vars_os().collect());
    fs::write(included, b"[demo]\nvalue=changed\n").unwrap();
    assert_eq!(
        opened.config().value("demo", None, "value"),
        Some(Some(b"alias".as_slice()))
    );
}

#[path = "support/layout_git.rs"]
mod layout_git;

#[rstest]
fn git_separate_metadata_location_matches_explicit_git_directory(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    layout_git::git(
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
    let location = RepositoryLocation::at_git_dir(root.path().join("checkout/.git")).unwrap();
    let opened = location.open_with_config(&ConfigInputs::default()).unwrap();
    let observed = layout_git::git(
        root.path(),
        &["--git-dir=metadata", "rev-parse", "--show-object-format"],
        b"",
    );
    assert_eq!(
        String::from_utf8(observed).unwrap().trim(),
        opened.object_format().to_string()
    );
    assert_eq!(
        opened.git_dir(),
        fs::canonicalize(root.path().join("metadata")).unwrap()
    );
    assert_eq!(
        opened.worktree(),
        Some(
            fs::canonicalize(root.path().join("checkout"))
                .unwrap()
                .as_path()
        )
    );
    fs::write(root.path().join("checkout/.git"), b"gitdir: ../missing\n").unwrap();
    assert_eq!(
        location
            .open_with_config(&ConfigInputs::default())
            .unwrap()
            .git_dir(),
        opened.git_dir()
    );
    let direct = RepositoryLocation::at_git_dir(opened.git_dir()).unwrap();
    assert_eq!(
        direct
            .open_with_config(&ConfigInputs::default())
            .unwrap()
            .git_dir(),
        opened.git_dir()
    );
}

#[rstest]
#[case("pointer", "pointer", b"gitdir: file\n" as &[u8])]
#[case("commondir", "", b"file\n")]
fn indirection_targets_must_be_directories(
    #[case] name: &str,
    #[case] input: &str,
    #[case] bytes: &[u8],
) {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), b"data").unwrap();
    fs::write(root.path().join(name), bytes).unwrap();
    let path = root.path().join(input);
    assert!(matches!(
        RepositoryLocation::at_git_dir(path),
        Err(OpenError::Malformed { .. })
    ));
}
