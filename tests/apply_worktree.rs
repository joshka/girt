//! Original working-file patch fixtures and isolated Git comparisons.
#![cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "support/apply_git.rs"]
mod support;
use std::sync::atomic::AtomicBool;

use girt::apply::{Patch, TreeOptions, WorktreeLimits};
use girt::attributes::{Attributes, Case, Limits, Source};
use girt::filter::{Drivers, Pipeline, Settings};
use girt::{InitKind, ObjectFormat, Repository};
use rstest::rstest;

struct Fixture {
    _temporary: tempfile::TempDir,
    repo: Repository,
}
impl Fixture {
    fn new() -> Self {
        Self::format(ObjectFormat::Sha1)
    }
    fn format(format: ObjectFormat) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let repo =
            Repository::init(format, temporary.path().join("r"), InitKind::Worktree).unwrap();
        Self {
            _temporary: temporary,
            repo,
        }
    }
    fn root(&self) -> &std::path::Path {
        self.repo.worktree().unwrap()
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let target = self.root().join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, bytes).unwrap();
    }
    fn filters(&self, rules: &[u8]) -> Pipeline {
        let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
        attributes.add(Source::Directory(b""), rules).unwrap();
        Pipeline::from_attributes(attributes, Settings::default(), Drivers::default())
    }
}
const CHANGE: &[u8] = b"--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n";

#[test]
fn prepares_and_rechecks_without_writes_or_index() {
    let fixture = Fixture::new();
    fixture.write("file", b"old\n");
    let patch = Patch::parse(CHANGE).unwrap();
    let mut plan = patch
        .prepare_worktree(&fixture.repo, &mut fixture.filters(b""))
        .unwrap();

    assert_eq!(plan.files()[0].content(), Some(b"new\n".as_slice()));
    assert_eq!(plan.files()[0].hunks.len(), 1);
    plan.verify().unwrap();
    assert_eq!(
        std::fs::read(fixture.root().join("file")).unwrap(),
        b"old\n"
    );
    assert!(
        fixture
            .repo
            .read_index(Default::default())
            .unwrap()
            .is_none()
    );
    fixture.write("file", b"bad\n");
    assert!(plan.verify().is_err());
    assert_eq!(
        std::fs::read(fixture.root().join("file")).unwrap(),
        b"bad\n"
    );
}

#[test]
fn filters_both_preimage_and_working_postimage() {
    let fixture = Fixture::new();
    fixture.write("file", b"old\r\n");
    let plan = Patch::parse(CHANGE)
        .unwrap()
        .prepare_worktree(&fixture.repo, &mut fixture.filters(b"file text eol=crlf\n"))
        .unwrap();

    assert_eq!(plan.files()[0].content(), Some(b"new\r\n".as_slice()));
    assert!(!plan.files()[0].unfiltered);
    assert_eq!(
        std::fs::read(fixture.root().join("file")).unwrap(),
        b"old\r\n"
    );
}

#[test]
fn late_failure_preserves_every_original_file() {
    let fixture = Fixture::new();
    fixture.write("file", b"old\n");
    fixture.write("later", b"actual\n");
    let bytes = [
        CHANGE,
        b"--- a/later\n+++ b/later\n@@ -1 +1 @@\n-wrong\n+changed\n",
    ]
    .concat();

    assert!(
        Patch::parse(&bytes)
            .unwrap()
            .prepare_worktree(&fixture.repo, &mut fixture.filters(b""))
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root().join("file")).unwrap(),
        b"old\n"
    );
    assert_eq!(
        std::fs::read(fixture.root().join("later")).unwrap(),
        b"actual\n"
    );
}

#[rstest]
#[case::metadata(b".git/config")]
#[case::alias(b"git~1/config")]
#[case::traversal(b"../escape")]
#[case::absolute(b"/outside")]
fn refuses_unsafe_paths_before_writes(#[case] path: &[u8]) {
    let fixture = Fixture::new();
    let bytes = [
        b"--- /dev/null\n+++ b/".as_slice(),
        path,
        b"\n@@ -0,0 +1 @@\n+bad\n",
    ]
    .concat();

    let patch = Patch::parse(&bytes).unwrap();
    assert!(
        patch
            .prepare_worktree(&fixture.repo, &mut fixture.filters(b" "))
            .is_err()
    );
}

