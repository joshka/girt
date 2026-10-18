//! Original disposable repositories and Git stage oracles; no upstream test source.
#![cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "support/rewrite_git.rs"]
mod fixture;

use std::sync::atomic::AtomicBool;

use fixture::{Fixture, git};
use girt::merge::{MarkerOptions, PublicationError};
use girt::{ObjectId, Repository};

fn working() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--template=", "."], b"");
    Fixture { root }
}

fn trees(f: &Fixture) -> [ObjectId; 3] {
    let b = f.blob(b"base\n");
    let o = f.blob(b"ours\n");
    let t = f.blob(b"theirs\n");
    [
        f.tree(&[("100644", b, b"file")]),
        f.tree(&[("100644", o, b"file")]),
        f.tree(&[("100644", t, b"file")]),
    ]
}

#[test]
fn publishes_markers_and_original_conflict_stages() {
    let f = working();
    let [base, ours, theirs] = trees(&f);
    git(
        f.root.path(),
        &["read-tree", "--reset", "-u", &ours.to_string()],
        b"",
    );
    let objects = f.objects();
    let result = objects.merge_trees(base, ours, theirs).unwrap();
    let plan = result
        .prepare_checkout(&objects, &MarkerOptions::default())
        .unwrap();
    assert_eq!(plan.index().entries().len(), 3);
    let repo = Repository::open(f.root.path()).unwrap();
    let report = repo.checkout_merge(Some(ours), &plan).unwrap();
    assert!(report.index_published);
    assert_eq!(
        std::fs::read(f.root.path().join("file")).unwrap(),
        b"<<<<<<< ours\nours\n=======\ntheirs\n>>>>>>> theirs\n"
    );
    let stages = git(f.root.path(), &["ls-files", "-u"], b"");
    let expected: Vec<_> = plan
        .index()
        .entries()
        .iter()
        .flat_map(|entry| format!("100644 {} {}\tfile\n", entry.id, entry.stage as u8).into_bytes())
        .collect();
    assert_eq!(stages, expected);
    assert!(
        repo.read_index(Default::default())
            .unwrap()
            .unwrap()
            .tree_id()
            .is_none()
    );
}

#[test]
fn dirty_baseline_preserves_index_and_working_bytes() {
    let f = working();
    let [base, ours, theirs] = trees(&f);
    git(
        f.root.path(),
        &["read-tree", "--reset", "-u", &ours.to_string()],
        b"",
    );
    let before = std::fs::read(f.root.path().join(".git/index")).unwrap();
    std::fs::write(f.root.path().join("file"), b"uncommitted\n").unwrap();
    let objects = f.objects();
    let result = objects.merge_trees(base, ours, theirs).unwrap();
    let plan = result
        .prepare_checkout(&objects, &MarkerOptions::default())
        .unwrap();
    let error = Repository::open(f.root.path())
        .unwrap()
        .checkout_merge(Some(ours), &plan)
        .unwrap_err();
    assert!(
        matches!(error, PublicationError::Checkout(failure) if failure.report.applied.is_empty() && !failure.report.index_published)
    );
    assert_eq!(
        std::fs::read(f.root.path().join("file")).unwrap(),
        b"uncommitted\n"
    );
    assert_eq!(
        std::fs::read(f.root.path().join(".git/index")).unwrap(),
        before
    );
}

#[test]
fn untracked_obstruction_is_preserved() {
    let f = working();
    let empty = f.tree(&[]);
    let added = f.tree(&[("100644", f.blob(b"tracked\n"), b"file")]);
    std::fs::write(f.root.path().join("file"), b"untracked\n").unwrap();
    let objects = f.objects();
    let result = objects.merge_trees(empty, empty, added).unwrap();
    let plan = result
        .prepare_checkout(&objects, &MarkerOptions::default())
        .unwrap();
    assert!(
        Repository::open(f.root.path())
            .unwrap()
            .checkout_merge(None, &plan)
            .is_err()
    );
    assert_eq!(
        std::fs::read(f.root.path().join("file")).unwrap(),
        b"untracked\n"
    );
    assert!(!f.root.path().join(".git/index").exists());
}

