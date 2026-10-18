//! Tree application against original Git CLI observations in disposable repositories.
#[path = "support/apply_git.rs"]
mod support;

use std::sync::atomic::AtomicBool;

use girt::apply::{Patch, TreeError, TreeLimits, TreeOptions};
use girt::tree::TreeEditor;
use girt::{EntryMode, ObjectFormat, ObjectId, Repository, Tree, TreeValue};
use rstest::rstest;
use support::git;

struct Fixture {
    root: tempfile::TempDir,
    repo: Repository,
    base: ObjectId,
}

impl Fixture {
    fn new(format: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        git(
            root.path(),
            &[
                "init",
                "--bare",
                "--template=",
                &format!("--object-format={format}"),
                ".",
            ],
            b"",
        );
        let repo = Repository::open(root.path()).unwrap();
        let loose = repo.loose_objects();
        let mut editor = TreeEditor::new(&loose);
        editor
            .set(b"f", loose.write_blob(b"old\n").unwrap())
            .unwrap();
        editor
            .set_entry(
                b"nested/g",
                TreeValue {
                    mode: EntryMode::Executable,
                    id: loose.write_blob(b"before\n").unwrap(),
                },
            )
            .unwrap();
        let base = editor.write().unwrap();
        Self { root, repo, base }
    }
}

#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn computes_same_tree_as_git_without_installing_objects(#[case] format: &str) {
    let fixture = Fixture::new(format);
    let objects = fixture.repo.objects().unwrap();
    let bytes = b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n--- a/nested/g\n+++ b/nested/g\n@@ -1 +1 @@\n-before\n+after\n";
    let patch = Patch::parse(bytes).unwrap();
    let new_blob = ObjectId::for_blob(fixture.repo.object_format(), b"new\n");

    let applied = patch.apply_tree(&objects, fixture.base).unwrap();

    assert!(objects.read_blob(new_blob).is_err());
    assert!(objects.read_tree(applied.tree().id()).is_err());
    assert_eq!(applied.files().len(), 2);
    assert_eq!(applied.files()[1].new.unwrap().mode, EntryMode::Executable);
    applied
        .install_objects(&fixture.repo.loose_objects())
        .unwrap();
    assert_eq!(objects.read_blob(new_blob).unwrap(), b"new\n");
    git(
        fixture.root.path(),
        &["read-tree", &fixture.base.to_string()],
        b"",
    );
    git(fixture.root.path(), &["apply", "--cached"], bytes);
    assert_eq!(
        git(fixture.root.path(), &["write-tree"], b""),
        format!("{}\n", applied.tree().id()).as_bytes()
    );
    assert_eq!(
        objects.read_tree(applied.tree().id()).unwrap(),
        applied.tree().clone()
    );
}

#[test]
fn adds_nested_files_and_prunes_removed_parents() {
    let fixture = Fixture::new("sha1");
    let bytes = b"--- /dev/null\n+++ b/new/deep/file\n@@ -0,0 +1 @@\n+added\n--- a/nested/g\n+++ /dev/null\n@@ -1 +0,0 @@\n-before\n";
    let objects = fixture.repo.objects().unwrap();

    let result = Patch::parse(bytes)
        .unwrap()
        .apply_tree(&objects, fixture.base)
        .unwrap();
    result
        .install_objects(&fixture.repo.loose_objects())
        .unwrap();

    git(
        fixture.root.path(),
        &["read-tree", &fixture.base.to_string()],
        b"",
    );
    git(fixture.root.path(), &["apply", "--cached"], bytes);
    assert_eq!(
        git(fixture.root.path(), &["write-tree"], b""),
        format!("{}\n", result.tree().id()).as_bytes()
    );
    assert!(
        !result
            .tree()
            .entries()
            .iter()
            .any(|entry| entry.name == b"nested")
    );
}