#[rstest]
#[case::leaf(false)]
#[case::ancestor(true)]
fn refuses_symlinks_without_touching_target(#[case] ancestor: bool) {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("file"), b"old\n").unwrap();
    let patch = if ancestor {
        std::os::unix::fs::symlink(outside.path(), fixture.root().join("link")).unwrap();
        Patch::parse(b"--- a/link/file\n+++ b/link/file\n@@ -1 +1 @@\n-old\n+new\n").unwrap()
    } else {
        std::os::unix::fs::symlink(outside.path().join("file"), fixture.root().join("file"))
            .unwrap();
        Patch::parse(CHANGE).unwrap()
    };

    assert!(
        patch
            .prepare_worktree(&fixture.repo, &mut fixture.filters(b""))
            .is_err()
    );
    assert_eq!(
        std::fs::read(outside.path().join("file")).unwrap(),
        b"old\n"
    );
}

#[test]
fn missing_directories_stay_absent_during_preparation() {
    let fixture = Fixture::new();
    let patch =
        Patch::parse(b"--- /dev/null\n+++ b/new/deep/file\n@@ -0,0 +1 @@\n+added\n").unwrap();
    let mut plan = patch
        .prepare_worktree(&fixture.repo, &mut fixture.filters(b""))
        .unwrap();

    plan.verify().unwrap();
    assert!(!fixture.root().join("new").exists());
    std::fs::create_dir(fixture.root().join("new")).unwrap();
    assert!(plan.verify().is_err());
}

#[test]
fn refuses_projected_nested_bare_repository_before_creating_it() {
    let fixture = Fixture::new();
    let bytes = b"--- /dev/null\n+++ b/nested/HEAD\n@@ -0,0 +1 @@\n+head\n--- /dev/null\n+++ b/nested/objects\n@@ -0,0 +1 @@\n+objects\n--- /dev/null\n+++ b/nested/refs\n@@ -0,0 +1 @@\n+refs\n";

    assert!(
        Patch::parse(bytes)
            .unwrap()
            .prepare_worktree(&fixture.repo, &mut fixture.filters(b""))
            .is_err()
    );
    assert!(!fixture.root().join("nested").exists());
}

#[test]
fn cancellation_and_payload_bounds_leave_files_unchanged() {
    let fixture = Fixture::new();
    fixture.write("file", b"old\n");
    let patch = Patch::parse(CHANGE).unwrap();
    assert!(
        patch
            .prepare_worktree_with(
                &fixture.repo,
                &mut fixture.filters(b""),
                TreeOptions::default(),
                WorktreeLimits::default(),
                &AtomicBool::new(true)
            )
            .is_err()
    );
    let mut limits = WorktreeLimits::default();
    limits.max_retained_bytes = 0;
    assert!(
        patch
            .prepare_worktree_with(
                &fixture.repo,
                &mut fixture.filters(b""),
                TreeOptions::default(),
                limits,
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root().join("file")).unwrap(),
        b"old\n"
    );
}

#[test]
fn matches_git_filtered_working_bytes_without_index_changes() {
    let fixture = Fixture::new();
    fixture.write(".gitattributes", b"file text eol=crlf\n");
    fixture.write("file", b"old\r\n");
    support::git(fixture.root(), &["add", ".gitattributes", "file"], b"");
    let before = fixture
        .repo
        .read_index(Default::default())
        .unwrap()
        .unwrap();
    let plan = Patch::parse(CHANGE)
        .unwrap()
        .prepare_worktree(&fixture.repo, &mut fixture.filters(b"file text eol=crlf\n"))
        .unwrap();

    support::git(fixture.root(), &["apply"], CHANGE);

    assert_eq!(
        std::fs::read(fixture.root().join("file")).unwrap(),
        plan.files()[0].content().unwrap()
    );
    assert_eq!(
        fixture
            .repo
            .read_index(Default::default())
            .unwrap()
            .unwrap(),
        before
    );
}

