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
    let objects = repository.objects_controlled(Default::default(), Default::default(), &cancel).unwrap();
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
    assert_eq!(controlled.stored_kind(id).unwrap(), trusted.stored_kind(id).unwrap());
    assert_eq!(trusted.get(id).unwrap().unwrap().data(), b"payload");
}

#[test]
fn every_fallback_attempt_is_charged() {
    let (_root, known, id) = fixture(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let limits = FetchLimits::default().with_max_known_objects(1);
    let reads = KnownRead::<true>::new(&known, limits, &cancel);
    assert!(reads.kind(id).unwrap().is_some());
    assert!(matches!(reads.kind(id), Err(FetchError::Limit("known read attempts"))));
}

#[test]
fn payload_bytes_are_aggregate_and_inclusive() {
    let (_root, known, id) = fixture(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let limits = FetchLimits::default().with_max_known_bytes(7);
    let reads = KnownRead::<true>::new(&known, limits, &cancel);
    assert_eq!(reads.get(id).unwrap().unwrap().data(), b"payload");
    assert!(matches!(reads.get(id), Err(FetchError::Limit("known read bytes"))));
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
    let limits = FetchLimits::default().with_max_known_objects(0).with_max_known_bytes(0);
    let reads = KnownRead::<false>::new(&known, limits, &cancel);
    assert_eq!(reads.kind(id).unwrap(), Some(ObjectKind::Blob));
    assert_eq!(reads.get(id).unwrap().unwrap().data(), b"payload");
}
