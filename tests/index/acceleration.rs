//! Original fixtures generated through an isolated Git executable.
#[path = "../support/layout_git.rs"]
mod oracle;

use girt::index::Limits;
use girt::{InitKind, ObjectFormat, Repository};
use rstest::rstest;

#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn reads_git_untracked_cache_and_retains_its_bytes(#[case] format: ObjectFormat) {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, temporary.path().join("r"), InitKind::Worktree).unwrap();
    let root = repo.worktree().unwrap();
    std::fs::write(root.join("tracked"), b"tracked\n").unwrap();
    std::fs::create_dir(root.join("nested")).unwrap();
    std::fs::write(root.join("nested/new"), b"new\n").unwrap();
    std::fs::write(root.join(".gitignore"), b"*.ignored\n").unwrap();
    oracle::git(root, &["add", "tracked"], b"");
    oracle::git(root, &["update-index", "--untracked-cache"], b"");
    oracle::git(root, &["status", "--porcelain"], b"");
    oracle::git(root, &["status", "--porcelain"], b"");

    let index = repo.read_index(Limits::default()).unwrap().unwrap();
    let cache = index.untracked_cache(Limits::default()).unwrap().unwrap();
    assert!(cache.ident().ends_with(b"\0"));
    assert_eq!(cache.exclude_name(), b".gitignore");
    assert_eq!(cache.directories()[0].name(), b"");
    assert!(cache.directories()[0].valid());
    assert!(cache.directories()[0].stat().is_some());
    assert_eq!(
        cache.directories()[0].exclude(),
        // Git hashes its ignore buffer with an additional newline, even when the file ends in one.
        Some(girt::ObjectId::for_blob(format, b"*.ignored\n\n"))
    );
    assert!(
        cache
            .directories()
            .iter()
            .any(|dir| dir.name() == b"nested")
    );
    let extension = index
        .extensions()
        .iter()
        .find(|extension| extension.signature() == *b"UNTR")
        .unwrap();
    assert_eq!(cache.encode(Limits::default()).unwrap(), extension.data());
    for end in 0..extension.data().len() {
        assert!(
            girt::index::UntrackedCache::parse(format, &extension.data()[..end], Limits::default())
                .is_err(),
            "accepted truncated length {end}"
        );
    }
}

#[cfg(unix)]
#[path = "../support/stand_in.rs"]
mod stand_in;

#[cfg(unix)]
#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn reads_git_hook_monitor_checkpoint(#[case] format: ObjectFormat) {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, temporary.path().join("r"), InitKind::Worktree).unwrap();
    let root = repo.worktree().unwrap();
    std::fs::write(root.join("tracked"), b"tracked\n").unwrap();
    oracle::git(root, &["add", "tracked"], b"");
    let hook = temporary.path().join("monitor");
    stand_in::shell(&hook, "printf 'checkpoint\\000tracked\\000'\n");
    oracle::git(
        root,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
        b"",
    );
    oracle::git(root, &["config", "core.fsmonitorHookVersion", "2"], b"");
    oracle::git(root, &["status", "--porcelain"], b"");

    let index = repo.read_index(Limits::default()).unwrap().unwrap();
    let cache = index.fsmonitor_cache(Limits::default()).unwrap().unwrap();
    assert_eq!(
        cache.token(),
        &girt::index::MonitorToken::Opaque(b"checkpoint".to_vec())
    );
    assert!(cache.dirty().is_empty());
    oracle::git(root, &["update-index", "--split-index"], b"");
    oracle::git(
        root,
        &["update-index", "--no-fsmonitor-valid", "tracked"],
        b"",
    );
    let resolved = repo.read_index(Limits::default()).unwrap().unwrap();
    assert_eq!(
        resolved
            .fsmonitor_cache(Limits::default())
            .unwrap()
            .unwrap()
            .dirty(),
        [0]
    );
    let extension = index
        .extensions()
        .iter()
        .find(|extension| extension.signature() == *b"FSMN")
        .unwrap();
    assert_eq!(cache.encode(Limits::default()).unwrap(), extension.data());
}