#[test]
fn publishes_filtered_bytes_and_leaves_index_unchanged() {
    use girt::apply::WorktreeAction;
    let fixture = Fixture::new();
    fixture.write(".gitattributes", b"file text eol=crlf\n");
    fixture.write("file", b"old\r\n");
    support::git(fixture.root(), &["add", ".gitattributes", "file"], b"");
    let index = fixture
        .repo
        .read_index(Default::default())
        .unwrap()
        .unwrap();
    let plan = Patch::parse(CHANGE)
        .unwrap()
        .prepare_worktree(&fixture.repo, &mut fixture.filters(b"file text eol=crlf\n"))
        .unwrap();

    let report = plan.publish().unwrap();

    assert_eq!(
        std::fs::read(fixture.root().join("file")).unwrap(),
        b"new\r\n"
    );
    assert_eq!(
        fixture
            .repo
            .read_index(Default::default())
            .unwrap()
            .unwrap(),
        index
    );
    assert_eq!(report.writes.len(), 2);
    assert_eq!(report.writes[0].action, WorktreeAction::Removed);
    assert_eq!(report.writes[1].action, WorktreeAction::Created);
    assert!(report.writes.iter().all(|write| write.complete));
}

#[test]
fn changed_later_file_is_refused_before_any_write() {
    let fixture = Fixture::new();
    fixture.write("file", b"old\n");
    fixture.write("later", b"old\n");
    let bytes = [
        CHANGE,
        b"--- a/later\n+++ b/later\n@@ -1 +1 @@\n-old\n+new\n",
    ]
    .concat();
    let plan = Patch::parse(&bytes)
        .unwrap()
        .prepare_worktree(&fixture.repo, &mut fixture.filters(b""))
        .unwrap();
    fixture.write("later", b"external\n");

    let failure = plan.publish().unwrap_err();

    assert!(failure.report.writes.is_empty());
    assert_eq!(
        std::fs::read(fixture.root().join("file")).unwrap(),
        b"old\n"
    );
    assert_eq!(
        std::fs::read(fixture.root().join("later")).unwrap(),
        b"external\n"
    );
}

#[test]
fn publishes_nested_addition_and_reports_directories() {
    use girt::apply::WorktreeAction;
    let fixture = Fixture::new();
    let patch =
        Patch::parse(b"--- /dev/null\n+++ b/new/deep/file\n@@ -0,0 +1 @@\n+added\n").unwrap();
    let plan = patch
        .prepare_worktree(&fixture.repo, &mut fixture.filters(b""))
        .unwrap();

    let report = plan.publish().unwrap();

    assert_eq!(
        std::fs::read(fixture.root().join("new/deep/file")).unwrap(),
        b"added\n"
    );
    assert_eq!(
        report
            .writes
            .iter()
            .filter(|write| write.action == WorktreeAction::DirectoryCreated)
            .count(),
        2
    );
    assert_eq!(
        report.writes.last().unwrap().action,
        WorktreeAction::Created
    );
}

