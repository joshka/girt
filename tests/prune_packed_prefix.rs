//! Opt-in verified prefixes drain duplicate backlogs without weakening survivor proofs.
use std::sync::atomic::AtomicBool;

use girt::retention::{
    MaintenanceIsolation, PrunePackedCause, PrunePackedLimits, PrunePackedSelection,
};
use girt::{InitKind, ObjectFormat, ObjectId, Repository};

#[path = "support/maintenance_git.rs"]
mod oracle;

struct Exclusive;
impl MaintenanceIsolation for Exclusive {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        // This disposable repository has no other consumers or writers.
        Ok(())
    }
}

fn fixture(format: ObjectFormat) -> (tempfile::TempDir, Repository, Vec<ObjectId>) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("r"), InitKind::Bare).unwrap();
    let mut ids = Vec::new();
    for number in 0..7 {
        ids.push(
            repo.loose_objects()
                .write_blob(format!("payload-{number:04}").as_bytes())
                .unwrap(),
        );
    }
    ids.sort_unstable();
    let input = ids.iter().map(|id| format!("{id}\n")).collect::<String>();
    oracle::git(
        repo.git_dir(),
        &["pack-objects", "objects/pack/pack"],
        input.as_bytes(),
    );
    (root, repo, ids)
}

#[test]
fn object_and_byte_limited_prefixes_drain_in_deterministic_passes() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for byte_limited in [false, true] {
            let (_root, repo, ids) = fixture(format);
            let mut remaining = ids.clone();
            let mut limits =
                PrunePackedLimits::default().with_selection(PrunePackedSelection::Prefix {
                    max_objects: if byte_limited { 3 } else { 2 },
                });
            if byte_limited {
                limits.max_bytes = 24;
            }
            for _ in 0..4 {
                let report = repo
                    .prune_packed(&mut Exclusive, limits, &AtomicBool::new(false))
                    .unwrap();
                let expected = remaining.len().min(2);
                assert_eq!(report.deleted, remaining[..expected]);
                assert_eq!(report.verified_bytes, expected as u64 * 12);
                assert_eq!(report.more_candidates, remaining.len() > expected);
                remaining.drain(..expected);
            }
            assert!(remaining.is_empty());
            let final_report = repo
                .prune_packed(&mut Exclusive, limits, &AtomicBool::new(false))
                .unwrap();
            assert!(final_report.deleted.is_empty());
            assert!(!final_report.more_candidates);
            for id in ids {
                assert!(repo.objects().unwrap().read(id).unwrap().is_some());
            }
            oracle::git(repo.git_dir(), &["fsck", "--strict"], b"");
        }
    }
}

#[test]
fn default_selection_preserves_all_candidate_byte_refusal() {
    let (_root, repo, ids) = fixture(ObjectFormat::Sha1);
    let mut limits = PrunePackedLimits::default();
    assert_eq!(limits.selection, PrunePackedSelection::All);
    limits.max_bytes = 24;
    let failure = repo
        .prune_packed(&mut Exclusive, limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(failure.cause, PrunePackedCause::ByteLimit));
    assert!(failure.report.deleted.is_empty());
    for id in ids {
        assert!(repo.loose_objects().read_blob(id, 1024).is_ok());
    }
}

#[test]
fn an_individually_unfit_survivor_errors_without_any_deletion() {
    let (_root, repo, ids) = fixture(ObjectFormat::Sha1);
    let mut limits = PrunePackedLimits::default()
        .with_selection(PrunePackedSelection::Prefix { max_objects: 2 });
    limits.max_bytes = 1;
    let failure = repo
        .prune_packed(&mut Exclusive, limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(failure.cause, PrunePackedCause::ByteLimit));
    assert!(failure.report.deleted.is_empty());
    for id in ids {
        assert!(repo.loose_objects().read_blob(id, 1024).is_ok());
    }
}

#[test]
fn zero_prefix_refuses_a_duplicate_without_deletion() {
    let (_root, repo, _) = fixture(ObjectFormat::Sha1);
    let limits = PrunePackedLimits::default()
        .with_selection(PrunePackedSelection::Prefix { max_objects: 0 });
    let failure = repo
        .prune_packed(&mut Exclusive, limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(failure.cause, PrunePackedCause::ObjectLimit));
    assert!(failure.report.deleted.is_empty());
}
