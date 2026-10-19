//! Explicit incremental maintenance preserves primitive proofs and completed phase reports.
use std::sync::atomic::AtomicBool;

use girt::maintenance::{IncrementalRepackLimits, MaintenanceRun, MaintenanceTask, TaskSelection};
#[cfg(unix)]
use girt::maintenance::{MaintenanceLimits, TaskOutcome, TaskSkip};
use girt::retention::MaintenanceIsolation;
use girt::{InitKind, ObjectFormat, Repository};

#[path = "support/maintenance_git.rs"]
mod oracle;

#[derive(Default)]
struct Isolation(usize);
impl MaintenanceIsolation for Isolation {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        self.0 += 1;
        Ok(()) // Only this test accesses its private repository.
    }
}

fn selection() -> TaskSelection {
    TaskSelection::explicit(&[MaintenanceTask::IncrementalRepack]).unwrap()
}

fn fixture(format: ObjectFormat) -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("r.git"), InitKind::Bare).unwrap();
    for size in [20, 40, 80] {
        let id = repo.loose_objects().write_blob(&vec![b'x'; size]).unwrap();
        oracle::git(
            repo.git_dir(),
            &["pack-objects", "--compression=0", "objects/pack/pack"],
            format!("{id}\n").as_bytes(),
        );
    }
    (root, repo)
}

#[test]
fn default_limits_admit_linux_sized_indexes() {
    let limits = IncrementalRepackLimits::default();
    assert!(limits.write.max_objects >= 20_000_000);
    assert!(limits.write.max_bytes >= 512 * 1024 * 1024);
    assert!(limits.expire.packs.max_index_bytes >= 512 * 1024 * 1024);
    assert!(limits.batch.batch.index.max_objects >= 10_000_000);
}

#[cfg(unix)]
#[test]
fn writes_expires_then_publishes_a_batch_under_one_guard() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (_root, repo) = fixture(format);
        let objects = repo.objects().unwrap();
        let mut sizes: Vec<_> = objects.packs().map(|pack| pack.pack_bytes()).collect();
        sizes.sort_unstable();
        let expected = sizes[1] + 1;
        let ids = objects.object_ids(Default::default()).unwrap();
        drop(objects);
        let mut isolation = Isolation::default();
        let report = MaintenanceRun::new(&repo, selection())
            .run(&mut isolation, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(isolation.0, 1);
        let TaskOutcome::IncrementalRepack(phases) = &report.tasks[0].outcome else {
            panic!("missing incremental phases");
        };
        assert_eq!(phases.index.as_ref().unwrap().packs, 3);
        assert!(phases.expire.is_some());
        assert_eq!(phases.batch_bytes, Some(expected));
        assert!(phases.repack.is_some());
        let objects = repo.objects().unwrap();
        for id in ids {
            assert!(objects.read(id).unwrap().is_some());
        }
        oracle::git(repo.git_dir(), &["multi-pack-index", "verify"], b"");
        oracle::git(repo.git_dir(), &["fsck", "--strict", "--no-dangling"], b"");
        assert!(!repo.object_dir().join("maintenance.lock").exists());
    }
}

#[cfg(unix)]
#[test]
fn batch_failure_keeps_completed_publication_and_expiry() {
    let (_root, repo) = fixture(ObjectFormat::Sha1);
    let mut incremental = IncrementalRepackLimits::default();
    incremental.batch.write.max_objects = 0;
    let failure = MaintenanceRun::new(&repo, selection())
        .with_limits(MaintenanceLimits::default().with_incremental_repack(incremental))
        .run(&mut Isolation::default(), &AtomicBool::new(false))
        .unwrap_err();
    let TaskOutcome::IncrementalRepack(phases) = &failure.report.tasks[0].outcome else {
        panic!("missing completed phases");
    };
    assert!(phases.index.is_some());
    assert!(phases.expire.is_some());
    assert!(phases.repack.is_none());
    assert!(repo.object_dir().join("pack/multi-pack-index").is_file());
    oracle::git(repo.git_dir(), &["fsck", "--strict", "--no-dangling"], b"");
}

#[cfg(unix)]
#[test]
fn auto_counts_only_packs_absent_from_midx() {
    let (_root, repo) = fixture(ObjectFormat::Sha1);
    oracle::git(
        repo.git_dir(),
        &["config", "maintenance.incremental-repack.auto", "4"],
        b"",
    );
    let repo = Repository::open(repo.git_dir()).unwrap();
    let report = MaintenanceRun::new(&repo, selection())
        .with_auto(true)
        .run(&mut Isolation::default(), &AtomicBool::new(false))
        .unwrap();
    assert!(matches!(
        report.tasks[0].outcome,
        TaskOutcome::Skipped(TaskSkip::NotDue)
    ));
    assert!(!repo.object_dir().join("pack/multi-pack-index").exists());
    oracle::git(
        repo.git_dir(),
        &["config", "maintenance.incremental-repack.auto", "3"],
        b"",
    );
    let repo = Repository::open(repo.git_dir()).unwrap();
    MaintenanceRun::new(&repo, selection())
        .with_auto(true)
        .run(&mut Isolation::default(), &AtomicBool::new(false))
        .unwrap();
    let report = MaintenanceRun::new(&repo, selection())
        .with_auto(true)
        .run(&mut Isolation::default(), &AtomicBool::new(false))
        .unwrap();
    assert!(matches!(
        report.tasks[0].outcome,
        TaskOutcome::Skipped(TaskSkip::NotDue)
    ));
}

