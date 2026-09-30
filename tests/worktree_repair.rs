//! Original metadata-only fixtures for exact-registration repair, without repository opening.
use std::fs;
use std::path::{Path, PathBuf};

use girt::{RepositoryLocation, WorktreeAdminError, WorktreeRepair};
use rstest::rstest;

struct Fixture {
    _root: tempfile::TempDir,
    common: PathBuf,
    registration: PathBuf,
    source: PathBuf,
    destination: PathBuf,
}

impl Fixture {
    fn new(forward_relative: bool, back_relative: bool) -> Self {
        Self::named(forward_relative, back_relative, "common", "destination")
    }

    fn named(
        forward_relative: bool,
        back_relative: bool,
        common_name: &str,
        checkout_name: &str,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(root.path()).unwrap();
        let common = base.join(common_name);
        let registration = common.join("worktrees/new");
        let destination = base.join(checkout_name);
        let source = destination.join("temporary");
        fs::create_dir_all(&registration).unwrap();
        fs::create_dir_all(&source).unwrap();
        let forward = if forward_relative {
            PathBuf::from("../..")
                .join(common_name)
                .join("worktrees/new")
        } else {
            registration.clone()
        };
        let back = if back_relative {
            PathBuf::from("../../..")
                .join(checkout_name)
                .join("temporary/.git")
        } else {
            source.join(".git")
        };
        fs::write(
            source.join(".git"),
            format!("gitdir: {}\n", forward.display()),
        )
        .unwrap();
        fs::write(registration.join("gitdir"), format!("{}\n", back.display())).unwrap();
        fs::write(registration.join("commondir"), b"../..\n").unwrap();
        fs::write(registration.join("HEAD"), b"private HEAD untouched\n").unwrap();
        fs::write(registration.join("index"), b"private index untouched").unwrap();
        fs::write(destination.join("user"), b"user contents").unwrap();
        Self {
            _root: root,
            common,
            registration,
            source,
            destination,
        }
    }

    fn prepare(&self) -> Result<WorktreeRepair, WorktreeAdminError> {
        RepositoryLocation::at_git_dir(self.source.join(".git"))
            .unwrap()
            .prepare_worktree_repair()
    }

    fn publish(&self) {
        fs::hard_link(self.source.join(".git"), self.destination.join(".git")).unwrap();
    }
}

#[rstest]
#[case::absolute(false, false)]
#[case::relative(true, true)]
#[case::mixed_forward(true, false)]
#[case::mixed_back(false, true)]
fn preserves_each_captured_link_style(#[case] forward_relative: bool, #[case] back_relative: bool) {
    let fixture = Fixture::new(forward_relative, back_relative);
    let original = fs::read(fixture.source.join(".git")).unwrap();
    let repair = fixture.prepare().unwrap();
    fixture.publish();
    repair.repair(&fixture.destination).unwrap();
    let forward = fs::read_to_string(fixture.destination.join(".git")).unwrap();
    let forward_path = Path::new(forward.trim().strip_prefix("gitdir: ").unwrap());
    assert_eq!(forward_path.is_relative(), forward_relative);
    assert_eq!(
        fs::canonicalize(fixture.destination.join(forward_path)).unwrap(),
        fixture.registration
    );
    let back = fs::read_to_string(fixture.registration.join("gitdir")).unwrap();
    assert_eq!(Path::new(back.trim()).is_relative(), back_relative);
    assert_eq!(
        fs::canonicalize(fixture.registration.join(back.trim())).unwrap(),
        fixture.destination.join(".git")
    );
    assert_eq!(fs::read(fixture.source.join(".git")).unwrap(), original);
    assert_eq!(
        fs::read(fixture.registration.join("commondir")).unwrap(),
        b"../..\n"
    );
    assert_eq!(
        fs::read(fixture.registration.join("HEAD")).unwrap(),
        b"private HEAD untouched\n"
    );
    assert_eq!(
        fs::read(fixture.registration.join("index")).unwrap(),
        b"private index untouched"
    );
    assert_eq!(
        fs::read(fixture.destination.join("user")).unwrap(),
        b"user contents"
    );
}

#[rstest]
#[case::sha1(b"[extensions]\nobjectFormat=sha1\n")]
#[case::sha256(b"[extensions]\nobjectFormat=sha256\n")]
#[case::invalid(b"[malformed configuration")]
fn ignores_repository_storage_and_configuration(#[case] config: &[u8]) {
    let fixture = Fixture::new(true, true);
    fs::write(fixture.common.join("config"), config).unwrap();
    fs::write(fixture.common.join("shallow"), b"not an object id").unwrap();
    let repair = fixture.prepare().unwrap();
    fixture.publish();
    repair.repair(&fixture.destination).unwrap();
    assert_eq!(fs::read(fixture.common.join("config")).unwrap(), config);
    assert_eq!(
        fs::read(fixture.common.join("shallow")).unwrap(),
        b"not an object id"
    );
    assert!(!fixture.common.join("objects").exists());
}

