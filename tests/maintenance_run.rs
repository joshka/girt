//! Explicit task execution against isolated, independently generated Git repositories.
use std::sync::atomic::AtomicBool;

use girt::maintenance::{MaintenanceRun, MaintenanceTask, TaskOutcome, TaskSelection};
use girt::retention::MaintenanceIsolation;
use girt::{ObjectFormat, ObjectId, Repository};

#[path = "support/maintenance_git.rs"]
mod oracle;

#[derive(Default)]
struct Isolation(usize);

impl MaintenanceIsolation for Isolation {
    type Guard = ();

    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        self.0 += 1;
        // This disposable fixture has no independent writers or alternate dependents.
        Ok(())
    }
}

fn fixture(format: ObjectFormat) -> (tempfile::TempDir, Repository, ObjectId, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    oracle::git(
        root.path(),
        &[
            "init",
            "--bare",
            "--template=",
            "--initial-branch=main",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
    let tree = oracle::git(root.path(), &["mktree"], b"");
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let first = oracle::git(root.path(), &["commit-tree", tree], b"first\n");
    let first = std::str::from_utf8(&first).unwrap().trim();
    let second = oracle::git(
        root.path(),
        &["commit-tree", tree, "-p", first],
        b"second\n",
    );
    let second = std::str::from_utf8(&second).unwrap().trim();
    oracle::git(root.path(), &["update-ref", "refs/heads/main", first], b"");
    oracle::git(
        root.path(),
        &["update-ref", "--no-deref", "HEAD", second],
        b"",
    );
    let ids = (
        ObjectId::from_hex(format, first).unwrap(),
        ObjectId::from_hex(format, second).unwrap(),
    );
    let repo = Repository::open(root.path()).unwrap();
    (root, repo, ids.0, ids.1)
}

#[test]
fn unsupported_tasks_refuse_before_isolation_or_lock() {
    let (root, repo, _, _) = fixture(ObjectFormat::Sha1);
    for task in [
        MaintenanceTask::Gc,
        MaintenanceTask::Prefetch,
        MaintenanceTask::LooseObjects,
        MaintenanceTask::IncrementalRepack,
    ] {
        let selected = TaskSelection::explicit(&[MaintenanceTask::PackRefs, task]).unwrap();
        let mut isolation = Isolation::default();
        assert!(
            MaintenanceRun::new(&repo, selected)
                .run(&mut isolation, &AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(isolation.0, 0);
        assert!(!root.path().join("packed-refs").exists());
        assert!(!repo.object_dir().join("maintenance.lock").exists());
    }
}

#[test]
fn graph_roots_include_jj_keep_refs_but_not_detached_head() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (root, repo, first, detached) = fixture(format);
        let selection = || TaskSelection::explicit(&[MaintenanceTask::CommitGraph]).unwrap();
        MaintenanceRun::new(&repo, selection())
            .run(&mut Isolation::default(), &AtomicBool::new(false))
            .unwrap();
        let graph = repo.commit_graph().unwrap().unwrap();
        assert!(graph.get(first).is_some());
        assert!(graph.get(detached).is_none());
        oracle::git(
            root.path(),
            &["update-ref", "refs/jj/keep/test", &detached.to_string()],
            b"",
        );
        MaintenanceRun::new(&repo, selection())
            .run(&mut Isolation::default(), &AtomicBool::new(false))
            .unwrap();
        assert!(
            repo.commit_graph()
                .unwrap()
                .unwrap()
                .get(detached)
                .is_some()
        );
        oracle::git(root.path(), &["commit-graph", "verify"], b"");
        oracle::git(root.path(), &["fsck", "--strict"], b"");
    }
}

#[test]
fn selected_tasks_share_one_isolation_and_ignore_enabled_flags() {
    let (root, _, _, _) = fixture(ObjectFormat::Sha1);
    oracle::git(
        root.path(),
        &["config", "maintenance.pack-refs.enabled", "false"],
        b"",
    );
    oracle::git(
        root.path(),
        &["config", "maintenance.commit-graph.enabled", "false"],
        b"",
    );
    oracle::git(root.path(), &["config", "maintenance.auto", "false"], b"");
    let repo = Repository::open(root.path()).unwrap();
    let selected =
        TaskSelection::explicit(&[MaintenanceTask::CommitGraph, MaintenanceTask::PackRefs])
            .unwrap();
    let mut isolation = Isolation::default();
    let report = MaintenanceRun::new(&repo, selected)
        .run(&mut isolation, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(isolation.0, 1);
    assert_eq!(report.tasks.len(), 2);
    assert!(matches!(report.tasks[0].outcome, TaskOutcome::PackRefs(_)));
    assert!(matches!(
        report.tasks[1].outcome,
        TaskOutcome::CommitGraph(_)
    ));
    assert!(root.path().join("packed-refs").exists());
    assert!(!repo.object_dir().join("maintenance.lock").exists());
    oracle::git(root.path(), &["fsck", "--strict"], b"");
}

#[test]
fn cancellation_and_busy_lock_preserve_foreign_files() {
    let (root, repo, _, _) = fixture(ObjectFormat::Sha1);
    let selected = || TaskSelection::explicit(&[MaintenanceTask::PackRefs]).unwrap();
    let mut isolation = Isolation::default();
    assert!(
        MaintenanceRun::new(&repo, selected())
            .run(&mut isolation, &AtomicBool::new(true))
            .is_err()
    );
    assert_eq!(isolation.0, 0);
    let lock = repo.try_lock_maintenance().unwrap();
    let before = std::fs::read(repo.object_dir().join("maintenance.lock")).unwrap();
    assert!(
        MaintenanceRun::new(&repo, selected())
            .run(&mut isolation, &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(
        std::fs::read(repo.object_dir().join("maintenance.lock")).unwrap(),
        before
    );
    assert!(!root.path().join("packed-refs").exists());
    lock.release().unwrap();
}

#[test]
fn graph_configuration_and_shallow_state_skip_without_publication() {
    for shallow in [false, true] {
        let (root, _, first, _) = fixture(ObjectFormat::Sha1);
        if shallow {
            std::fs::write(root.path().join("shallow"), format!("{first}\n")).unwrap();
        } else {
            oracle::git(root.path(), &["config", "core.commitGraph", "false"], b"");
        }
        let repo = Repository::open(root.path()).unwrap();
        let selected = TaskSelection::explicit(&[MaintenanceTask::CommitGraph]).unwrap();
        let report = MaintenanceRun::new(&repo, selected)
            .run(&mut Isolation::default(), &AtomicBool::new(false))
            .unwrap();
        assert!(matches!(report.tasks[0].outcome, TaskOutcome::Skipped(_)));
        assert!(!repo.object_dir().join("info/commit-graph").exists());
        assert!(
            !repo
                .object_dir()
                .join("info/commit-graphs/commit-graph-chain")
                .exists()
        );
    }
}

#[test]
fn automatic_graph_threshold_is_inclusive_and_ignores_detached_head() {
    for threshold in [0, 1, 2, -1] {
        let (root, _, _, _) = fixture(ObjectFormat::Sha1);
        oracle::git(
            root.path(),
            &[
                "config",
                "maintenance.commit-graph.auto",
                &threshold.to_string(),
            ],
            b"",
        );
        let repo = Repository::open(root.path()).unwrap();
        let selected = TaskSelection::explicit(&[MaintenanceTask::CommitGraph]).unwrap();
        let report = MaintenanceRun::new(&repo, selected)
            .with_auto(true)
            .run(&mut Isolation::default(), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(
            matches!(report.tasks[0].outcome, TaskOutcome::CommitGraph(_)),
            threshold == 1 || threshold == -1
        );
    }
}

#[test]
fn completed_task_report_survives_a_later_failure() {
    let (root, _, _, _) = fixture(ObjectFormat::Sha1);
    oracle::git(
        root.path(),
        &["config", "maintenance.commit-graph.auto", "invalid"],
        b"",
    );
    let repo = Repository::open(root.path()).unwrap();
    let selected =
        TaskSelection::explicit(&[MaintenanceTask::PackRefs, MaintenanceTask::CommitGraph])
            .unwrap();
    let failure = MaintenanceRun::new(&repo, selected)
        .with_auto(true)
        .run(&mut Isolation::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(failure.report.tasks.len(), 2);
    assert!(!matches!(
        failure.report.tasks[0].outcome,
        TaskOutcome::NotStarted
    ));
    assert!(matches!(
        failure.report.tasks[1].outcome,
        TaskOutcome::NotStarted
    ));
    assert!(!repo.object_dir().join("maintenance.lock").exists());
}
