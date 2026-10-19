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
    let limits = HistoryLimits {
        max_commits: 0,
        ..HistoryLimits::default()
    };
    assert!(matches!(
        CommitGraph::from_objects_controlled(&objects, &[absent], limits, &AtomicBool::new(false)),
        Err(GraphWriteError::History(_))
    ));
    assert!(!repository.object_dir().join("info/commit-graph").exists());
}
