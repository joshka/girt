//! Additive loose packing preserves loose copies until an explicit later prune.
use std::sync::atomic::AtomicBool;

use girt::pack::{LoosePackLimits, LoosePackPublication};
use girt::retention::{MaintenanceIsolation, PrunePackedLimits};
use girt::{InitKind, ObjectFormat, ObjectId, Repository};

#[path = "support/maintenance_git.rs"]
mod oracle;

#[derive(Default)]
struct Isolation(usize);
impl MaintenanceIsolation for Isolation {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        self.0 += 1;
        // This disposable fixture has no independent readers, writers or dependents.
        Ok(())
    }
}

fn fixture(format: ObjectFormat) -> (tempfile::TempDir, Repository, Vec<ObjectId>) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("r"), InitKind::Bare).unwrap();
    let ids = [b"unreachable one".as_slice(), b"unreachable two".as_slice()]
        .iter()
        .map(|bytes| repo.loose_objects().write_blob(bytes).unwrap())
        .collect();
    (root, repo, ids)
}

#[test]
fn newly_packed_loose_copies_survive_until_a_later_run() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (_root, repo, ids) = fixture(format);
        let mut isolation = Isolation::default();
        let report = repo
            .pack_loose(
                &mut isolation,
                LoosePackLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(isolation.0, 1);
        assert_eq!(report.publication, LoosePackPublication::Pair);
        let mut packed = report.packed.clone();
        packed.sort_unstable();
        let mut expected = ids.clone();
        expected.sort_unstable();
        assert_eq!(packed, expected);
        for id in &ids {
            assert!(
                !repo
                    .loose_objects()
                    .read_blob(*id, 1024)
                    .unwrap()
                    .is_empty()
            );
        }
        let counts = oracle::git(repo.git_dir(), &["count-objects", "-v"], b"");
        let counts = String::from_utf8(counts).unwrap();
        assert!(counts.lines().any(|line| line == "count: 2"));
        assert!(counts.lines().any(|line| line == "in-pack: 2"));
        assert!(counts.lines().any(|line| line == "prune-packable: 2"));
        let second = repo
            .pack_loose(
                &mut isolation,
                LoosePackLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(second.packed.is_empty());
        assert_eq!(second.publication, LoosePackPublication::None);
        let pruned = repo
            .prune_packed(
                &mut isolation,
                PrunePackedLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(pruned.deleted.len(), 2);
        for id in ids {
            assert!(repo.objects().unwrap().read(id).unwrap().is_some());
        }
        oracle::git(repo.git_dir(), &["fsck", "--strict"], b"");
    }
}

#[test]
fn cancellation_precedes_isolation_and_publication() {
    let (_root, repo, _) = fixture(ObjectFormat::Sha1);
    let mut isolation = Isolation::default();
    assert!(
        repo.pack_loose(
            &mut isolation,
            LoosePackLimits::default(),
            &AtomicBool::new(true)
        )
        .is_err()
    );
    assert_eq!(isolation.0, 0);
    assert_eq!(
        std::fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn inventory_exhaustion_preserves_all_loose_objects() {
    let (_root, repo, ids) = fixture(ObjectFormat::Sha1);
    let limits = LoosePackLimits::default().with_max_entries(0);
    assert!(
        repo.pack_loose(&mut Isolation::default(), limits, &AtomicBool::new(false))
            .is_err()
    );
    for id in ids {
        assert!(repo.loose_objects().read_blob(id, 1024).is_ok());
    }
    assert_eq!(
        std::fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn payload_budget_refuses_before_publication() {
    let (_root, repo, ids) = fixture(ObjectFormat::Sha1);
    let write = girt::PackWriteLimits::default().with_max_input_bytes(1);
    let limits = LoosePackLimits::default().with_write(write);
    assert!(
        repo.pack_loose(&mut Isolation::default(), limits, &AtomicBool::new(false))
            .is_err()
    );
    for id in ids {
        assert!(repo.loose_objects().read_blob(id, 1024).is_ok());
    }
    assert_eq!(
        std::fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn corrupt_loose_identity_never_becomes_an_indexed_pack() {
    let (_root, repo, ids) = fixture(ObjectFormat::Sha1);
    let id = ids[0].to_string();
    let source = repo.object_dir().join(&id[..2]).join(&id[2..]);
    let false_id = ObjectId::for_blob(ObjectFormat::Sha1, b"different").to_string();
    let destination = repo.object_dir().join(&false_id[..2]).join(&false_id[2..]);
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::copy(source, destination).unwrap();
    assert!(
        repo.pack_loose(
            &mut Isolation::default(),
            LoosePackLimits::default(),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn non_regular_loose_candidate_refuses_before_publication() {
    let (_root, repo, _) = fixture(ObjectFormat::Sha1);
    let id = ObjectId::for_blob(ObjectFormat::Sha1, b"directory").to_string();
    std::fs::create_dir_all(repo.object_dir().join(&id[..2]).join(&id[2..])).unwrap();
    assert!(
        repo.pack_loose(
            &mut Isolation::default(),
            LoosePackLimits::default(),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn selected_object_cap_leaves_remaining_loose_objects_for_another_batch() {
    let (_root, repo, ids) = fixture(ObjectFormat::Sha1);
    let limits =
        LoosePackLimits::default().with_write(girt::PackWriteLimits::default().with_max_objects(1));
    let report = repo
        .pack_loose(&mut Isolation::default(), limits, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(report.packed.len(), 1);
    assert!(report.more_loose);
    for id in ids {
        assert!(repo.loose_objects().read_blob(id, 1024).is_ok());
    }
    oracle::git(repo.git_dir(), &["fsck", "--strict"], b"");
}
