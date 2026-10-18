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
