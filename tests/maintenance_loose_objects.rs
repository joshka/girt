//! Loose-object maintenance composes verified deletion and additive packing under one guard.
use std::sync::atomic::AtomicBool;

use girt::maintenance::{
    MaintenanceLimits, MaintenanceRun, MaintenanceTask, TaskOutcome, TaskSelection, TaskSkip,
};
use girt::pack::{LoosePackLimits, LoosePackPublication};
use girt::retention::{MaintenanceIsolation, PrunePackedLimits, PrunePackedSelection};
use girt::{InitKind, ObjectFormat, ObjectId, PackWriteLimits, Repository};

#[path = "support/maintenance_git.rs"]
mod oracle;

#[derive(Default)]
struct Isolation(usize);
impl MaintenanceIsolation for Isolation {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        self.0 += 1;
        // This fixture has no other writers, readers or alternate dependents.
        Ok(())
    }
}

fn fixture(format: ObjectFormat) -> (tempfile::TempDir, Repository, Vec<ObjectId>, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("r.git"), InitKind::Bare).unwrap();
    let old: Vec<_> = [b"packed one".as_slice(), b"packed two".as_slice()]
        .into_iter()
        .map(|bytes| repo.loose_objects().write_blob(bytes).unwrap())
        .collect();
    repo.pack_loose(
        &mut Isolation::default(),
        LoosePackLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let new = repo.loose_objects().write_blob(b"new object").unwrap();
    (root, repo, old, new)
}

fn selection() -> TaskSelection {
    TaskSelection::explicit(&[MaintenanceTask::LooseObjects]).unwrap()
}

fn count(repo: &Repository, key: &str) -> usize {
    let bytes = oracle::git(repo.git_dir(), &["count-objects", "-v"], b"");
    let text = std::str::from_utf8(&bytes).unwrap();
    text.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}: ")))
        .unwrap()
        .parse()
        .unwrap()
}

fn assert_readable(repo: &Repository, ids: &[ObjectId]) {
    let objects = repo.objects().unwrap();
    for &id in ids {
        assert!(objects.read(id).unwrap().is_some());
    }
    oracle::git(repo.git_dir(), &["fsck", "--strict", "--no-dangling"], b"");
}

#[test]
fn two_runs_match_git_and_keep_newly_packed_loose_copies() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (_root, repo, old, new) = fixture(format);
        let (_mirror_root, mirror, _, _) = fixture(format);
        for pass in 1..=2 {
            let mut isolation = Isolation::default();
            let report = MaintenanceRun::new(&repo, selection())
                .run(&mut isolation, &AtomicBool::new(false))
                .unwrap();
            assert_eq!(isolation.0, 1);
            let TaskOutcome::LooseObjects(phases) = &report.tasks[0].outcome else {
                panic!("loose-objects task did not report its phases");
            };
            let prune = phases.prune.as_ref().unwrap();
            let pack = phases.pack.as_ref().unwrap();
            if pass == 1 {
                assert_eq!(prune.deleted.len(), 2);
                assert_eq!(pack.selected, [new]);
                assert_eq!(pack.publication, LoosePackPublication::Pair);
                assert!(repo.loose_objects().read_blob(new, 1024).is_ok());
            } else {
                assert_eq!(prune.deleted, [new]);
                assert!(pack.selected.is_empty());
                assert_eq!(pack.publication, LoosePackPublication::None);
            }
            oracle::git(
                mirror.git_dir(),
                &["maintenance", "run", "--task=loose-objects"],
                b"",
            );
            for key in ["count", "in-pack", "packs", "prune-packable", "garbage"] {
                assert_eq!(count(&repo, key), count(&mirror, key), "{key}, pass {pass}");
            }
            assert!(!repo.object_dir().join("maintenance.lock").exists());
            let mut ids = old.clone();
            ids.push(new);
            assert_readable(&repo, &ids);
            assert_readable(&mirror, &ids);
        }
    }
}

#[test]
fn automatic_threshold_counts_packed_duplicates_inclusively() {
    for threshold in ["0", "4", "3", "-2"] {
        let (_root, initial, old, new) = fixture(ObjectFormat::Sha1);
        oracle::git(
            initial.git_dir(),
            &["config", "maintenance.loose-objects.auto", threshold],
            b"",
        );
        oracle::git(
            initial.git_dir(),
            &["config", "maintenance.loose-objects.enabled", "false"],
            b"",
        );
        oracle::git(
            initial.git_dir(),
            &["config", "maintenance.auto", "false"],
            b"",
        );
        let repo = Repository::open(initial.git_dir()).unwrap();
        let report = MaintenanceRun::new(&repo, selection())
            .with_auto(true)
            .run(&mut Isolation::default(), &AtomicBool::new(false))
            .unwrap();
        if matches!(threshold, "0" | "4") {
            assert!(matches!(
                report.tasks[0].outcome,
                TaskOutcome::Skipped(TaskSkip::NotDue)
            ));
            assert_eq!(count(&repo, "count"), 3);
        } else {
            assert!(matches!(
                report.tasks[0].outcome,
                TaskOutcome::LooseObjects(_)
            ));
            assert_eq!(count(&repo, "count"), 1);
        }
        let mut ids = old;
        ids.push(new);
        assert_readable(&repo, &ids);
    }
}

