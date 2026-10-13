//! Original Git-generated packs; Git verify-pack is the independent counter oracle.
#[path = "support/pack_git.rs"]
mod pack_git;

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

use girt::fetch::{FetchLimits, KnownHistory, ValidationProgress, receive_local_with_validation_progress};
use girt::push::{ForcePolicy, LocalPushContext, PreparedPush, PushCommand, PushLimits, send_local_with_progress};
use girt::refs::RefName;
use girt::transport::TransportControl;
use girt::{InitKind, ObjectFormat, PackLimits, Repository};
use pack_git::{Fixture, git};
use rstest::rstest;

fn check(events: &[ValidationProgress], objects: u64, deltas: u64) {
    assert!(deltas > 0);
    assert_eq!(events.last(), Some(&ValidationProgress {
        objects: (objects, objects), deltas: Some((deltas, deltas)), complete: true,
    }));
    assert_eq!(events.iter().filter(|event| event.complete).count(), 1);
    for pair in events.windows(2) {
        assert!(pair[0].objects.0 <= pair[1].objects.0);
        assert!(pair[0].deltas.map_or(0, |value| value.0) <= pair[1].deltas.map_or(0, |value| value.0));
    }
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn local_fetch_reports_git_verified_entries_and_deltas(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    let mut events = Vec::new();
    let received = receive_local_with_validation_progress(
        fixture.root.path(),
        |advertisement| advertisement.refs.iter().filter(|reference| !reference.peeled).map(|reference| reference.id).collect(),
        &KnownHistory::default(), FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)), |_| ControlFlow::Continue(()),
        |event| events.push(event),
    ).unwrap();
    let root = tempfile::tempdir().unwrap();
    let destination = Repository::init(format, root.path().join("destination"), InitKind::Bare).unwrap();
    let installed = received.install(&destination, PackLimits::default(), &AtomicBool::new(false)).unwrap();
    let index = destination.object_dir().join(format!("pack/pack-{}.idx", installed.checksum.unwrap()));
    let report = git(destination.git_dir(), &["verify-pack", "-v", index.to_str().unwrap()], b"");
    let report = std::str::from_utf8(&report).unwrap();
    let deltas = report.lines().filter(|line| line.split_whitespace().count() == 7).count();
    check(&events, installed.objects as u64, deltas as u64);
    assert_eq!(installed.objects, fixture.records.len());
    assert!(fixture.index_path.exists());
    assert_ne!(fixture.ordinary, fixture.delta);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn local_push_validation_cancellation_never_publishes(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    let id = std::str::from_utf8(&git(fixture.root.path(), &["rev-parse", "main"], b"")).unwrap().trim().parse().unwrap();
    let prepared = PreparedPush::new_local(
        &fixture.repo.objects(PackLimits::default()).unwrap(),
        vec![PushCommand { name: RefName::new("refs/heads/main").unwrap(), expected: None, new: id, force: ForcePolicy::FastForwardOnly }],
        &[], PushLimits::default(), &AtomicBool::new(false),
    ).unwrap();
    for delta in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let destination = Repository::init(format, root.path().join("destination"), InitKind::Bare).unwrap();
        let cancel = AtomicBool::new(false);
        let mut events = Vec::new();
        let inputs = girt::config::ConfigInputs::default();
        let result = send_local_with_progress(destination.git_dir(), &prepared, TransportControl::new(&cancel), LocalPushContext { config_inputs: &inputs, identity: None }, |event| {
            events.push(event);
            if if delta { event.deltas.is_some_and(|value| value.0 > 0) } else { event.objects.0 > 0 } {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert!(matches!(result, Err(girt::push::PushError::NotSent(_))));
        assert!(events.iter().all(|event| !event.complete));
        assert!(destination.references().unwrap().list().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(destination.object_dir().join("pack")).unwrap().count(), 0);
    }
}
