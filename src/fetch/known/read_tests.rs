use std::sync::atomic::{AtomicBool, Ordering};

use super::super::{FetchError, FetchLimits};
use super::KnownHistory;
use super::controlled::KnownRead;
use crate::{InitKind, ObjectFormat, ObjectKind, Repository};

fn fixture(format: ObjectFormat) -> (tempfile::TempDir, KnownHistory, crate::ObjectId) {
    let root = tempfile::tempdir().unwrap();
    let repository = Repository::init(format, root.path().join("r.git"), InitKind::Bare).unwrap();
    let id = repository.loose_objects().write_blob(b"payload").unwrap();
    let cancel = AtomicBool::new(false);
    let objects = repository
        .objects_controlled(Default::default(), Default::default(), &cancel)
        .unwrap();
    let known = KnownHistory::from_store_roots(objects, &[], &cancel).unwrap();
    (root, known, id)
}

#[rstest::rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn controlled_payload_and_kind_match_trusted_reads(#[case] format: ObjectFormat) {
    let (_root, known, id) = fixture(format);
    let cancel = AtomicBool::new(false);
    let controlled = KnownRead::<true>::new(&known, FetchLimits::default(), &cancel);
    let trusted = KnownRead::<false>::new(&known, FetchLimits::default(), &cancel);
    assert_eq!(controlled.get(id).unwrap().unwrap().data(), b"payload");
    assert_eq!(controlled.kind(id).unwrap(), Some(ObjectKind::Blob));
    assert_eq!(
        controlled.stored_kind(id).unwrap(),
        trusted.stored_kind(id).unwrap()
    );
    assert_eq!(trusted.get(id).unwrap().unwrap().data(), b"payload");
}

