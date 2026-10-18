//! Original Git-oracle fixtures for the status acceleration validity contract.
#[path = "../support/layout_git.rs"]
mod oracle;
#[path = "../support/stand_in.rs"]
mod stand_in;

use girt::status::{Baseline, UntrackedFiles};
use girt::workflow::Environment;
use girt::{InitKind, ObjectFormat, Repository};

#[test]
fn monitor_failure_or_reset_cannot_hide_a_working_file_change() {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        temporary.path().join("r"),
        InitKind::Worktree,
    )
    .unwrap();
    let root = repo.worktree().unwrap();
    std::fs::write(root.join("tracked"), b"old\n").unwrap();
    oracle::git(root, &["add", "tracked"], b"");
    let hook = temporary.path().join("monitor");
    stand_in::shell(&hook, "printf 'token\\000/\\000'\n");
    oracle::git(
        root,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
        b"",
    );
    oracle::git(root, &["config", "core.fsmonitorHookVersion", "2"], b"");
    oracle::git(root, &["status", "--porcelain"], b"");
    std::fs::write(root.join("tracked"), b"new\n").unwrap();
    let repo = Repository::open(root).unwrap();
    let environment = Environment::default();
    let rules = repo.ignore_rules(&environment).unwrap();
    let monitor = repo
        .fsmonitor(&environment, girt::fsmonitor::Commands::Run)
        .unwrap()
        .unwrap();

    let report = girt::status::Scan::new(&repo, Baseline::Head, UntrackedFiles::All, &rules)
        .with_monitor(&monitor)
        .run()
        .unwrap();
    assert_eq!(report.unstaged.len(), 1);
    assert_eq!(report.unstaged[0].path, b"tracked");
}

#[test]
fn stale_untracked_cache_cannot_hide_directory_or_ignore_edits() {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        temporary.path().join("r"),
        InitKind::Worktree,
    )
    .unwrap();
    let root = repo.worktree().unwrap();
    std::fs::write(root.join("tracked"), b"old\n").unwrap();
    std::fs::create_dir(root.join("new")).unwrap();
    std::fs::write(root.join("new/file"), b"new\n").unwrap();
    std::fs::write(root.join(".gitignore"), b"*.hidden\n").unwrap();
    oracle::git(root, &["add", "tracked"], b"");
    oracle::git(root, &["update-index", "--untracked-cache"], b"");
    oracle::git(root, &["status", "--porcelain"], b"");
    std::fs::write(root.join("visible.hidden"), b"new\n").unwrap();
    // Editing an existing ignore file need not change the parent directory's mtime.
    std::fs::write(root.join(".gitignore"), b"*.other\n").unwrap();
    let repo = Repository::open(root).unwrap();
    let rules = repo.ignore_rules(&Environment::default()).unwrap();

    let report = girt::status::Scan::new(&repo, Baseline::Head, UntrackedFiles::Collapsed, &rules)
        .run()
        .unwrap();
    let paths: Vec<_> = report
        .untracked
        .iter()
        .map(|entry| entry.path.as_slice())
        .collect();
    assert_eq!(paths, [b".gitignore".as_slice(), b"new", b"visible.hidden"]);
}

#[test]
fn monitor_clean_promises_skip_reads_but_dirty_index_bits_override_them() {
    use girt::fsmonitor::Commands;
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        temporary.path().join("r"),
        InitKind::Worktree,
    )
    .unwrap();
    let root = repo.worktree().unwrap();
    oracle::git(root, &["config", "core.ignoreCase", "false"], b"");
    oracle::git(root, &["config", "core.precomposeUnicode", "false"], b"");
    std::fs::write(root.join("tracked"), b"old\n").unwrap();
    oracle::git(root, &["add", "tracked"], b"");
    oracle::git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "initial"],
        b"",
    );
    let hook = temporary.path().join("monitor");
    stand_in::shell(&hook, "printf 'token\\000'\n");
    oracle::git(
        root,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
        b"",
    );
    oracle::git(root, &["config", "core.fsmonitorHookVersion", "2"], b"");
    oracle::git(root, &["status", "--porcelain"], b"");
    std::fs::write(root.join("tracked"), b"new\n").unwrap();
    // This deliberately incomplete monitor promises no changes. Git trusts that promise too.
    assert!(oracle::git(root, &["status", "--porcelain"], b"").is_empty());
    let repo = Repository::open(root).unwrap();
    let environment = Environment::default();
    let rules = repo.ignore_rules(&environment).unwrap();
    let monitor = repo
        .fsmonitor(&environment, Commands::Run)
        .unwrap()
        .unwrap();
    let report = girt::status::Scan::new(&repo, Baseline::Head, UntrackedFiles::Omit, &rules)
        .with_monitor(&monitor)
        .run()
        .unwrap();
    assert!(report.unstaged.is_empty());

    for key in ["core.ignoreCase", "core.precomposeUnicode"] {
        if key == "core.precomposeUnicode" && !cfg!(target_os = "macos") {
            continue;
        }
        oracle::git(root, &["config", key, "true"], b"");
        let aliased = Repository::open(root).unwrap();
        let rules = aliased.ignore_rules(&environment).unwrap();
        let monitor = aliased
            .fsmonitor(&environment, Commands::Run)
            .unwrap()
            .unwrap();
        let report =
            girt::status::Scan::new(&aliased, Baseline::Head, UntrackedFiles::Omit, &rules)
                .with_monitor(&monitor)
                .run()
                .unwrap();
        assert_eq!(report.unstaged.len(), 1, "{key}");
        oracle::git(root, &["config", key, "false"], b"");
    }

    oracle::git(
        root,
        &["update-index", "--no-fsmonitor-valid", "tracked"],
        b"",
    );
    let report = girt::status::Scan::new(&repo, Baseline::Head, UntrackedFiles::Omit, &rules)
        .with_monitor(&monitor)
        .run()
        .unwrap();
    assert_eq!(report.unstaged.len(), 1);
}