#[rstest]
#[case::source("source")]
#[case::destination("destination")]
#[case::backlink("gitdir")]
#[case::common_link("commondir")]
fn changed_link_bytes_refuse_before_repair(#[case] changed: &str) {
    let fixture = Fixture::new(true, true);
    let repair = fixture.prepare().unwrap();
    fixture.publish();
    let changed = match changed {
        "source" => fixture.source.join(".git"),
        "destination" => fixture.destination.join(".git"),
        name => fixture.registration.join(name),
    };
    fs::write(&changed, b"changed").unwrap();
    let before = fs::read(fixture.destination.join(".git")).unwrap();
    assert!(repair.repair(&fixture.destination).is_err());
    assert_eq!(fs::read(fixture.destination.join(".git")).unwrap(), before);
    assert_eq!(fs::read(changed).unwrap(), b"changed");
    assert!(!fixture.registration.join("girt-admin.lock").exists());
}

#[rstest]
#[case::backlink("gitdir", b"missing\n")]
#[case::common_link("commondir", b".\n")]
fn inconsistent_links_refuse_preparation(#[case] name: &str, #[case] bytes: &[u8]) {
    let fixture = Fixture::new(true, true);
    fs::write(fixture.registration.join(name), bytes).unwrap();
    assert!(fixture.prepare().is_err());
    assert!(!fixture.destination.join(".git").exists());
}

#[test]
fn busy_registration_preserves_published_links() {
    let fixture = Fixture::new(true, true);
    let repair = fixture.prepare().unwrap();
    fixture.publish();
    fs::write(fixture.registration.join("girt-admin.lock"), b"other owner").unwrap();
    assert!(matches!(
        repair.repair(&fixture.destination),
        Err(WorktreeAdminError::Busy(_))
    ));
    assert_eq!(
        fs::read(fixture.destination.join(".git")).unwrap(),
        fs::read(fixture.source.join(".git")).unwrap()
    );
}

#[test]
fn publication_collision_leaves_other_gitfile_untouched() {
    let fixture = Fixture::new(true, true);
    let repair = fixture.prepare().unwrap();
    fs::write(fixture.destination.join(".git"), b"other checkout").unwrap();
    assert!(
        fs::hard_link(
            fixture.source.join(".git"),
            fixture.destination.join(".git")
        )
        .is_err()
    );
    assert!(repair.repair(&fixture.destination).is_err());
    assert_eq!(
        fs::read(fixture.destination.join(".git")).unwrap(),
        b"other checkout"
    );
}

#[test]
fn second_replacement_failure_reports_published_forward_link() {
    let fixture = Fixture::new(true, true);
    let repair = fixture.prepare().unwrap();
    fixture.publish();
    let backlink = fs::read(fixture.registration.join("gitdir")).unwrap();
    fs::write(
        fixture.registration.join("gitdir.girt-repair-temp"),
        b"retained",
    )
    .unwrap();
    let error = repair.repair(&fixture.destination).unwrap_err();
    let WorktreeAdminError::Io { written, .. } = error else {
        panic!("unexpected error: {error}")
    };
    assert_eq!(written, [fixture.destination.join(".git")]);
    assert_eq!(
        fs::read(fixture.registration.join("gitdir")).unwrap(),
        backlink
    );
    assert!(fixture.source.join(".git").is_file());
}