#[test]
fn every_fallback_attempt_is_charged() {
    let (_root, known, id) = fixture(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let limits = FetchLimits::default().with_max_known_objects(1);
    let reads = KnownRead::<true>::new(&known, limits, &cancel);
    assert!(reads.kind(id).unwrap().is_some());
    assert!(matches!(
        reads.kind(id),
        Err(FetchError::Limit("known read attempts"))
    ));
}

#[test]
fn payload_bytes_are_aggregate_and_inclusive() {
    let (_root, known, id) = fixture(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let limits = FetchLimits::default().with_max_known_bytes(7);
    let reads = KnownRead::<true>::new(&known, limits, &cancel);
    assert_eq!(reads.get(id).unwrap().unwrap().data(), b"payload");
    assert!(matches!(
        reads.get(id),
        Err(FetchError::Limit("known read bytes"))
    ));
}

#[test]
fn caller_cancellation_is_observed_after_context_creation() {
    let (_root, known, id) = fixture(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let reads = KnownRead::<true>::new(&known, FetchLimits::default(), &cancel);
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(reads.kind(id), Err(FetchError::Cancelled)));
    assert!(matches!(reads.stored_kind(id), Err(FetchError::Cancelled)));
    assert!(matches!(reads.get(id), Err(FetchError::Cancelled)));
}

#[test]
fn legacy_specialization_keeps_trusted_limits_and_cancellation_behavior() {
    let (_root, known, id) = fixture(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(true);
    let limits = FetchLimits::default()
        .with_max_known_objects(0)
        .with_max_known_bytes(0);
    let reads = KnownRead::<false>::new(&known, limits, &cancel);
    assert_eq!(reads.kind(id).unwrap(), Some(ObjectKind::Blob));
    assert_eq!(reads.get(id).unwrap().unwrap().data(), b"payload");
}

#[test]
fn root_admission_precedes_reads() {
    let (_root, known, id) = fixture(ObjectFormat::Sha1);
    let store = known.store.as_ref().unwrap().as_ref().clone();
    let cancel = AtomicBool::new(false);
    let result = KnownHistory::from_store_roots_controlled(
        store,
        &[id],
        FetchLimits::default().with_max_wants(0),
        crate::PeelLimits::default(),
        &cancel,
    );
    assert!(matches!(result, Err(FetchError::Limit("known roots"))));
}

#[test]
fn root_reads_share_the_aggregate_byte_budget() {
    let (_root, known, id) = fixture(ObjectFormat::Sha1);
    let store = known.store.as_ref().unwrap().as_ref().clone();
    let cancel = AtomicBool::new(false);
    let result = KnownHistory::from_store_roots_controlled(
        store,
        &[id, id],
        FetchLimits::default().with_max_known_bytes(7),
        crate::PeelLimits::default(),
        &cancel,
    );
    assert!(matches!(result, Err(FetchError::Limit("known read bytes"))));
}

#[rstest::rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn annotated_roots_establish_only_peeled_commits(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repository = Repository::init(format, root.path().join("r.git"), InitKind::Bare).unwrap();
    let loose = repository.loose_objects();
    let tree = loose
        .write_tree(&crate::Tree::new(format, vec![]).unwrap())
        .unwrap();
    let payload = format!(
        "tree {tree}\nauthor Fixture <fixture@example.com> 1700000000 +0000\ncommitter Fixture <fixture@example.com> 1700000000 +0000\n\nroot\n"
    );
    let commit = loose
        .write_commit(&crate::Commit::parse(format, payload.as_bytes()).unwrap())
        .unwrap();
    let tag = loose
        .write_tag(
            &crate::Tag::new(crate::TagFields {
                target: commit,
                target_kind: ObjectKind::Commit,
                name: b"v1".to_vec(),
                tagger: None,
                extra_headers: vec![],
                message: b"release".to_vec(),
            })
            .unwrap(),
        )
        .unwrap();
    let cancel = AtomicBool::new(false);
    let store = repository
        .objects_controlled(Default::default(), Default::default(), &cancel)
        .unwrap();
    let limited = KnownHistory::from_store_roots_controlled(
        store.clone(),
        &[tag],
        FetchLimits::default().with_max_known_edges(0),
        crate::PeelLimits::default(),
        &cancel,
    );
    assert!(matches!(
        limited,
        Err(FetchError::Limit("known root tag edges"))
    ));
    let known = KnownHistory::from_store_roots_controlled(
        store,
        &[tag, tree],
        FetchLimits::default(),
        crate::PeelLimits::default(),
        &cancel,
    )
    .unwrap();
    assert!(known.complete_roots.contains(&commit));
    assert!(!known.complete_roots.contains(&tag));
    assert!(!known.complete_roots.contains(&tree));
}

#[test]
fn missing_fallbacks_and_per_read_limits_are_enforced() {
    let (_root, known, id) = fixture(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let missing = crate::ObjectId::for_blob(ObjectFormat::Sha1, b"absent");
    let reads = KnownRead::<true>::new(
        &known,
        FetchLimits::default().with_max_known_objects(1),
        &cancel,
    );
    assert!(reads.get(missing).unwrap().is_none());
    assert!(matches!(
        reads.get(id),
        Err(FetchError::Limit("known read attempts"))
    ));
    let reads = KnownRead::<true>::new(
        &known,
        FetchLimits::default()
            .with_known_read(crate::ReadLimits::default().with_max_object_bytes(1)),
        &cancel,
    );
    assert!(matches!(
        reads.get(id),
        Err(FetchError::Limit("object bytes"))
    ));
}

#[test]
fn compressed_input_limit_reaches_loose_fallbacks() {
    let (_root, known, id) = fixture(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let limits = FetchLimits::default()
        .with_known_read(crate::ReadLimits::default().with_max_input_bytes(1));
    let reads = KnownRead::<true>::new(&known, limits, &cancel);
    assert!(matches!(
        reads.get(id),
        Err(FetchError::Limit("loose compressed bytes"))
    ));
}

#[rstest::rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn packed_fallbacks_share_the_aggregate_payload_budget(#[case] format: ObjectFormat) {
    let fixture = crate::pack_git::Fixture::new(format, true, 4);
    let payload = &fixture
        .records
        .iter()
        .find(|(id, _, _)| *id == fixture.ordinary)
        .unwrap()
        .2;
    let cancel = AtomicBool::new(false);
    let store = fixture.repo.objects_with(Default::default()).unwrap();
    let known = KnownHistory::from_store_roots(store, &[], &cancel).unwrap();
    let reads = KnownRead::<true>::new(
        &known,
        FetchLimits::default().with_max_known_bytes(payload.len()),
        &cancel,
    );
    assert_eq!(
        reads.get(fixture.ordinary).unwrap().unwrap().data(),
        payload
    );
    assert!(matches!(
        reads.get(fixture.ordinary),
        Err(FetchError::Limit("known read bytes"))
    ));
}

#[test]
fn unreadable_root_refuses_partial_completeness_evidence() {
    let (_root, known, _) = fixture(ObjectFormat::Sha1);
    let store = known.store.as_ref().unwrap().as_ref().clone();
    let cancel = AtomicBool::new(false);
    let missing = crate::ObjectId::for_blob(ObjectFormat::Sha1, b"not stored");
    let result = KnownHistory::from_store_roots_controlled(
        store,
        &[missing],
        FetchLimits::default(),
        crate::PeelLimits::default(),
        &cancel,
    );
    assert!(matches!(result, Err(FetchError::Peel(_))));
}