#[cfg(windows)]
#[test]
fn unsupported_expiry_refuses_the_batch_before_isolation() {
    let (_root, repo) = fixture(ObjectFormat::Sha1);
    let mut isolation = Isolation::default();
    let failure = MaintenanceRun::new(&repo, selection())
        .run(&mut isolation, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        failure.source,
        girt::maintenance::MaintenanceRunError::Unsupported(MaintenanceTask::IncrementalRepack)
    ));
    assert_eq!(isolation.0, 0);
    assert!(!repo.object_dir().join("maintenance.lock").exists());
    assert!(!repo.object_dir().join("pack/multi-pack-index").exists());
}

#[cfg(unix)]
#[test]
fn expiry_target_uses_survivors_and_future_dated_packs_are_reported() {
    use std::fs::{self, File};
    use std::time::{Duration, UNIX_EPOCH};
    for future in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            ObjectFormat::Sha1,
            root.path().join("r.git"),
            InitKind::Bare,
        )
        .unwrap();
        let mut ids = Vec::new();
        // Uncompressed packs make the supersets strictly larger than the separate final pack.
        for size in [32, 1000, 80, 30] {
            ids.push(repo.loose_objects().write_blob(&vec![b'a'; size]).unwrap());
        }
        let mut paths = Vec::new();
        for (number, selected) in [&ids[..1], &ids[..2], &ids[..3], &ids[3..]]
            .into_iter()
            .enumerate()
        {
            let input: String = selected.iter().map(|id| format!("{id}\n")).collect();
            let output = oracle::git(
                repo.git_dir(),
                &["pack-objects", "--compression=0", "objects/pack/pack"],
                input.as_bytes(),
            );
            let path = repo.object_dir().join(format!(
                "pack/pack-{}.pack",
                std::str::from_utf8(&output).unwrap().trim()
            ));
            let base = if future { 4_000_000_000 } else { 1_700_000_000 };
            File::open(&path)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(base + number as u64))
                .unwrap();
            paths.push(path);
        }
        let expected = if future {
            fs::metadata(&paths[1]).unwrap().len() + 1
        } else {
            fs::metadata(&paths[3]).unwrap().len() + 1
        };
        let report = MaintenanceRun::new(&repo, selection())
            .run(&mut Isolation::default(), &AtomicBool::new(false))
            .unwrap();
        let TaskOutcome::IncrementalRepack(phases) = &report.tasks[0].outcome else {
            panic!("missing phases")
        };
        assert_eq!(phases.batch_bytes, Some(expected));
        let expired = phases.expire.as_ref().unwrap();
        for path in &paths[..2] {
            if future {
                assert!(expired.preserved.contains(path));
                assert!(path.exists());
            } else {
                assert!(expired.removed.contains(path));
                assert!(!path.exists());
            }
        }
        let objects = repo.objects().unwrap();
        for id in ids {
            assert!(objects.read(id).unwrap().is_some());
        }
        oracle::git(repo.git_dir(), &["multi-pack-index", "verify"], b"");
        oracle::git(repo.git_dir(), &["fsck", "--strict", "--no-dangling"], b"");
    }
}

#[cfg(unix)]
#[test]
fn disabled_and_forced_auto_checks_are_lazy_and_limits_preserve_storage() {
    let (_root, repo) = fixture(ObjectFormat::Sha1);
    for value in ["0", "-2", "invalid"] {
        oracle::git(
            repo.git_dir(),
            &["config", "maintenance.incremental-repack.auto", value],
            b"",
        );
        let repo = Repository::open(repo.git_dir()).unwrap();
        let result = MaintenanceRun::new(&repo, selection())
            .with_auto(true)
            .run(&mut Isolation::default(), &AtomicBool::new(false));
        match value {
            "0" => assert!(matches!(
                result.unwrap().tasks[0].outcome,
                TaskOutcome::Skipped(TaskSkip::NotDue)
            )),
            "-2" => assert!(matches!(
                result.unwrap().tasks[0].outcome,
                TaskOutcome::IncrementalRepack(_)
            )),
            _ => {
                assert!(result.is_err());
                MaintenanceRun::new(&repo, selection())
                    .run(&mut Isolation::default(), &AtomicBool::new(false))
                    .unwrap();
            }
        }
    }
    let before = std::fs::read(repo.object_dir().join("pack/multi-pack-index")).unwrap();
    let mut limits = IncrementalRepackLimits::default();
    limits.write.max_objects = 0;
    assert!(
        MaintenanceRun::new(&repo, selection())
            .with_limits(MaintenanceLimits::default().with_incremental_repack(limits))
            .run(&mut Isolation::default(), &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(
        before,
        std::fs::read(repo.object_dir().join("pack/multi-pack-index")).unwrap()
    );
    assert!(
        MaintenanceRun::new(&repo, selection())
            .run(&mut Isolation::default(), &AtomicBool::new(true))
            .is_err()
    );
    std::fs::write(repo.object_dir().join("maintenance.lock"), b"foreign").unwrap();
    assert!(
        MaintenanceRun::new(&repo, selection())
            .run(&mut Isolation::default(), &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(
        std::fs::read(repo.object_dir().join("maintenance.lock")).unwrap(),
        b"foreign"
    );
}
