//! Controlled graph construction leaves publication to the caller.
use std::sync::atomic::AtomicBool;

use girt::commit_graph::{CommitGraph, GraphWriteError};
use girt::{HistoryLimits, InitKind, ObjectFormat, Repository};

#[test]
fn pre_cancelled_build_refuses_before_inspecting_roots() {
    let directory = tempfile::tempdir().unwrap();
    let repository = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("r"),
        InitKind::Bare,
    )
    .unwrap();
    let objects = repository.objects().unwrap();
    let absent = girt::ObjectId::for_blob(ObjectFormat::Sha1, b"missing");
    assert!(matches!(
        CommitGraph::from_objects_controlled(
            &objects,
            &[absent],
            HistoryLimits::default(),
            &AtomicBool::new(true)
        ),
        Err(GraphWriteError::Cancelled)
    ));
    assert!(!repository.object_dir().join("info/commit-graph").exists());
}

#[test]
fn uncancelled_empty_graph_matches_existing_constructor() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let directory = tempfile::tempdir().unwrap();
        let repository =
            Repository::init(format, directory.path().join("r"), InitKind::Bare).unwrap();
        let objects = repository.objects().unwrap();
        let old = CommitGraph::from_objects_with(&objects, &[], HistoryLimits::default()).unwrap();
        let controlled = CommitGraph::from_objects_controlled(
            &objects,
            &[],
            HistoryLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(old.encode().unwrap(), controlled.encode().unwrap());
    }
}

#[test]
fn commit_limit_still_refuses_without_publication() {
    let directory = tempfile::tempdir().unwrap();
    let repository = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("r"),
        InitKind::Bare,
    )
    .unwrap();
    let objects = repository.objects().unwrap();
    let absent = girt::ObjectId::for_blob(ObjectFormat::Sha1, b"missing");
    let limits = HistoryLimits::default().with_max_commits(0);
    assert!(matches!(
        CommitGraph::from_objects_controlled(&objects, &[absent], limits, &AtomicBool::new(false)),
        Err(GraphWriteError::History(_))
    ));
    assert!(!repository.object_dir().join("info/commit-graph").exists());
}

#[path = "support/maintenance_git.rs"]
mod maintenance_git;

#[test]
fn nonempty_controlled_graph_matches_existing_and_git_in_both_formats() {
    use girt::ReadLimits;
    use girt::commit_graph::GraphWriteMode;
    for (format, name) in [
        (ObjectFormat::Sha1, "sha1"),
        (ObjectFormat::Sha256, "sha256"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        maintenance_git::git(
            root,
            &[
                "init",
                "--bare",
                "--template=",
                "--ref-format=files",
                "--object-format",
                name,
                "-q",
            ],
            b"",
        );
        let tree = String::from_utf8(maintenance_git::git(root, &["mktree"], b"")).unwrap();
        let first = String::from_utf8(maintenance_git::git(
            root,
            &["commit-tree", tree.trim()],
            b"first\n",
        ))
        .unwrap();
        let second = String::from_utf8(maintenance_git::git(
            root,
            &["commit-tree", tree.trim(), "-p", first.trim()],
            b"second\n",
        ))
        .unwrap();
        maintenance_git::git(root, &["update-ref", "refs/heads/main", second.trim()], b"");
        let tip = girt::ObjectId::from_hex(format, second.trim()).unwrap();
        let repository = Repository::open(root).unwrap();
        let objects = repository.objects().unwrap();
        let cancel = AtomicBool::new(false);
        let old =
            CommitGraph::from_objects_with(&objects, &[tip], HistoryLimits::default()).unwrap();
        let controlled = CommitGraph::from_objects_controlled(
            &objects,
            &[tip],
            HistoryLimits::default(),
            &cancel,
        )
        .unwrap();
        assert_eq!(old.encode().unwrap(), controlled.encode().unwrap());
        let limits =
            HistoryLimits::default().with_read(ReadLimits::default().with_max_object_bytes(1));
        assert!(matches!(
            CommitGraph::from_objects_controlled(&objects, &[tip], limits, &cancel),
            Err(GraphWriteError::Read { .. })
        ));
        controlled
            .install(repository.object_dir(), GraphWriteMode::Full)
            .unwrap();
        maintenance_git::git(root, &["commit-graph", "verify"], b"");
        maintenance_git::git(root, &["fsck", "--strict"], b"");
    }
}
