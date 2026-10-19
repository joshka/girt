//! Explicit maintenance selection is pure policy and never runs a task.
use girt::maintenance::{MaintenanceTask as Task, TaskSelection, TaskSelectionError};

#[test]
fn implicit_defaults_are_refused() {
    assert!(matches!(
        TaskSelection::explicit(&[]),
        Err(TaskSelectionError::DefaultTasks)
    ));
}

#[test]
fn duplicates_are_refused_instead_of_running_twice() {
    assert!(matches!(
        TaskSelection::explicit(&[Task::Gc, Task::Gc]),
        Err(TaskSelectionError::Duplicate(Task::Gc))
    ));
}

#[test]
fn foreground_refs_precede_requested_background_order() {
    let selection = TaskSelection::explicit(&[
        Task::LooseObjects,
        Task::CommitGraph,
        Task::PackRefs,
        Task::Prefetch,
    ])
    .unwrap();
    assert_eq!(
        selection.tasks(),
        &[
            Task::PackRefs,
            Task::LooseObjects,
            Task::CommitGraph,
            Task::Prefetch,
        ]
    );
}

#[test]
fn every_supported_name_round_trips() {
    for (name, task) in [
        ("gc", Task::Gc),
        ("commit-graph", Task::CommitGraph),
        ("prefetch", Task::Prefetch),
        ("loose-objects", Task::LooseObjects),
        ("incremental-repack", Task::IncrementalRepack),
        ("pack-refs", Task::PackRefs),
    ] {
        assert_eq!(name.parse::<Task>().unwrap(), task);
        assert_eq!(task.to_string(), name);
    }
}

#[test]
fn unsupported_names_are_not_silently_skipped() {
    for name in [
        "geometric-repack",
        "worktree-prune",
        "rerere-gc",
        "GC",
        "",
        "bogus",
    ] {
        assert!(matches!(
            name.parse::<Task>(),
            Err(TaskSelectionError::Unsupported(_))
        ));
    }
}

#[path = "support/maintenance_git.rs"]
mod maintenance_git;

#[test]
fn git_explicit_task_ignores_disabled_and_malformed_selection_config() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    maintenance_git::git(
        root,
        &["init", "--bare", "--template=", "--ref-format=files", "-q"],
        b"",
    );
    maintenance_git::git(
        root,
        &[
            "-c",
            "maintenance.pack-refs.enabled=false",
            "-c",
            "maintenance.auto=bogus",
            "-c",
            "maintenance.strategy=bogus",
            "maintenance",
            "run",
            "--task=pack-refs",
        ],
        b"",
    );
    assert!(root.join("packed-refs").is_file());
    let selected = TaskSelection::explicit(&[Task::PackRefs]).unwrap();
    assert_eq!(selected.tasks(), &[Task::PackRefs]);
}