#[cfg(unix)]
#[rstest]
#[case(1)]
#[case(2)]
fn explicit_monitor_queries_match_git_hook_protocol(#[case] version: u8) {
    use std::sync::atomic::AtomicBool;

    use girt::fsmonitor::{Commands, Error};
    use girt::index::MonitorToken;
    use girt::workflow::Environment;

    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        temporary.path().join("r"),
        InitKind::Worktree,
    )
    .unwrap();
    let root = repo.worktree().unwrap();
    let hook = temporary.path().join("monitor");
    let observed = temporary.path().join("arguments");
    stand_in::shell(
        &hook,
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\nif [ \"$1\" = 2 ]; then printf 'next\\000file\\000dir/\\000'; else printf 'file\\000dir/\\000'; fi\n",
            observed.display()
        ),
    );
    oracle::git(
        root,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
        b"",
    );
    oracle::git(
        root,
        &["config", "core.fsmonitorHookVersion", &version.to_string()],
        b"",
    );
    let repo = Repository::open(root).unwrap();
    let cancel = AtomicBool::new(false);
    let limits = girt::fsmonitor::Limits::default();
    let environment = Environment::default();
    let refused = repo
        .fsmonitor(&environment, Commands::Refuse)
        .unwrap()
        .unwrap();
    assert!(matches!(
        refused.query_with(None, limits, &cancel),
        Err(Error::Refused)
    ));
    assert!(!observed.exists());
    let monitor = repo
        .fsmonitor(&environment, Commands::Run)
        .unwrap()
        .unwrap();
    let old = if version == 1 {
        MonitorToken::Timestamp(123)
    } else {
        MonitorToken::Opaque(b"old".to_vec())
    };
    let changes = monitor.query_with(Some(&old), limits, &cancel).unwrap();
    assert!(!changes.full_scan());
    assert_eq!(changes.paths(), [b"file".to_vec(), b"dir/".to_vec()]);
    let expected = if version == 1 { "1\n123\n" } else { "2\nold\n" };
    assert_eq!(std::fs::read_to_string(&observed).unwrap(), expected);
    assert!(
        monitor
            .query_with(None, limits, &cancel)
            .unwrap()
            .full_scan()
    );
}

#[test]
fn daemon_configuration_is_a_typed_refusal_without_starting_it() {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        temporary.path().join("r"),
        InitKind::Worktree,
    )
    .unwrap();
    let root = repo.worktree().unwrap();
    oracle::git(root, &["config", "core.fsmonitor", "true"], b"");
    let repo = Repository::open(root).unwrap();
    assert!(matches!(
        repo.fsmonitor(
            &girt::workflow::Environment::default(),
            girt::fsmonitor::Commands::Run
        ),
        Err(girt::fsmonitor::Error::Unsupported(_))
    ));
}

#[cfg(unix)]
#[test]
fn monitor_negotiates_and_bounds_untrusted_hook_output() {
    use std::sync::atomic::AtomicBool;

    use girt::fsmonitor::{Commands, Error};
    use girt::index::MonitorToken;
    use girt::workflow::Environment;

    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        temporary.path().join("r"),
        InitKind::Worktree,
    )
    .unwrap();
    let root = repo.worktree().unwrap();
    let hook = temporary.path().join("monitor");
    stand_in::shell(
        &hook,
        "if [ \"$1\" = 2 ]; then exit 1; fi\nprintf 'file\\000'\n",
    );
    oracle::git(
        root,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
        b"",
    );
    let repo = Repository::open(root).unwrap();
    let monitor = repo
        .fsmonitor(&Environment::default(), Commands::Run)
        .unwrap()
        .unwrap();
    let cancel = AtomicBool::new(false);
    let changes = monitor.query(Some(&MonitorToken::Timestamp(123))).unwrap();
    assert!(matches!(changes.token(), MonitorToken::Timestamp(_)));
    assert_eq!(changes.paths(), [b"file".to_vec()]);
    assert!(!changes.full_scan());
    let incompatible = monitor.query(Some(&MonitorToken::Opaque(vec![]))).unwrap();
    assert!(incompatible.full_scan());
    assert!(matches!(
        monitor.query_with(
            None,
            girt::fsmonitor::Limits::default().with_max_bytes(2),
            &cancel
        ),
        Err(Error::Limit(_))
    ));
    assert!(matches!(
        monitor.query_with(None, Default::default(), &AtomicBool::new(true)),
        Err(Error::Cancelled)
    ));
}