#[rstest]
#[case::missing(b"--- a/missing\n+++ b/missing\n@@ -1 +1 @@\n-old\n+new\n")]
#[case::existing_add(b"--- /dev/null\n+++ b/f\n@@ -0,0 +1 @@\n+added\n")]
#[case::leaf_parent(b"--- /dev/null\n+++ b/f/child\n@@ -0,0 +1 @@\n+added\n")]
#[case::dotdot(b"--- a/../f\n+++ b/../f\n@@ -1 +1 @@\n-old\n+new\n")]
#[case::duplicate(
    b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+other\n"
)]
#[case::new_directory_collision(b"--- /dev/null\n+++ b/new/child\n@@ -0,0 +1 @@\n+added\n--- /dev/null\n+++ b/new\n@@ -0,0 +1 @@\n+other\n")]
fn refuses_path_precondition_failures(#[case] bytes: &[u8]) {
    let fixture = Fixture::new("sha1");

    let error = Patch::parse(bytes)
        .unwrap()
        .apply_tree(&fixture.repo.objects().unwrap(), fixture.base)
        .unwrap_err();

    assert!(matches!(error, TreeError::Path(_)));
}

#[test]
fn late_hunk_failure_does_not_install_earlier_blobs() {
    let fixture = Fixture::new("sha1");
    let objects = fixture.repo.objects().unwrap();
    let bytes = b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n--- a/nested/g\n+++ b/nested/g\n@@ -1 +1 @@\n-wrong\n+after\n";

    let error = Patch::parse(bytes)
        .unwrap()
        .apply_tree(&objects, fixture.base)
        .unwrap_err();

    assert!(matches!(error, TreeError::Content { .. }));
    assert!(
        objects
            .read_blob(ObjectId::for_blob(fixture.repo.object_format(), b"new\n"))
            .is_err()
    );
}

