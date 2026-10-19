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

#[test]
fn numeric_rules_apply_to_all_three_count_tasks() {
    for task in [
        Task::CommitGraph,
        Task::LooseObjects,
        Task::IncrementalRepack,
    ] {
        for (value, expected) in [
            ("-2147483648", AutoThreshold::Always),
            ("2147483647", AutoThreshold::Count(2147483647)),
            ("1", AutoThreshold::Count(1)),
        ] {
            let text = format!("[maintenance \"{task}\"]\nauto = {value}\n");
            let config = Config::parse(text.as_bytes()).unwrap();
            assert_eq!(
                MaintenanceConfig::new(&config)
                    .auto_threshold(task)
                    .unwrap(),
                Some(expected)
            );
        }
    }
}

#[test]
fn built_in_checks_ignore_unrelated_numeric_auto_settings() {
    for task in [Task::Gc, Task::Prefetch, Task::PackRefs] {
        let text = format!("[maintenance \"{task}\"]\nauto = bogus\n");
        let config = Config::parse(text.as_bytes()).unwrap();
        assert_eq!(
            MaintenanceConfig::new(&config)
                .auto_threshold(task)
                .unwrap(),
            None
        );
    }
}

#[test]
fn task_errors_preserve_subsection_context_and_scalar_source() {
    use std::error::Error;
    let config =
        Config::parse(b"[maintenance \"commit-graph\"]\nauto = bogus\nenabled = bogus\n").unwrap();
    let policy = MaintenanceConfig::new(&config);
    for error in [
        policy.auto_threshold(Task::CommitGraph).unwrap_err(),
        policy.task_enabled(Task::CommitGraph).unwrap_err(),
    ] {
        assert_eq!(error.task, Task::CommitGraph);
        assert!(error.to_string().contains("commit-graph"));
        assert!(error.source().unwrap().is::<girt::config::InvalidValue>());
    }
}

#[test]
fn command_auto_uses_legacy_gc_fallback_only_when_unset() {
    for (value, expected) in [("0", false), ("-1", false), ("1", true), ("6700", true)] {
        let text = format!("[gc]\nauto = {value}\n");
        let config = Config::parse(text.as_bytes()).unwrap();
        assert_eq!(
            MaintenanceConfig::new(&config).command_auto().unwrap(),
            expected
        );
    }
    for value in ["bogus", "2147483648", "-2147483649"] {
        let text = format!("[gc]\nauto = {value}\n");
        let config = Config::parse(text.as_bytes()).unwrap();
        assert!(MaintenanceConfig::new(&config).command_auto().is_err());
        for enabled in [true, false] {
            let text = format!("[gc]\nauto = {value}\n[maintenance]\nauto = {enabled}\n");
            let config = Config::parse(text.as_bytes()).unwrap();
            assert_eq!(
                MaintenanceConfig::new(&config).command_auto().unwrap(),
                enabled
            );
        }
    }
}

#[test]
fn command_trigger_policy_matches_git_after_commit() {
    use std::path::Path;
    use std::process::Command;
    fn git(root: &Path, args: &[String]) {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let output = command
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
            .env("GIT_TRACE2_EVENT", root.join("trace.jsonl"))
            .env("GIT_AUTHOR_NAME", "Maintenance Fixture")
            .env("GIT_AUTHOR_EMAIL", "maintenance@example.com")
            .env("GIT_COMMITTER_NAME", "Maintenance Fixture")
            .env("GIT_COMMITTER_EMAIL", "maintenance@example.com")
            .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for (gc, automatic) in [
        (None, None),
        (Some("0"), None),
        (Some("-1"), None),
        (Some("1"), None),
        (Some("0"), Some("true")),
        (Some("1"), Some("false")),
        (Some("bogus"), Some("true")),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(
            root,
            &["init", "--template=", "--ref-format=files", "-q"].map(str::to_owned),
        );
        let mut args = vec![
            "-c".to_owned(),
            "gc.autoDetach=false".to_owned(),
            "-c".to_owned(),
            "maintenance.autoDetach=false".to_owned(),
        ];
        let mut text = String::new();
        if let Some(gc) = gc {
            args.extend(["-c".to_owned(), format!("gc.auto={gc}")]);
            text.push_str(&format!("[gc]\nauto = {gc}\n"));
        }
        if let Some(automatic) = automatic {
            args.extend(["-c".to_owned(), format!("maintenance.auto={automatic}")]);
            text.push_str(&format!("[maintenance]\nauto = {automatic}\n"));
        }
        args.extend(["commit", "--allow-empty", "-qm", "fixture"].map(str::to_owned));
        git(root, &args);
        let trace = std::fs::read_to_string(root.join("trace.jsonl")).unwrap();
        let observed = trace
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .any(|event| {
                event["event"] == "child_start"
                    && event["argv"].as_array().is_some_and(|args| {
                        args.iter().any(|arg| arg == "maintenance")
                            && args.iter().any(|arg| arg == "run")
                    })
            });
        let config = Config::parse(text.as_bytes()).unwrap();
        assert_eq!(
            MaintenanceConfig::new(&config).command_auto().unwrap(),
            observed,
            "{text}"
        );
    }
}