#[test]
fn completed_pruning_survives_a_later_pack_refusal_in_the_report() {
    let (_root, repo, old, new) = fixture(ObjectFormat::Sha1);
    let limits = MaintenanceLimits::default().with_loose_pack(
        LoosePackLimits::default().with_write(PackWriteLimits::default().with_max_objects(0)),
    );
    let failure = MaintenanceRun::new(&repo, selection())
        .with_limits(limits)
        .run(&mut Isolation::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(failure.failed_task, Some(MaintenanceTask::LooseObjects));
    let TaskOutcome::LooseObjects(phases) = &failure.report.tasks[0].outcome else {
        panic!("completed phase was discarded");
    };
    assert_eq!(phases.prune.as_ref().unwrap().deleted.len(), 2);
    assert!(phases.pack.is_none()); // The source retains the failed pack attempt's partial report.
    assert_eq!(count(&repo, "count"), 1);
    assert!(!repo.object_dir().join("maintenance.lock").exists());
    let mut ids = old;
    ids.push(new);
    assert_readable(&repo, &ids);
}

#[test]
fn bounded_prune_prefixes_drain_without_deleting_new_copies_in_the_first_pass() {
    let (_root, repo, old, new) = fixture(ObjectFormat::Sha1);
    let limits = MaintenanceLimits::default().with_prune_packed(
        PrunePackedLimits::default()
            .with_selection(PrunePackedSelection::Prefix { max_objects: 1 }),
    );
    for pass in 1..=3 {
        let report = MaintenanceRun::new(&repo, selection())
            .with_limits(limits)
            .run(&mut Isolation::default(), &AtomicBool::new(false))
            .unwrap();
        let TaskOutcome::LooseObjects(phases) = &report.tasks[0].outcome else {
            panic!("missing phases");
        };
        assert_eq!(phases.prune.as_ref().unwrap().deleted.len(), 1);
        assert_eq!(count(&repo, "count"), 3 - pass);
        if pass == 1 {
            assert!(phases.prune.as_ref().unwrap().more_candidates);
            assert_eq!(phases.pack.as_ref().unwrap().selected, [new]);
            assert!(repo.loose_objects().read_blob(new, 1024).is_ok());
        }
    }
    let mut ids = old;
    ids.push(new);
    assert_readable(&repo, &ids);
}

#[test]
fn cancellation_foreign_lock_and_unsupported_selection_preserve_loose_files() {
    let (_root, repo, old, new) = fixture(ObjectFormat::Sha1);
    let mut isolation = Isolation::default();
    assert!(
        MaintenanceRun::new(&repo, selection())
            .run(&mut isolation, &AtomicBool::new(true))
            .is_err()
    );
    assert_eq!(isolation.0, 0);
    let lock = repo.try_lock_maintenance().unwrap();
    assert!(
        MaintenanceRun::new(&repo, selection())
            .run(&mut isolation, &AtomicBool::new(false))
            .is_err()
    );
    assert!(repo.object_dir().join("maintenance.lock").exists());
    lock.release().unwrap();
    let selected =
        TaskSelection::explicit(&[MaintenanceTask::LooseObjects, MaintenanceTask::Gc]).unwrap();
    let mut isolation = Isolation::default();
    assert!(
        MaintenanceRun::new(&repo, selected)
            .run(&mut isolation, &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(isolation.0, 0);
    assert_eq!(count(&repo, "count"), 3);
    let mut ids = old;
    ids.push(new);
    assert_readable(&repo, &ids);
}

#[test]
fn invalid_threshold_is_consulted_only_in_automatic_mode() {
    let (_root, initial, _, _) = fixture(ObjectFormat::Sha1);
    oracle::git(
        initial.git_dir(),
        &["config", "maintenance.loose-objects.auto", "invalid"],
        b"",
    );
    let repo = Repository::open(initial.git_dir()).unwrap();
    assert!(
        MaintenanceRun::new(&repo, selection())
            .with_auto(true)
            .run(&mut Isolation::default(), &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(count(&repo, "count"), 3);
    MaintenanceRun::new(&repo, selection())
        .run(&mut Isolation::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(count(&repo, "count"), 1);
}

#[test]
fn automatic_inventory_limits_refuse_before_pruning() {
    let (_root, repo, _, _) = fixture(ObjectFormat::Sha1);
    let limits = MaintenanceLimits::default()
        .with_loose_pack(LoosePackLimits::default().with_max_entries(0));
    assert!(
        MaintenanceRun::new(&repo, selection())
            .with_limits(limits)
            .with_auto(true)
            .run(&mut Isolation::default(), &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(count(&repo, "count"), 3);
    assert!(!repo.object_dir().join("maintenance.lock").exists());
}

#[test]
fn non_directory_fanout_has_a_typed_pre_prune_refusal() {
    let (_root, repo, _, _) = fixture(ObjectFormat::Sha1);
    let path = (0..256)
        .map(|prefix| repo.object_dir().join(format!("{prefix:02x}")))
        .find(|path| !path.exists())
        .unwrap();
    std::fs::write(&path, b"not a directory").unwrap();
    let failure = MaintenanceRun::new(&repo, selection())
        .with_auto(true)
        .run(&mut Isolation::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        failure.source,
        girt::maintenance::MaintenanceRunError::NonRegularLoose(ref found) if found == &path
    ));
    assert_eq!(std::fs::read(&path).unwrap(), b"not a directory");
    std::fs::remove_file(path).unwrap();
    assert_eq!(count(&repo, "count"), 3);
    assert!(!repo.object_dir().join("maintenance.lock").exists());
}