#[cfg(unix)]
#[rstest]
#[case::source(true)]
#[case::destination(false)]
fn same_bytes_different_file_refuses_repair(#[case] source: bool) {
    let fixture = Fixture::new(true, true);
    let repair = fixture.prepare().unwrap();
    fixture.publish();
    let path = if source {
        fixture.source.join(".git")
    } else {
        fixture.destination.join(".git")
    };
    let bytes = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(repair.repair(&fixture.destination).is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn directory_alias_is_allowed_but_gitfile_symlink_is_not() {
    let fixture = Fixture::new(true, true);
    let alias = fixture.common.join("alias");
    std::os::unix::fs::symlink(&fixture.destination, &alias).unwrap();
    let repair = fixture.prepare().unwrap();
    fixture.publish();
    repair.repair(&alias).unwrap();
    let fixture = Fixture::new(true, true);
    let repair = fixture.prepare().unwrap();
    std::os::unix::fs::symlink(
        fixture.source.join(".git"),
        fixture.destination.join(".git"),
    )
    .unwrap();
    assert!(repair.repair(&fixture.destination).is_err());
}

#[test]
fn first_replacement_failure_reports_no_published_paths() {
    let fixture = Fixture::new(true, true);
    let repair = fixture.prepare().unwrap();
    fixture.publish();
    let original = fs::read(fixture.destination.join(".git")).unwrap();
    fs::write(
        fixture.destination.join(".git.girt-repair-temp"),
        b"retained",
    )
    .unwrap();
    let error = repair.repair(&fixture.destination).unwrap_err();
    let WorktreeAdminError::Io { written, .. } = error else {
        panic!("unexpected error: {error}")
    };
    assert!(written.is_empty());
    assert_eq!(
        fs::read(fixture.destination.join(".git")).unwrap(),
        original
    );
}

#[cfg(unix)]
#[rstest]
#[case::registration(false)]
#[case::common(true)]
fn changed_directory_identity_refuses_repair(#[case] common: bool) {
    let fixture = Fixture::new(true, true);
    let repair = fixture.prepare().unwrap();
    fixture.publish();
    let original = fs::read(fixture.destination.join(".git")).unwrap();
    if common {
        let old = fixture.common.with_extension("old");
        fs::rename(&fixture.common, &old).unwrap();
        fs::create_dir_all(fixture.common.join("worktrees")).unwrap();
        fs::rename(old.join("worktrees/new"), &fixture.registration).unwrap();
    } else {
        let old = fixture.registration.with_extension("old");
        fs::rename(&fixture.registration, &old).unwrap();
        fs::create_dir(&fixture.registration).unwrap();
        fs::copy(old.join("gitdir"), fixture.registration.join("gitdir")).unwrap();
        fs::copy(
            old.join("commondir"),
            fixture.registration.join("commondir"),
        )
        .unwrap();
    }
    assert!(repair.repair(&fixture.destination).is_err());
    assert_eq!(
        fs::read(fixture.destination.join(".git")).unwrap(),
        original
    );
}

#[cfg(unix)]
#[test]
fn symlink_registration_leaf_refuses_preparation() {
    let fixture = Fixture::new(true, true);
    let old = fixture.registration.with_extension("old");
    fs::rename(&fixture.registration, &old).unwrap();
    std::os::unix::fs::symlink(&old, &fixture.registration).unwrap();
    assert!(fixture.prepare().is_err());
}

#[rstest]
#[case::space("has space")]
#[case::backslash("has\\backslash")]
#[case::newline("has\nnewline")]
#[case::component_ends_in_newline("trailing\n")]
#[case::carriage_return("has\rcarriage")]
fn repairs_literal_path_components(#[case] component: &str, #[values(false, true)] relative: bool) {
    let fixture = Fixture::named(
        relative,
        relative,
        component,
        &format!("checkout-{component}"),
    );
    let repair = fixture.prepare().unwrap();
    fixture.publish();
    repair.repair(&fixture.destination).unwrap();
    let location = RepositoryLocation::at_git_dir(fixture.destination.join(".git")).unwrap();
    assert_eq!(location.git_dir(), fixture.registration);
    let back = fs::read(fixture.registration.join("gitdir")).unwrap();
    let back = std::str::from_utf8(&back[..back.len() - 1]).unwrap();
    assert_eq!(
        fs::canonicalize(fixture.registration.join(back)).unwrap(),
        fixture.destination.join(".git")
    );
}

#[rstest]
#[case::none(b"")]
#[case::lf(b"\n")]
#[case::crlf(b"\r\n")]
#[case::multiple(b"\r\n\r\n")]
#[case::cr(b"\r")]
fn metadata_kinds_accept_observed_terminators(
    #[case] suffix: &[u8],
    #[values("gitfile", "gitdir", "commondir")] kind: &str,
) {
    let fixture = Fixture::new(true, true);
    let path = if kind == "gitfile" {
        fixture.source.join(".git")
    } else {
        fixture.registration.join(kind)
    };
    let mut bytes = fs::read(&path).unwrap();
    bytes.pop();
    bytes.extend_from_slice(suffix);
    fs::write(&path, &bytes).unwrap();
    let repair = fixture.prepare().unwrap();
    fixture.publish();
    repair.repair(&fixture.destination).unwrap();
    if kind == "commondir" {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[rstest]
#[case::space(b" \n")]
#[case::nul(b"\0\n")]
fn metadata_kinds_reject_literal_unresolvable_suffixes(
    #[case] suffix: &[u8],
    #[values("gitfile", "gitdir", "commondir")] kind: &str,
) {
    let fixture = Fixture::new(true, true);
    let path = if kind == "gitfile" {
        fixture.source.join(".git")
    } else {
        fixture.registration.join(kind)
    };
    let mut bytes = fs::read(&path).unwrap();
    bytes.pop();
    bytes.extend_from_slice(suffix);
    fs::write(&path, bytes).unwrap();
    let result = RepositoryLocation::at_git_dir(fixture.source.join(".git"));
    if let Ok(location) = result {
        assert!(location.prepare_worktree_repair().is_err());
    }
    assert!(!fixture.destination.join(".git").exists());
}

#[rstest]
#[case::ambiguous_final_newline(false)]
#[case::trailing_slash_preserves_component(true)]
fn absolute_common_path_ending_in_newline_needs_a_separator(#[case] separator: bool) {
    let fixture = Fixture::named(true, true, "common\n", "destination");
    let suffix = if separator { "/\n" } else { "\n" };
    fs::write(
        fixture.registration.join("commondir"),
        format!("{}{suffix}", fixture.common.display()),
    )
    .unwrap();
    let location = RepositoryLocation::at_git_dir(fixture.source.join(".git"));
    if separator {
        let repair = location.unwrap().prepare_worktree_repair().unwrap();
        fixture.publish();
        repair.repair(&fixture.destination).unwrap();
    } else {
        assert!(location.is_err());
    }
}