#[test]
fn foreign_index_lock_is_preserved() {
    let f = working();
    let [base, ours, theirs] = trees(&f);
    git(
        f.root.path(),
        &["read-tree", "--reset", "-u", &ours.to_string()],
        b"",
    );
    std::fs::write(f.root.path().join(".git/index.lock"), b"foreign\n").unwrap();
    let objects = f.objects();
    let result = objects.merge_trees(base, ours, theirs).unwrap();
    let plan = result
        .prepare_checkout(&objects, &MarkerOptions::default())
        .unwrap();
    assert!(
        Repository::open(f.root.path())
            .unwrap()
            .checkout_merge(Some(ours), &plan)
            .is_err()
    );
    assert_eq!(
        std::fs::read(f.root.path().join(".git/index.lock")).unwrap(),
        b"foreign\n"
    );
    assert_eq!(
        std::fs::read(f.root.path().join("file")).unwrap(),
        b"ours\n"
    );
}

#[test]
fn cancellation_prevents_object_and_worktree_publication() {
    let f = working();
    let [base, ours, theirs] = trees(&f);
    git(
        f.root.path(),
        &["read-tree", "--reset", "-u", &ours.to_string()],
        b"",
    );
    let objects = f.objects();
    let result = objects.merge_trees(base, ours, theirs).unwrap();
    let plan = result
        .prepare_checkout(&objects, &MarkerOptions::default())
        .unwrap();
    assert!(!objects.contains(plan.tree_id()).unwrap());
    assert!(matches!(
        Repository::open(f.root.path())
            .unwrap()
            .checkout_merge_with(
                Some(ours),
                &plan,
                Default::default(),
                &AtomicBool::new(true)
            ),
        Err(PublicationError::Cancelled)
    ));
    assert!(!objects.contains(plan.tree_id()).unwrap());
    assert_eq!(
        std::fs::read(f.root.path().join("file")).unwrap(),
        b"ours\n"
    );
}

#[test]
fn edited_generated_payloads_must_match_their_claimed_identity() {
    let f = Fixture::new("sha1");
    let b = f.blob(b"a\nb\nc\n");
    let o = f.blob(b"A\nb\nc\n");
    let t = f.blob(b"a\nb\nC\n");
    let base = f.tree(&[("100644", b, b"file")]);
    let ours = f.tree(&[("100644", o, b"file")]);
    let theirs = f.tree(&[("100644", t, b"file")]);
    let objects = f.objects();
    let mut result = objects.merge_trees(base, ours, theirs).unwrap();
    result.blobs[0].1 = b"different bytes".to_vec();
    assert!(matches!(
        result.prepare_checkout(&objects, &MarkerOptions::default()),
        Err(PublicationError::ObjectIdentity)
    ));
}

#[test]
fn marker_output_is_bounded_before_rendering() {
    let f = working();
    let [base, ours, theirs] = trees(&f);
    let objects = f.objects();
    let result = objects.merge_trees(base, ours, theirs).unwrap();
    let mut limits = girt::merge::PreparationLimits::default();
    limits.max_marker_bytes = 0;
    assert!(matches!(
        result.prepare_checkout_with(
            &objects,
            &MarkerOptions::default(),
            limits,
            &AtomicBool::new(false)
        ),
        Err(PublicationError::Limit("marker bytes"))
    ));
}

#[test]
fn index_only_callers_install_clean_generated_content_before_publication() {
    let f = working();
    let b = f.blob(b"a\nb\nc\n");
    let o = f.blob(b"A\nb\nc\n");
    let t = f.blob(b"a\nb\nC\n");
    let base = f.tree(&[("100644", b, b"file")]);
    let ours = f.tree(&[("100644", o, b"file")]);
    let theirs = f.tree(&[("100644", t, b"file")]);
    let objects = f.objects();
    let result = objects.merge_trees(base, ours, theirs).unwrap();
    let plan = result
        .prepare_checkout(&objects, &MarkerOptions::default())
        .unwrap();
    let repo = Repository::open(f.root.path()).unwrap();
    plan.install_objects(&repo.loose_objects()).unwrap();
    let mut edit = repo.edit_index(Default::default()).unwrap();
    edit.replace_index(plan.index().clone()).unwrap();
    edit.commit().unwrap();
    assert_eq!(
        repo.read_index(Default::default())
            .unwrap()
            .unwrap()
            .tree_id(),
        Some(plan.tree_id())
    );
    assert_eq!(git(f.root.path(), &["show", ":file"], b""), b"A\nb\nC\n");
    assert!(!f.root.path().join("file").exists());
}
