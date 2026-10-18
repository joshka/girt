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
        let temporary = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            ObjectFormat::Sha1,
            temporary.path().join("r"),
            InitKind::Worktree,
        )
        .unwrap();
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
