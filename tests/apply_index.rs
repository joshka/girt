//! Index-only patch computation and explicit publication, compared with isolated Git.
#[path = "support/apply_git.rs"]
mod support;
use std::sync::atomic::AtomicBool;

use girt::apply::{IndexError, IndexLimits, Patch, TreeError, TreeOptions};
use girt::index::{Entry, Index, Mode, Stage, Stat};
use girt::{ObjectFormat, ObjectId, Repository};
use rstest::rstest;
use support::git;

struct Fixture {
    root: tempfile::TempDir,
    repo: Repository,
    index: Index,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--bare", "--template=", "."], b"");
        let repo = Repository::open(root.path()).unwrap();
        let loose = repo.loose_objects();
        let stat = Stat {
            size: 123,
            ..Stat::default()
        };
        let index = Index::new(
            ObjectFormat::Sha1,
            vec![
                Entry::new(
                    b"a".to_vec(),
                    Mode::Regular,
                    loose.write_blob(b"old\n").unwrap(),
                ),
                Entry::new(
                    b"keep".to_vec(),
                    Mode::Regular,
                    loose.write_blob(b"unchanged\n").unwrap(),
                )
                .with_stat(stat)
                .with_assume_valid(true)
                .with_skip_worktree(true),
            ],
            Default::default(),
        )
        .unwrap();
        let mut edit = repo.edit_index(Default::default()).unwrap();
        edit.replace_index(index.clone()).unwrap();
        edit.commit().unwrap();
        Self { root, repo, index }
    }
}
const PATCH: &[u8] = b"--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";

#[test]
fn compute_check_and_explicitly_publish_without_worktree() {
    let fixture = Fixture::new();
    let objects = fixture.repo.objects().unwrap();
    let target = ObjectId::for_blob(ObjectFormat::Sha1, b"new\n");
    let saved = std::fs::read(fixture.root.path().join("index")).unwrap();
    let mut edit = fixture.repo.edit_index(Default::default()).unwrap();
    let applied = Patch::parse(PATCH)
        .unwrap()
        .apply_index(&objects, edit.index())
        .unwrap();

    assert!(!objects.contains(target).unwrap());
    assert_eq!(
        std::fs::read(fixture.root.path().join("index")).unwrap(),
        saved
    );
    assert_eq!(applied.index().entries()[1], fixture.index.entries()[1]);
    assert_eq!(applied.files()[0].hunks.len(), 1);
    let expected = applied.tree().id();
    assert_eq!(applied.index().tree_id(), Some(expected));
    applied
        .install_objects(&fixture.repo.loose_objects())
        .unwrap();
    edit.replace_index(applied.into_index()).unwrap();
    edit.commit().unwrap();
    assert_eq!(
        git(fixture.root.path(), &["write-tree"], b""),
        format!("{expected}\n").as_bytes()
    );
    assert!(!fixture.root.path().join("a").exists());
}

#[test]
fn late_hunk_failure_preserves_index_and_objects() {
    let fixture = Fixture::new();
    let objects = fixture.repo.objects().unwrap();
    let saved = std::fs::read(fixture.root.path().join("index")).unwrap();
    let bytes = [
        PATCH,
        b"--- a/keep\n+++ b/keep\n@@ -1 +1 @@\n-wrong\n+changed\n",
    ]
    .concat();

    assert!(
        Patch::parse(&bytes)
            .unwrap()
            .apply_index(&objects, &fixture.index)
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root.path().join("index")).unwrap(),
        saved
    );
    assert!(
        !objects
            .contains(ObjectId::for_blob(ObjectFormat::Sha1, b"new\n"))
            .unwrap()
    );
}

#[rstest]
#[case::stage(1)]
#[case::intent(2)]
#[case::sparse(3)]
fn refuses_index_states_needing_policy(#[case] kind: usize) {
    let fixture = Fixture::new();
    let mut entries = fixture.index.entries().to_vec();
    match kind {
        1 => entries[0].stage = Stage::Ours,
        2 => entries[0].intent_to_add = true,
        3 => {
            entries[0].mode = Mode::SparseDirectory;
            entries[0].path = b"dir/".to_vec();
            entries[0].skip_worktree = true;
        }
        _ => unreachable!(),
    }
    let index = Index::new(ObjectFormat::Sha1, entries, Default::default()).unwrap();

    assert!(matches!(
        Patch::parse(PATCH)
            .unwrap()
            .apply_index(&fixture.repo.objects().unwrap(), &index),
        Err(IndexError::UnsupportedEntry { .. })
    ));
}

