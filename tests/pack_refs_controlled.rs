//! Controlled reference packing uses isolated Git-generated fixtures, never upstream source.
use std::fs;
use std::sync::atomic::AtomicBool;

use girt::refs::{PackRefsError, PackRefsLimits, PackRefsOptions};
use girt::{ObjectFormat, PackLimits, Repository};
use rstest::rstest;

#[path = "support/maintenance_git.rs"]
mod oracle;

fn fixture(format: ObjectFormat) -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    oracle::git(
        root.path(),
        &[
            "init",
            "--bare",
            "--template=",
            "--initial-branch=main",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
    let tree = oracle::git(root.path(), &["mktree"], b"");
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let commit = oracle::git(root.path(), &["commit-tree", tree], b"one\n");
    let commit = std::str::from_utf8(&commit).unwrap().trim();
    oracle::git(root.path(), &["update-ref", "refs/heads/main", commit], b"");
    oracle::git(
        root.path(),
        &[
            "-c",
            "tag.gpgsign=false",
            "tag",
            "-a",
            "-m",
            "annotated",
            "annotated",
            commit,
        ],
        b"",
    );
    let repo = Repository::open(root.path()).unwrap();
    (root, repo)
}

#[test]
fn cancellation_precedes_lock_and_publication() {
    let (root, repo) = fixture(ObjectFormat::Sha1);
    let error = repo
        .references()
        .unwrap()
        .pack_controlled(
            &repo.objects_with(PackLimits::default()).unwrap(),
            PackRefsOptions::ALL,
            PackRefsLimits::default(),
            &AtomicBool::new(true),
        )
        .unwrap_err();
    assert!(matches!(error, PackRefsError::Cancelled { .. }));
    assert!(!root.path().join("packed-refs").exists());
    assert!(!root.path().join("packed-refs.lock").exists());
    assert!(root.path().join("refs/heads/main").exists());
}

#[rstest]
#[case::entries(0)]
#[case::references(1)]
#[case::input(2)]
#[case::output(3)]
fn budgets_refuse_before_publication(#[case] resource: usize) {
    let (root, repo) = fixture(ObjectFormat::Sha1);
    let mut limits = PackRefsLimits::default();
    match resource {
        0 => limits.max_entries = 0,
        1 => limits.max_refs = 0,
        2 => limits.max_input_bytes = 0,
        3 => limits.max_output_bytes = 0,
        _ => unreachable!(),
    }
    let before = oracle::git(root.path(), &["show-ref"], b"");
    let error = repo
        .references()
        .unwrap()
        .pack_controlled(
            &repo.objects_with(PackLimits::default()).unwrap(),
            PackRefsOptions::ALL,
            limits,
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(error, PackRefsError::Limit { .. }));
    assert!(!root.path().join("packed-refs").exists());
    assert!(!root.path().join("packed-refs.lock").exists());
    assert_eq!(oracle::git(root.path(), &["show-ref"], b""), before);
}

#[test]
fn tag_peeling_obeys_caller_depth_before_publication() {
    let (root, repo) = fixture(ObjectFormat::Sha1);
    let mut limits = PackRefsLimits::default();
    limits.peel.max_tags = 0;
    let error = repo
        .references()
        .unwrap()
        .pack_controlled(
            &repo.objects_with(PackLimits::default()).unwrap(),
            PackRefsOptions::ALL,
            limits,
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(error, PackRefsError::Peel { .. }));
    assert!(!root.path().join("packed-refs").exists());
    assert!(root.path().join("refs/tags/annotated").exists());
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn packed_values_and_peels_match_git(#[case] format: ObjectFormat) {
    let (root, repo) = fixture(format);
    let before = oracle::git(root.path(), &["show-ref", "--dereference"], b"");
    oracle::git(root.path(), &["pack-refs", "--all", "--no-prune"], b"");
    let expected = fs::read(root.path().join("packed-refs")).unwrap();
    fs::remove_file(root.path().join("packed-refs")).unwrap();
    let report = repo
        .references()
        .unwrap()
        .pack_controlled(
            &repo.objects_with(PackLimits::default()).unwrap(),
            PackRefsOptions::ALL,
            PackRefsLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.rewritten);
    assert_eq!(report.packed, 2);
    assert_eq!(fs::read(root.path().join("packed-refs")).unwrap(), expected);
    assert_eq!(
        oracle::git(root.path(), &["show-ref", "--dereference"], b""),
        before
    );
    oracle::git(root.path(), &["fsck", "--strict"], b"");
}

#[test]
fn existing_packed_records_consume_the_reference_budget() {
    let (root, repo) = fixture(ObjectFormat::Sha1);
    oracle::git(root.path(), &["pack-refs", "--all"], b"");
    let before = fs::read(root.path().join("packed-refs")).unwrap();
    let mut limits = PackRefsLimits::default();
    limits.max_refs = 1;
    let result = repo.references().unwrap().pack_controlled(
        &repo.objects_with(PackLimits::default()).unwrap(),
        PackRefsOptions::ALL,
        limits,
        &AtomicBool::new(false),
    );
    assert!(matches!(
        result,
        Err(PackRefsError::Limit {
            resource: "references",
            ..
        })
    ));
    assert_eq!(fs::read(root.path().join("packed-refs")).unwrap(), before);
    oracle::git(root.path(), &["fsck", "--strict"], b"");
}

#[test]
fn locked_prune_rechecks_consume_the_same_input_budget() {
    let (root, repo) = fixture(ObjectFormat::Sha1);
    let initial_bytes = fs::read(root.path().join("refs/heads/main")).unwrap().len()
        + fs::read(root.path().join("refs/tags/annotated"))
            .unwrap()
            .len();
    let mut limits = PackRefsLimits::default();
    limits.max_input_bytes = initial_bytes;
    let result = repo.references().unwrap().pack_controlled(
        &repo.objects_with(PackLimits::default()).unwrap(),
        PackRefsOptions::ALL,
        limits,
        &AtomicBool::new(false),
    );
    if cfg!(unix) {
        let Err(PackRefsError::Limit {
            resource: "input bytes",
            report,
        }) = result
        else {
            panic!("expected prune budget: {result:?}");
        };
        assert!(report.rewritten);
        assert_eq!(report.pruned, 0);
        assert_eq!(report.retained.len(), 2);
    } else {
        assert_eq!(result.unwrap().retained.len(), 2);
    }
    oracle::git(root.path(), &["fsck", "--strict"], b"");
    assert!(root.path().join("refs/heads/main").exists());
    assert!(root.path().join("refs/tags/annotated").exists());
}