#[test]
fn publishes_rename_cycle_from_original_sources() {
    let fixture = Fixture::new();
    fixture.write("a", b"first\n");
    fixture.write("b", b"second\n");
    let bytes = b"diff --git a/a b/b\nsimilarity index 100%\nrename from a\nrename to b\ndiff --git a/b b/a\nsimilarity index 100%\nrename from b\nrename to a\n";
    let plan = Patch::parse(bytes)
        .unwrap()
        .prepare_worktree(&fixture.repo, &mut fixture.filters(b""))
        .unwrap();

    plan.publish().unwrap();

    assert_eq!(
        std::fs::read(fixture.root().join("a")).unwrap(),
        b"second\n"
    );
    assert_eq!(std::fs::read(fixture.root().join("b")).unwrap(), b"first\n");
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn generated_patches_match_git_working_files(#[case] format: ObjectFormat) {
    use std::os::unix::fs::PermissionsExt;
    let delta: Vec<u8> = (0..8192).map(|n| (n % 256) as u8).collect();
    let mut changed_delta = delta.clone();
    changed_delta[500..510].copy_from_slice(b"changed!!!");
    for (before, after, executable) in [
        (b"old".to_vec(), b"new".to_vec(), false),
        (b"\0old\xff".to_vec(), b"\0new\xfe".to_vec(), false),
        (delta, changed_delta, false),
        (b"mode\n".to_vec(), b"mode\n".to_vec(), true),
    ] {
        let fixture = Fixture::format(format);
        fixture.write("file", &before);
        std::fs::set_permissions(
            fixture.root().join("file"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        support::git(fixture.root(), &["add", "file"], b"");
        let index = fixture
            .repo
            .read_index(Default::default())
            .unwrap()
            .unwrap();
        fixture.write("file", &after);
        if executable {
            std::fs::set_permissions(
                fixture.root().join("file"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let patch = support::git(fixture.root(), &["diff", "--binary", "--full-index"], b"");
        fixture.write("file", &before);
        std::fs::set_permissions(
            fixture.root().join("file"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let plan = Patch::parse(&patch)
            .unwrap()
            .prepare_worktree(&fixture.repo, &mut fixture.filters(b""))
            .unwrap();

        plan.publish().unwrap();
        let actual = std::fs::read(fixture.root().join("file")).unwrap();
        let actual_mode = std::fs::metadata(fixture.root().join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(actual, after);
        assert_eq!(
            fixture
                .repo
                .read_index(Default::default())
                .unwrap()
                .unwrap(),
            index
        );
        fixture.write("file", &before);
        std::fs::set_permissions(
            fixture.root().join("file"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        support::git(fixture.root(), &["apply"], &patch);
        assert_eq!(std::fs::read(fixture.root().join("file")).unwrap(), actual);
        assert_eq!(
            std::fs::metadata(fixture.root().join("file"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            actual_mode
        );
    }
}

#[test]
fn required_smudge_failure_precedes_every_write() {
    use girt::filter::Commands;
    let fixture = Fixture::new();
    fixture.write("file", b"old\n");
    let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
    attributes
        .add(Source::Directory(b""), b"*.fail filter=bad\n")
        .unwrap();
    let config =
        girt::Config::parse(b"[filter \"bad\"]\nsmudge = false\nrequired = true\n").unwrap();
    let drivers = Drivers::from_config(&config)
        .unwrap()
        .with_commands(Commands::Run);
    let mut filters = Pipeline::from_attributes(attributes, Settings::default(), drivers);
    let bytes = [
        CHANGE,
        b"--- /dev/null\n+++ b/later.fail\n@@ -0,0 +1 @@\n+new\n",
    ]
    .concat();

    assert!(
        Patch::parse(&bytes)
            .unwrap()
            .prepare_worktree(&fixture.repo, &mut filters)
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root().join("file")).unwrap(),
        b"old\n"
    );
    assert!(!fixture.root().join("later.fail").exists());
}

#[test]
fn configured_volume_aliases_are_refused() {
    let mut fixture = Fixture::new();
    fixture.write("FILE", b"old\n");
    support::git(fixture.root(), &["config", "core.ignoreCase", "true"], b"");
    fixture.repo = Repository::open(fixture.root()).unwrap();

    assert!(
        Patch::parse(CHANGE)
            .unwrap()
            .prepare_worktree(&fixture.repo, &mut fixture.filters(b""))
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root().join("FILE")).unwrap(),
        b"old\n"
    );
}

#[test]
fn replaced_root_symlink_is_refused_before_writing_outside() {
    let fixture = Fixture::new();
    fixture.write("file", b"old\n");
    let plan = Patch::parse(CHANGE)
        .unwrap()
        .prepare_worktree(&fixture.repo, &mut fixture.filters(b""))
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("file"), b"outside\n").unwrap();
    let moved = fixture.root().with_file_name("moved");
    std::fs::rename(fixture.root(), moved).unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.root()).unwrap();

    let failure = plan.publish().unwrap_err();

    assert!(failure.report.writes.is_empty());
    assert_eq!(
        std::fs::read(outside.path().join("file")).unwrap(),
        b"outside\n"
    );
}

#[test]
fn publication_scan_cost_is_refused_before_any_mutation() {
    let fixture = Fixture::new();
    fixture.write("file", b"old\n");
    fixture.write("second", b"old\n");
    fixture.write("third", b"old\n");
    for n in 0..20 {
        fixture.write(&format!("unrelated-{n}"), b"");
    }
    let bytes = [CHANGE, b"--- a/second\n+++ b/second\n@@ -1 +1 @@\n-old\n+new\n--- a/third\n+++ b/third\n@@ -1 +1 @@\n-old\n+new\n"].concat();
    let mut limits = WorktreeLimits::default();
    limits.max_directory_entries = 100;
    let plan = Patch::parse(&bytes)
        .unwrap()
        .prepare_worktree_with(
            &fixture.repo,
            &mut fixture.filters(b""),
            TreeOptions::default(),
            limits,
            &AtomicBool::new(false),
        )
        .unwrap();

    let failure = plan.publish().unwrap_err();

    assert!(failure.report.writes.is_empty());
    assert!(matches!(
        failure.cause.as_ref(),
        girt::apply::WorktreeError::Limit("publication directory inspection entries")
    ));
    for path in ["file", "second", "third"] {
        assert_eq!(std::fs::read(fixture.root().join(path)).unwrap(), b"old\n");
    }
}

#[test]
fn disabled_filemode_is_an_explicit_no_write_refusal() {
    let mut fixture = Fixture::new();
    fixture.write("file", b"old\n");
    support::git(fixture.root(), &["config", "core.fileMode", "false"], b"");
    fixture.repo = Repository::open(fixture.root()).unwrap();

    assert!(matches!(
        Patch::parse(CHANGE)
            .unwrap()
            .prepare_worktree(&fixture.repo, &mut fixture.filters(b"")),
        Err(girt::apply::WorktreeError::Refused {
            reason: "core.fileMode=false requires an explicit mode policy",
            ..
        })
    ));
    assert_eq!(
        std::fs::read(fixture.root().join("file")).unwrap(),
        b"old\n"
    );
}

#[rstest]
#[case::group_only(0o654, false)]
#[case::other_only(0o645, false)]
#[case::owner(0o744, true)]
fn executable_mode_follows_owner_bit(#[case] mode: u32, #[case] executable: bool) {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fixture.write("file", b"old\n");
    std::fs::set_permissions(
        fixture.root().join("file"),
        std::fs::Permissions::from_mode(mode),
    )
    .unwrap();
    let plan = Patch::parse(CHANGE)
        .unwrap()
        .prepare_worktree(&fixture.repo, &mut fixture.filters(b""))
        .unwrap();
    assert_eq!(plan.files()[0].executable(), executable);
}

#[rstest]
#[case::small(16)]
#[case::medium(32)]
#[case::near_limit(64)]
#[case::admitted(128)]
#[case::ample(512)]
fn scan_limit_refusal_never_leaves_planned_partial_changes(#[case] budget: usize) {
    let fixture = Fixture::new();
    fixture.write("file", b"old\n");
    for path in ["unrelated-a", "unrelated-b"] {
        fixture.write(path, b"");
    }
    let bytes = [
        CHANGE,
        b"--- /dev/null\n+++ b/new/deep/added\n@@ -0,0 +1 @@\n+added\n",
    ]
    .concat();
    let mut limits = WorktreeLimits::default();
    limits.max_directory_entries = budget;
    let plan = Patch::parse(&bytes).unwrap().prepare_worktree_with(
        &fixture.repo,
        &mut fixture.filters(b""),
        TreeOptions::default(),
        limits,
        &AtomicBool::new(false),
    );
    match plan {
        Err(_) => {
            assert_eq!(
                std::fs::read(fixture.root().join("file")).unwrap(),
                b"old\n"
            );
            assert!(!fixture.root().join("new").exists());
        }
        Ok(plan) => match plan.publish() {
            Err(failure) => {
                assert!(failure.report.writes.is_empty());
                assert_eq!(
                    std::fs::read(fixture.root().join("file")).unwrap(),
                    b"old\n"
                );
                assert!(!fixture.root().join("new").exists());
            }
            Ok(_) => {
                assert_eq!(
                    std::fs::read(fixture.root().join("file")).unwrap(),
                    b"new\n"
                );
                assert_eq!(
                    std::fs::read(fixture.root().join("new/deep/added")).unwrap(),
                    b"added\n"
                );
            }
        },
    }
}
