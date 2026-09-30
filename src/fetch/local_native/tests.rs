use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rstest::rstest;

use super::*;
use crate::{EntryMode, InitKind, ObjectFormat, Tree, TreeEntry};

fn source(format: ObjectFormat) -> (tempfile::TempDir, Repository, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("source"), InitKind::Bare).unwrap();
    let loose = repo.loose_objects();
    let blob = loose.write_blob(b"shared contents").unwrap();
    let tree = Tree::new(
        format,
        vec![
            TreeEntry {
                mode: EntryMode::Blob,
                name: b"a".to_vec(),
                id: blob,
            },
            TreeEntry {
                mode: EntryMode::Blob,
                name: b"b".to_vec(),
                id: blob,
            },
        ],
    )
    .unwrap();
    let id = loose.write_tree(&tree).unwrap();
    std::fs::write(repo.git_dir().join("refs/heads/main"), format!("{id}\n")).unwrap();
    (root, repo, id)
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn local_progress_counts_unique_reads_and_pack_entries(#[case] format: ObjectFormat) {
    let (_root, repo, id) = source(format);
    let mut events = Vec::new();
    let received = receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    );
    assert!(received.is_ok());
    assert_eq!(
        events,
        [
            LocalFetchProgress::Reading { objects: 0 },
            LocalFetchProgress::Reading { objects: 1 },
            LocalFetchProgress::Reading { objects: 2 },
            LocalFetchProgress::Packing { objects: (0, 2) },
            LocalFetchProgress::Packing { objects: (1, 2) },
            LocalFetchProgress::Packing { objects: (2, 2) },
            LocalFetchProgress::Complete,
        ]
    );
}

#[test]
fn local_progress_no_wants_has_no_pack() {
    let (_root, repo, _id) = source(ObjectFormat::Sha1);
    let mut events = Vec::new();
    receive(
        &repo,
        |_| vec![],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    )
    .unwrap();
    assert_eq!(
        events,
        [
            LocalFetchProgress::Reading { objects: 0 },
            LocalFetchProgress::Complete
        ]
    );
}

#[test]
fn local_progress_known_objects_are_read_but_not_packed() {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let objects = repo.objects(Default::default()).unwrap();
    let known = KnownHistory::new_local(&objects, &[id], FetchLimits::default(), &cancel).unwrap();
    let mut events = Vec::new();
    receive(
        &repo,
        |_| vec![id],
        &known,
        FetchLimits::default(),
        TransportControl::new(&cancel),
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    )
    .unwrap();
    assert_eq!(
        events,
        [
            LocalFetchProgress::Reading { objects: 0 },
            LocalFetchProgress::Reading { objects: 1 },
            LocalFetchProgress::Reading { objects: 2 },
            LocalFetchProgress::Complete
        ]
    );
}

#[rstest]
#[case::before_reads(LocalFetchProgress::Reading { objects: 0 })]
#[case::during_reads(LocalFetchProgress::Reading { objects: 1 })]
#[case::before_pack(LocalFetchProgress::Packing { objects: (0, 2) })]
#[case::during_pack(LocalFetchProgress::Packing { objects: (1, 2) })]
#[case::after_entries(LocalFetchProgress::Packing { objects: (2, 2) })]
fn local_progress_callback_can_cancel_before_completion(#[case] stop: LocalFetchProgress) {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let mut events = Vec::new();
    let result = receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&cancel),
        |_| ControlFlow::Continue(()),
        |event| {
            events.push(event);
            if event == stop {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    assert!(matches!(result, Err(FetchError::Cancelled)));
    assert!(!events.contains(&LocalFetchProgress::Complete));
    assert_eq!(events.last(), Some(&stop));
}

#[test]
fn local_progress_terminal_callback_cannot_undo_completion() {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&cancel),
        |_| ControlFlow::Continue(()),
        |event| {
            if event == LocalFetchProgress::Complete {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    )
    .unwrap();
    assert!(cancel.load(Ordering::Relaxed));
}

#[test]
fn local_progress_preserves_original_callback_cancellation() {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let mut events = Vec::new();
    let result = receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Break(()),
        |event| events.push(event),
    );
    assert!(matches!(result, Err(FetchError::Cancelled)));
    assert!(!events.contains(&LocalFetchProgress::Complete));
}

#[test]
fn local_progress_expired_deadline_has_no_notifications() {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let mut events = Vec::new();
    let result = receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl {
            cancel: &AtomicBool::new(false),
            deadline: Some(Instant::now() - Duration::from_secs(1)),
        },
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    );
    assert!(matches!(result, Err(FetchError::Deadline)));
    assert!(events.is_empty());
}

#[test]
fn local_progress_pack_failure_has_no_completion() {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let mut events = Vec::new();
    let result = receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits {
            max_pack_bytes: 12,
            ..Default::default()
        },
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    );
    assert!(matches!(result, Err(FetchError::PackWrite(_))));
    assert!(!events.contains(&LocalFetchProgress::Complete));
}

#[test]
fn local_progress_missing_source_has_no_completion() {
    let (_root, repo, _id) = source(ObjectFormat::Sha1);
    let missing = ObjectId::for_blob(ObjectFormat::Sha1, b"missing");
    std::fs::write(
        repo.git_dir().join("refs/heads/main"),
        format!("{missing}\n"),
    )
    .unwrap();
    let mut events = Vec::new();
    let result = receive(
        &repo,
        |_| vec![missing],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    );
    assert!(matches!(result, Err(FetchError::Missing(_))));
    assert!(!events.contains(&LocalFetchProgress::Complete));
}