#[test]
fn valid_untracked_listings_are_reused_and_edits_invalidate_them() {
    use girt::status::{UntrackedCacheFallback, UntrackedCacheUse};
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        temporary.path().join("r"),
        InitKind::Worktree,
    )
    .unwrap();
    let root = repo.worktree().unwrap();
    std::fs::create_dir(root.join("dir")).unwrap();
    std::fs::write(root.join("dir/tracked"), b"old\n").unwrap();
    std::fs::write(root.join("dir/untracked"), b"new\n").unwrap();
    std::fs::write(root.join(".gitignore"), b"*.tmp\n").unwrap();
    for (key, value) in [
        ("core.ignoreCase", "false"),
        ("core.precomposeUnicode", "false"),
        ("core.excludesFile", "/girt-absent-excludes"),
    ] {
        oracle::git(root, &["config", key, value], b"");
    }
    oracle::git(root, &["add", "dir/tracked", ".gitignore"], b"");
    oracle::git(root, &["update-index", "--untracked-cache"], b"");
    oracle::git(root, &["status", "--porcelain"], b"");
    oracle::git(root, &["status", "--porcelain"], b"");
    // Advance only the index clock, avoiding sleeps while making directory stats unambiguously old.
    std::fs::File::options()
        .write(true)
        .open(repo.git_dir().join("index"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(10))
        .unwrap();
    let repo = Repository::open(root).unwrap();
    let rules = repo.ignore_rules(&Environment::default()).unwrap();
    let report = repo
        .status(Baseline::Head, UntrackedFiles::Collapsed, &rules)
        .unwrap();
    assert!(
        matches!(report.untracked_cache, UntrackedCacheUse::Reused { directories } if directories > 0),
        "{:?}",
        report.untracked_cache
    );
    assert!(
        report
            .untracked
            .iter()
            .any(|entry| entry.path == b"dir/untracked")
    );

    std::fs::write(root.join("dir/newer"), b"new\n").unwrap();
    let report = repo
        .status(Baseline::Head, UntrackedFiles::Collapsed, &rules)
        .unwrap();
    assert!(
        report
            .untracked
            .iter()
            .any(|entry| entry.path == b"dir/newer")
    );
    std::fs::write(root.join(".gitignore"), b"untracked\nnewer\n").unwrap();
    let rules = repo.ignore_rules(&Environment::default()).unwrap();
    let report = repo
        .status(Baseline::Head, UntrackedFiles::Collapsed, &rules)
        .unwrap();
    assert!(report.untracked.is_empty());

    oracle::git(root, &["config", "core.ignoreCase", "true"], b"");
    let repo = Repository::open(root).unwrap();
    let rules = repo.ignore_rules(&Environment::default()).unwrap();
    assert_eq!(
        repo.status(Baseline::Head, UntrackedFiles::Collapsed, &rules)
            .unwrap()
            .untracked_cache,
        UntrackedCacheUse::Fallback(UntrackedCacheFallback::NamePolicy)
    );
    oracle::git(root, &["config", "core.untrackedCache", "false"], b"");
    let repo = Repository::open(root).unwrap();
    assert_eq!(
        repo.status(Baseline::Head, UntrackedFiles::Collapsed, &rules)
            .unwrap()
            .untracked_cache,
        UntrackedCacheUse::Disabled
    );
}

#[test]
fn changed_directory_notifications_compose_with_filters_and_pathspecs() {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        temporary.path().join("r"),
        InitKind::Worktree,
    )
    .unwrap();
    let root = repo.worktree().unwrap();
    std::fs::create_dir(root.join("dir")).unwrap();
    std::fs::write(root.join("dir/tracked"), b"old\r\n").unwrap();
    std::fs::write(root.join("other"), b"old\n").unwrap();
    std::fs::write(root.join(".gitattributes"), b"dir/tracked text eol=lf\n").unwrap();
    oracle::git(root, &["add", "."], b"");
    oracle::git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "initial"],
        b"",
    );
    let hook = temporary.path().join("monitor");
    stand_in::shell(&hook, "printf 'token\\000dir/\\000'\n");
    oracle::git(
        root,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
        b"",
    );
    oracle::git(root, &["config", "core.fsmonitorHookVersion", "2"], b"");
    oracle::git(root, &["status", "--porcelain"], b"");
    std::fs::write(root.join("dir/tracked"), b"new\r\n").unwrap();
    let repo = Repository::open(root).unwrap();
    let environment = Environment::default();
    let rules = repo.ignore_rules(&environment).unwrap();
    let monitor = repo
        .fsmonitor(&environment, girt::fsmonitor::Commands::Run)
        .unwrap()
        .unwrap();
    let mut filters = repo
        .filters(&environment, girt::filter::Commands::Refuse)
        .unwrap();
    let paths = girt::pathspec::Pathspec::parse(["dir"], &girt::pathspec::Context::new()).unwrap();
    let report = girt::status::Scan::new(&repo, Baseline::Head, UntrackedFiles::Collapsed, &rules)
        .with_monitor(&monitor)
        .with_filters(&mut filters)
        .with_pathspec(&paths)
        .run()
        .unwrap();
    assert_eq!(
        report.comparison,
        girt::status::Comparison::ConvertedContent
    );
    assert_eq!(report.unstaged.len(), 1);
    assert_eq!(report.unstaged[0].path, b"dir/tracked");
    assert!(
        matches!(report.unstaged[0].change, girt::status::Change::Modified(value) if value.id == girt::ObjectId::for_blob(ObjectFormat::Sha1, b"new\n"))
    );
}