#[test]
fn bounds_and_cancellation_precede_publication() {
    let fixture = Fixture::new();
    let objects = fixture.repo.objects().unwrap();
    let patch = Patch::parse(PATCH).unwrap();
    let mut limits = IndexLimits::default();
    limits.index = limits.index.with_max_entries(1);
    assert!(matches!(
        patch.apply_index_with(
            &objects,
            &fixture.index,
            TreeOptions::default(),
            limits,
            &AtomicBool::new(false)
        ),
        Err(IndexError::Index(_))
    ));
    assert!(matches!(
        patch.apply_index_with(
            &objects,
            &fixture.index,
            TreeOptions::default(),
            IndexLimits::default(),
            &AtomicBool::new(true)
        ),
        Err(IndexError::Tree(TreeError::Cancelled))
    ));
    let mut limits = IndexLimits::default();
    limits.tree.max_generated_bytes = 0;
    assert!(
        patch
            .apply_index_with(
                &objects,
                &fixture.index,
                TreeOptions::default(),
                limits,
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(
        fixture
            .repo
            .read_index(Default::default())
            .unwrap()
            .unwrap(),
        fixture.index
    );
}

#[test]
fn keeps_unrelated_gitlink_without_resolving_its_commit() {
    let fixture = Fixture::new();
    let id = ObjectId::from_hex(ObjectFormat::Sha1, &"1".repeat(40)).unwrap();
    let mut entries = fixture.index.entries().to_vec();
    entries.push(Entry::new(b"submodule".to_vec(), Mode::Gitlink, id));
    let index = Index::new(ObjectFormat::Sha1, entries, Default::default()).unwrap();

    let applied = Patch::parse(PATCH)
        .unwrap()
        .apply_index(&fixture.repo.objects().unwrap(), &index)
        .unwrap();

    assert_eq!(applied.index().entries()[2], index.entries()[2]);
    assert_eq!(applied.index().tree_id(), Some(applied.tree().id()));
}

#[test]
fn maps_paths_and_checks_result_index_bounds() {
    let fixture = Fixture::new();
    let objects = fixture.repo.objects().unwrap();
    let patch = Patch::parse(b"--- /dev/null\n+++ custom/file\n@@ -0,0 +1 @@\n+added\n").unwrap();
    let mut options = TreeOptions::default();
    options.directory = b"nested".to_vec();
    let applied = patch
        .apply_index_with(
            &objects,
            &fixture.index,
            options.clone(),
            IndexLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(
        applied
            .index()
            .entries()
            .iter()
            .any(|entry| entry.path == b"nested/file")
    );
    let mut limits = IndexLimits::default();
    limits.index = limits.index.with_max_entries(2);

    assert!(matches!(
        patch.apply_index_with(
            &objects,
            &fixture.index,
            options,
            limits,
            &AtomicBool::new(false)
        ),
        Err(IndexError::Index(_))
    ));
    assert!(
        !objects
            .contains(ObjectId::for_blob(ObjectFormat::Sha1, b"added\n"))
            .unwrap()
    );
}

#[test]
fn publishes_standalone_draft_from_resolved_split_snapshot() {
    let fixture = Fixture::new();
    git(fixture.root.path(), &["update-index", "--split-index"], b"");
    let mut edit = fixture.repo.edit_index(Default::default()).unwrap();
    assert!(
        edit.index()
            .extensions()
            .iter()
            .any(|extension| extension.signature() == *b"link")
    );
    let applied = Patch::parse(PATCH)
        .unwrap()
        .apply_index(&fixture.repo.objects().unwrap(), edit.index())
        .unwrap();
    let expected = applied.tree().id();
    assert!(
        !applied
            .index()
            .extensions()
            .iter()
            .any(|extension| extension.signature() == *b"link")
    );

    applied
        .install_objects(&fixture.repo.loose_objects())
        .unwrap();
    edit.replace_index(applied.into_index()).unwrap();
    edit.commit().unwrap();

    assert_eq!(
        git(fixture.root.path(), &["write-tree"], b""),
        format!("{expected}\n").as_bytes()
    );
}

#[test]
fn touched_flags_match_git_cached_application() {
    let fixture = Fixture::new();
    let mut entries = fixture.index.entries().to_vec();
    entries[0].skip_worktree = true;
    entries[0].assume_valid = true;
    let before = Index::new(ObjectFormat::Sha1, entries, Default::default()).unwrap();
    let mut edit = fixture.repo.edit_index(Default::default()).unwrap();
    edit.replace_index(before.clone()).unwrap();
    edit.commit().unwrap();
    assert!(before.entries()[0].skip_worktree);
    assert!(before.entries()[0].assume_valid);
    let applied = Patch::parse(PATCH)
        .unwrap()
        .apply_index(&fixture.repo.objects().unwrap(), &before)
        .unwrap();

    git(fixture.root.path(), &["apply", "--cached"], PATCH);
    let expected = fixture
        .repo
        .read_index(Default::default())
        .unwrap()
        .unwrap();

    assert!(!expected.entries()[0].skip_worktree);
    assert!(!expected.entries()[0].assume_valid);
    assert_eq!(applied.index().entries()[0], expected.entries()[0]);
    assert_eq!(applied.index().entries()[1], before.entries()[1]);
}

#[test]
fn refuses_touched_gitlink_without_writes() {
    let fixture = Fixture::new();
    let id = ObjectId::from_hex(ObjectFormat::Sha1, &"1".repeat(40)).unwrap();
    let index = Index::new(
        ObjectFormat::Sha1,
        vec![Entry::new(b"a".to_vec(), Mode::Gitlink, id)],
        Default::default(),
    )
    .unwrap();
    let objects = fixture.repo.objects().unwrap();

    assert!(matches!(
        Patch::parse(PATCH).unwrap().apply_index(&objects, &index),
        Err(IndexError::Tree(TreeError::Mode))
    ));
    assert!(
        !objects
            .contains(ObjectId::for_blob(ObjectFormat::Sha1, b"new\n"))
            .unwrap()
    );
}
