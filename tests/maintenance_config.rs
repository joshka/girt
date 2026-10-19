//! Lazy configuration policy, independent of task execution.
use girt::Config;
use girt::maintenance::{
    AutoThreshold, MaintenanceConfig, MaintenanceStrategy, MaintenanceTask as Task,
};

#[test]
fn defaults_do_not_choose_a_task_set() {
    let config = Config::parse(b"").unwrap();
    let policy = MaintenanceConfig::new(&config);
    assert!(policy.command_auto().unwrap());
    assert_eq!(policy.strategy().unwrap(), None);
    for task in [
        Task::Gc,
        Task::CommitGraph,
        Task::Prefetch,
        Task::LooseObjects,
        Task::IncrementalRepack,
        Task::PackRefs,
    ] {
        assert_eq!(policy.task_enabled(task).unwrap(), None);
    }
    assert_eq!(
        policy.auto_threshold(Task::CommitGraph).unwrap(),
        Some(AutoThreshold::Count(100))
    );
    assert_eq!(
        policy.auto_threshold(Task::LooseObjects).unwrap(),
        Some(AutoThreshold::Count(100))
    );
    assert_eq!(
        policy.auto_threshold(Task::IncrementalRepack).unwrap(),
        Some(AutoThreshold::Count(10))
    );
    for task in [Task::Gc, Task::Prefetch, Task::PackRefs] {
        assert_eq!(policy.auto_threshold(task).unwrap(), None);
    }
}

#[test]
fn zero_negative_and_positive_thresholds_are_distinct() {
    for (value, expected) in [
        ("0", AutoThreshold::Disabled),
        ("-2", AutoThreshold::Always),
        ("-1", AutoThreshold::Always),
        ("2k", AutoThreshold::Count(2048)),
    ] {
        let text = format!("[maintenance \"loose-objects\"]\nauto = {value}\n");
        let config = Config::parse(text.as_bytes()).unwrap();
        assert_eq!(
            MaintenanceConfig::new(&config)
                .auto_threshold(Task::LooseObjects)
                .unwrap(),
            Some(expected)
        );
    }
}

#[test]
fn settings_are_consulted_lazily() {
    let config = Config::parse(b"[maintenance]\nauto = bogus\nstrategy = bogus\n[maintenance \"pack-refs\"]\nenabled = bogus\nauto = bogus\n[maintenance \"loose-objects\"]\nauto = 1\n").unwrap();
    let policy = MaintenanceConfig::new(&config);
    assert_eq!(
        policy.auto_threshold(Task::LooseObjects).unwrap(),
        Some(AutoThreshold::Count(1))
    );
    assert_eq!(policy.auto_threshold(Task::PackRefs).unwrap(), None);
    assert!(policy.command_auto().is_err());
    assert!(policy.strategy().is_err());
    assert!(policy.task_enabled(Task::PackRefs).is_err());
}

#[test]
fn explicit_flags_and_incremental_strategy_are_reported_without_selection() {
    let config = Config::parse(b"[maintenance]\nauto = false\nstrategy = incremental\n[maintenance \"gc\"]\nenabled = false\n[maintenance \"commit-graph\"]\nenabled\n").unwrap();
    let policy = MaintenanceConfig::new(&config);
    assert!(!policy.command_auto().unwrap());
    assert_eq!(
        policy.strategy().unwrap(),
        Some(MaintenanceStrategy::Incremental)
    );
    assert_eq!(policy.task_enabled(Task::Gc).unwrap(), Some(false));
    assert_eq!(policy.task_enabled(Task::CommitGraph).unwrap(), Some(true));
}

#[test]
fn malformed_or_out_of_range_thresholds_are_refused() {
    for value in ["bogus", "2147483648", "-2147483649", ""] {
        let text = format!("[maintenance \"incremental-repack\"]\nauto = {value}\n");
        let config = Config::parse(text.as_bytes()).unwrap();
        assert!(
            MaintenanceConfig::new(&config)
                .auto_threshold(Task::IncrementalRepack)
                .is_err()
        );
    }
}