#[test]
fn maps_stripped_paths_beneath_directory() {
    let fixture = Fixture::new("sha1");
    let mut options = TreeOptions::default();
    options.strip_components = 0;
    options.directory = b"nested".to_vec();
    let patch = Patch::parse(b"--- g\n+++ g\n@@ -1 +1 @@\n-before\n+after\n").unwrap();

    let result = patch
        .apply_tree_with(
            &fixture.repo.objects().unwrap(),
            fixture.base,
            options,
            TreeLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();

    assert_eq!(result.files()[0].path, b"nested/g");
}

#[test]
fn cancellation_precedes_storage_access() {
    let fixture = Fixture::new("sha1");
    let patch = Patch::parse(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n").unwrap();
    let missing = ObjectId::for_blob(ObjectFormat::Sha1, b"not a tree");

    let error = patch
        .apply_tree_with(
            &fixture.repo.objects().unwrap(),
            missing,
            TreeOptions::default(),
            TreeLimits::default(),
            &AtomicBool::new(true),
        )
        .unwrap_err();

    assert!(matches!(error, TreeError::Cancelled));
}

#[test]
fn aggregate_generated_byte_limit_refuses_result() {
    let fixture = Fixture::new("sha1");
    let patch = Patch::parse(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n").unwrap();
    let mut limits = TreeLimits::default();
    limits.max_generated_bytes = 3;

    let error = patch
        .apply_tree_with(
            &fixture.repo.objects().unwrap(),
            fixture.base,
            TreeOptions::default(),
            limits,
            &AtomicBool::new(false),
        )
        .unwrap_err();

    assert!(matches!(error, TreeError::Limit("generated bytes")));
}

#[test]
fn preserves_opaque_empty_subtrees() {
    let fixture = Fixture::new("sha1");
    let loose = fixture.repo.loose_objects();
    let empty = loose
        .write_tree(&Tree::new(ObjectFormat::Sha1, vec![]).unwrap())
        .unwrap();
    let mut entries = fixture
        .repo
        .objects()
        .unwrap()
        .read_tree(fixture.base)
        .unwrap()
        .entries()
        .to_vec();
    entries.push(girt::TreeEntry {
        name: b"empty".to_vec(),
        mode: EntryMode::Tree,
        id: empty,
    });
    let base = loose
        .write_tree(&Tree::new(ObjectFormat::Sha1, entries).unwrap())
        .unwrap();
    let patch = Patch::parse(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n").unwrap();

    let result = patch
        .apply_tree(&fixture.repo.objects().unwrap(), base)
        .unwrap();

    assert_eq!(
        result
            .tree()
            .entries()
            .iter()
            .find(|entry| entry.name == b"empty")
            .unwrap()
            .id,
        empty
    );
}

#[test]
fn refuses_cross_format_installation_before_writes() {
    let fixture = Fixture::new("sha1");
    let other = Fixture::new("sha256");
    let patch = Patch::parse(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n").unwrap();
    let result = patch
        .apply_tree(&fixture.repo.objects().unwrap(), fixture.base)
        .unwrap();

    assert!(matches!(
        result.install_objects(&other.repo.loose_objects()),
        Err(TreeError::ObjectFormat)
    ));
    assert!(
        other
            .repo
            .objects()
            .unwrap()
            .read_blob(ObjectId::for_blob(ObjectFormat::Sha256, b"new\n"))
            .is_err()
    );
}

#[rstest]
#[case::entries(0, 128, 16 * 1024 * 1024, "directory entries")]
#[case::depth(1_000_000, 0, 16 * 1024 * 1024, "path depth")]
#[case::paths(1_000_000, 128, 0, "path bytes")]
fn enforces_tree_structure_limits(
    #[case] entries: usize,
    #[case] depth: usize,
    #[case] paths: usize,
    #[case] expected: &'static str,
) {
    let fixture = Fixture::new("sha1");
    let patch = Patch::parse(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n").unwrap();
    let mut limits = TreeLimits::default();
    limits.max_entries = entries;
    limits.max_depth = depth;
    limits.max_path_bytes = paths;

    let error = patch
        .apply_tree_with(
            &fixture.repo.objects().unwrap(),
            fixture.base,
            TreeOptions::default(),
            limits,
            &AtomicBool::new(false),
        )
        .unwrap_err();

    assert!(matches!(error, TreeError::Limit(actual) if actual == expected));
}

#[rstest]
#[case::root(b"--- /dev/null\n+++ b/.git/hooks/post-checkout\n@@ -0,0 +1 @@\n+added\n")]
#[case::nested_case(b"--- /dev/null\n+++ b/sub/.GIT/config\n@@ -0,0 +1 @@\n+added\n")]
fn refuses_repository_metadata_components_like_git(#[case] bytes: &[u8]) {
    let fixture = Fixture::new("sha1");
    let error = Patch::parse(bytes)
        .unwrap()
        .apply_tree(&fixture.repo.objects().unwrap(), fixture.base)
        .unwrap_err();

    assert!(matches!(error, TreeError::Path(_)));
    git(
        fixture.root.path(),
        &["read-tree", &fixture.base.to_string()],
        b"",
    );
    assert!(
        !support::git_output(
            fixture.root.path(),
            &["apply", "--cached", "--check"],
            bytes
        )
        .status
        .success()
    );
}

#[test]
fn refuses_directory_to_file_transition_that_git_accepts() {
    let fixture = Fixture::new("sha1");
    let bytes = b"--- a/nested/g\n+++ /dev/null\n@@ -1 +0,0 @@\n-before\n--- /dev/null\n+++ b/nested\n@@ -0,0 +1 @@\n+file\n";

    let error = Patch::parse(bytes)
        .unwrap()
        .apply_tree(&fixture.repo.objects().unwrap(), fixture.base)
        .unwrap_err();

    assert!(matches!(error, TreeError::Path(_)));
    git(
        fixture.root.path(),
        &["read-tree", &fixture.base.to_string()],
        b"",
    );
    git(
        fixture.root.path(),
        &["apply", "--cached", "--check"],
        bytes,
    );
}

#[test]
fn file_count_limit_precedes_storage_access() {
    let fixture = Fixture::new("sha1");
    let patch = Patch::parse(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n").unwrap();
    let mut limits = TreeLimits::default();
    limits.content.max_files = 0;
    let missing = ObjectId::for_blob(ObjectFormat::Sha1, b"not a tree");

    let error = patch
        .apply_tree_with(
            &fixture.repo.objects().unwrap(),
            missing,
            TreeOptions::default(),
            limits,
            &AtomicBool::new(false),
        )
        .unwrap_err();

    assert!(matches!(error, TreeError::Limit("files")));
}
