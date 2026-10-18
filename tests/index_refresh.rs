//! Refreshing index stat words, checked against independently generated Git repositories.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use girt::config::ConfigInputs;
use girt::index::StorageError;
use girt::pathspec::Pathspec;
use girt::status::{Baseline, Change, Error, Limits, UntrackedFiles};
use girt::workflow::Environment;
use girt::{ObjectId, Repository};
use rstest::rstest;

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(root)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Refresh Fixture")
        .env("GIT_AUTHOR_EMAIL", "refresh@example.invalid")
        .env("GIT_COMMITTER_NAME", "Refresh Fixture")
        .env("GIT_COMMITTER_EMAIL", "refresh@example.invalid")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Waits until the clock enters a later whole second, so that times already set fall before it.
fn next_second() {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    std::thread::sleep(Duration::from_nanos(
        1_000_000_000 - u64::from(now.subsec_nanos()) + 1_000_000,
    ));
}

/// A committed repository whose files `a`, `b` and `dir/c` hold their own names, with stat words
/// Git has refreshed in a later second than any file time, so every entry is trusted.
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        git(temp.path(), &["init", "-q", "repo"]);
        fs::create_dir(root.join("dir")).unwrap();
        for name in ["a", "b", "dir/c"] {
            fs::write(root.join(name), format!("{name}\n")).unwrap();
        }
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "initial"]);
        let fixture = Self { _temp: temp, root };
        for name in ["a", "b", "dir/c"] {
            fixture.touch(name, -30);
        }
        fixture.git_refresh();
        fixture
    }

    /// Git's own refresh, in a second after every file's ctime.
    fn git_refresh(&self) {
        next_second();
        git(&self.root, &["update-index", "-q", "--refresh"]);
    }

    fn repo(&self) -> Repository {
        Repository::open_with_config(&self.root, &ConfigInputs::default()).unwrap()
    }

    fn index(&self) -> PathBuf {
        self.root.join(".git/index")
    }

    /// Sets a file's modification time to `seconds` relative to now, changing its ctime to now.
    fn touch(&self, name: &str, seconds: i64) {
        let time = if seconds < 0 {
            SystemTime::now() - Duration::from_secs(seconds.unsigned_abs())
        } else {
            SystemTime::now() + Duration::from_secs(seconds as u64)
        };
        File::options()
            .write(true)
            .open(self.root.join(name))
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    /// Moves the index's own modification time into the past, so that every file touched since
    /// is at or after it.
    fn age_index(&self, seconds: u64) {
        File::options()
            .write(true)
            .open(self.index())
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(seconds))
            .unwrap();
    }

    fn stat(&self, name: &str) -> girt::index::Stat {
        let index = self.repo().read_index(Default::default()).unwrap().unwrap();
        index
            .entries()
            .iter()
            .find(|entry| entry.path == name.as_bytes())
            .unwrap()
            .stat
    }
}

/// Status that fails if it reads any working file, because no file bytes are allowed.
fn status_without_reading(repo: &Repository) -> Result<girt::status::Report, Error> {
    let environment = Environment::default().with("GIT_CONFIG_NOSYSTEM", "1");
    let rules = repo.ignore_rules(&environment).unwrap();
    let limits = Limits::default().with_max_worktree_bytes(0);
    repo.status_with(
        Baseline::Head,
        UntrackedFiles::Omit,
        &rules,
        &Pathspec::all(),
        limits,
        &AtomicBool::new(false),
    )
}

#[test]
fn records_touched_files_so_status_reads_nothing() {
    let fixture = Fixture::new();
    for name in ["a", "b", "dir/c"] {
        fixture.touch(name, -5);
    }
    fixture.age_index(10);
    let repo = fixture.repo();
    assert!(matches!(
        status_without_reading(&repo),
        Err(Error::Limit("worktree bytes"))
    ));

    // Status distrusts a ctime in the second its index was written.
    next_second();
    let refresh = repo.refresh_index().unwrap();
    assert_eq!((refresh.recorded, refresh.racy), (3, 0));
    assert!(refresh.written && refresh.changes.is_empty());
    // Touched after the previous index was written but before the refresh began: the refresh's
    // own rule keeps these, where the carried-over rule would have smudged every one.
    assert_eq!(fixture.stat("a").size, 2);
    let metadata = fs::metadata(fixture.root.join("a")).unwrap();
    use std::os::unix::fs::MetadataExt as _;
    assert_eq!(fixture.stat("a").mtime.seconds, metadata.mtime() as u32);
    assert_eq!(fixture.stat("a").inode, metadata.ino() as u32);

    let report = status_without_reading(&fixture.repo()).unwrap();
    assert!(report.unstaged.is_empty());
    assert!(git(&fixture.root, &["diff-files"]).is_empty());
    assert!(git(&fixture.root, &["status", "--porcelain"]).is_empty());
}

#[test]
fn modified_files_keep_their_stat_words() {
    let fixture = Fixture::new();
    let before = fixture.stat("a");
    fs::write(fixture.root.join("a"), "changed\n").unwrap();
    fixture.touch("a", -5);
    fixture.touch("b", -5);
    let refresh = fixture.repo().refresh_index().unwrap();
    assert_eq!(refresh.recorded, 1);
    assert_eq!(refresh.changes.len(), 1);
    assert_eq!(refresh.changes[0].path, b"a");
    assert!(matches!(refresh.changes[0].change, Change::Modified(_)));
    assert_eq!(fixture.stat("a"), before);
    assert_eq!(git(&fixture.root, &["diff-files", "--name-only"]), b"a\n");
}

#[test]
fn file_modified_in_the_refresh_second_is_smudged() {
    let fixture = Fixture::new();
    fixture.touch("a", -5);
    // A modification time at or after the second the refresh began.
    fixture.touch("b", 100);
    let refresh = fixture.repo().refresh_index().unwrap();
    assert_eq!((refresh.recorded, refresh.racy), (2, 1));
    assert_eq!(fixture.stat("a").size, 2);
    let racy = fixture.stat("b");
    assert_eq!(racy.size, 0);
    assert_eq!(
        racy.mtime.seconds,
        u32::try_from(
            fs::metadata(fixture.root.join("b"))
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        )
        .unwrap()
    );
    // Still reread, and still clean, by both implementations.
    assert!(matches!(
        status_without_reading(&fixture.repo()),
        Err(Error::Limit("worktree bytes"))
    ));
    assert!(git(&fixture.root, &["status", "--porcelain"]).is_empty());
}

#[test]
fn held_lock_is_reported_and_leaves_the_index() {
    let fixture = Fixture::new();
    fixture.touch("a", -5);
    let before = fs::read(fixture.index()).unwrap();
    let lock = fixture.root.join(".git/index.lock");
    fs::write(&lock, b"").unwrap();
    let error = fixture.repo().refresh_index().unwrap_err();
    assert!(
        matches!(&error, Error::Index(StorageError::Locked(path)) if path.ends_with(".git/index.lock")),
        "{error:?}"
    );
    assert_eq!(fs::read(fixture.index()).unwrap(), before);
    assert!(lock.exists());
}

#[test]
fn lock_is_held_during_the_scan_and_released_after() {
    let fixture = Fixture::new();
    fixture.touch("a", -5);
    let repo = fixture.repo();
    repo.refresh_index().unwrap();
    assert!(!fixture.root.join(".git/index.lock").exists());
    // Nothing left to record still releases the lock.
    assert!(!repo.refresh_index().unwrap().written);
    assert!(!fixture.root.join(".git/index.lock").exists());
}

#[test]
fn nothing_stale_writes_nothing() {
    // Git's own refresh leaves nothing for girt to record.
    let fixture = Fixture::new();
    let before = fs::read(fixture.index()).unwrap();
    let modified = fs::metadata(fixture.index()).unwrap().modified().unwrap();
    let refresh = fixture.repo().refresh_index().unwrap();
    assert_eq!(refresh.recorded, 0);
    assert!(!refresh.written);
    assert_eq!(fs::read(fixture.index()).unwrap(), before);
    assert_eq!(
        fs::metadata(fixture.index()).unwrap().modified().unwrap(),
        modified
    );
}

#[test]
fn missing_index_writes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    git(temp.path(), &["init", "-q", "repo"]);
    let root = temp.path().join("repo");
    let repo = Repository::open_with_config(&root, &ConfigInputs::default()).unwrap();
    assert_eq!(repo.refresh_index().unwrap().recorded, 0);
    assert!(!root.join(".git/index").exists());
    assert!(!root.join(".git/index.lock").exists());
}

#[rstest]
#[case::assume_unchanged("--assume-unchanged")]
#[case::skip_worktree("--skip-worktree")]
fn promised_entries_are_not_inspected(#[case] flag: &str) {
    let fixture = Fixture::new();
    git(&fixture.root, &["update-index", flag, "a"]);
    let before = fixture.stat("a");
    fixture.touch("a", -5);
    fixture.touch("b", -5);
    let refresh = fixture.repo().refresh_index().unwrap();
    assert_eq!(refresh.recorded, 1);
    assert_eq!(fixture.stat("a"), before);
}

#[test]
fn intent_to_add_entries_are_never_refreshed() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("new"), "").unwrap();
    git(&fixture.root, &["add", "-N", "new"]);
    let before = fixture.stat("new");
    fixture.touch("new", -5);
    let refresh = fixture.repo().refresh_index().unwrap();
    assert_eq!(refresh.recorded, 0);
    assert_eq!(refresh.changes.len(), 1);
    assert!(matches!(refresh.changes[0].change, Change::Added(_)));
    assert_eq!(fixture.stat("new"), before);
}

#[test]
fn file_mode_false_refreshes_mode_only_changes() {
    let fixture = Fixture::new();
    git(&fixture.root, &["config", "core.fileMode", "false"]);
    let path = fixture.root.join("a");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    fixture.touch("a", -5);
    let refresh = fixture.repo().refresh_index().unwrap();
    assert_eq!(refresh.recorded, 1);
    assert!(refresh.changes.is_empty());
    assert!(git(&fixture.root, &["diff-files"]).is_empty());
}

#[test]
fn executable_change_is_not_refreshed_by_default() {
    let fixture = Fixture::new();
    let path = fixture.root.join("a");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    fixture.touch("a", -5);
    let refresh = fixture.repo().refresh_index().unwrap();
    assert_eq!(refresh.recorded, 0);
    assert_eq!(refresh.changes.len(), 1);
}

#[test]
fn converted_content_is_refreshed_through_filters() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join(".gitattributes"), "crlf text eol=crlf\n").unwrap();
    fs::write(fixture.root.join("crlf"), "one\ntwo\n").unwrap();
    git(&fixture.root, &["add", "."]);
    git(&fixture.root, &["commit", "-qm", "attributes"]);
    // Git's checkout writes CRLF; the index holds LF.
    fs::remove_file(fixture.root.join("crlf")).unwrap();
    git(&fixture.root, &["checkout", "--", "crlf"]);
    assert_eq!(
        fs::read(fixture.root.join("crlf")).unwrap(),
        b"one\r\ntwo\r\n"
    );
    fixture.touch("crlf", -5);
    fixture.touch(".gitattributes", -5);

    let repo = fixture.repo();
    // Raw bytes differ from the stored blob, so nothing can be recorded for them.
    let before = fixture.stat("crlf");
    let raw = repo.refresh_index().unwrap();
    assert!(raw.changes.iter().any(|change| change.path == b"crlf"));
    // The words still describe the file Git checked out, not the touched one. Publishing the
    // `.gitattributes` refresh may smudge them (zero size) under the carried-over racy rule, when
    // Git wrote the file and its index within one timestamp tick, as on Linux.
    let after = fixture.stat("crlf");
    assert_eq!(
        girt::index::Stat { size: 0, ..after },
        girt::index::Stat { size: 0, ..before }
    );
    assert!(after.size == before.size || after.size == 0);
    let environment = Environment::default().with("GIT_CONFIG_NOSYSTEM", "1");
    let mut filters = repo
        .filters(&environment, girt::filter::Commands::Refuse)
        .unwrap();
    let refresh = repo
        .refresh_index_with(
            Some(&mut filters),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    // The raw refresh above already recorded `.gitattributes`.
    assert_eq!((refresh.recorded, refresh.racy), (1, 0));
    assert!(refresh.changes.is_empty());
    assert_eq!(fixture.stat("crlf").size, 10);
    assert_eq!(git(&fixture.root, &["diff-files"]), b"");
}

#[test]
fn resized_converted_file_is_left_for_git() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join(".gitattributes"), "* text=auto\n").unwrap();
    git(&fixture.root, &["add", "."]);
    git(&fixture.root, &["commit", "-qm", "attributes"]);
    // Same content after conversion, different size: Git reports it as needing an update.
    fs::write(fixture.root.join("a"), "a\r\n").unwrap();
    fixture.touch("a", -5);
    let repo = fixture.repo();
    let environment = Environment::default().with("GIT_CONFIG_NOSYSTEM", "1");
    let mut filters = repo
        .filters(&environment, girt::filter::Commands::Refuse)
        .unwrap();
    let before = fixture.stat("a");
    let refresh = repo
        .refresh_index_with(
            Some(&mut filters),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(refresh.changes.len(), 1);
    assert_eq!(fixture.stat("a"), before);
}

#[test]
fn split_index_is_refused() {
    let fixture = Fixture::new();
    git(&fixture.root, &["update-index", "--split-index"]);
    fixture.touch("a", -5);
    let before = fs::read(fixture.index()).unwrap();
    let error = fixture.repo().refresh_index().unwrap_err();
    assert!(matches!(error, Error::Unsupported { .. }), "{error:?}");
    assert_eq!(fs::read(fixture.index()).unwrap(), before);
    assert!(!fixture.root.join(".git/index.lock").exists());
}

#[test]
fn cancelled_refresh_writes_nothing() {
    let fixture = Fixture::new();
    fixture.touch("a", -5);
    let before = fs::read(fixture.index()).unwrap();
    let error = fixture
        .repo()
        .refresh_index_with(None, Limits::default(), &AtomicBool::new(true))
        .unwrap_err();
    assert!(matches!(error, Error::Cancelled));
    assert_eq!(fs::read(fixture.index()).unwrap(), before);
}

#[test]
fn walk_limits_apply() {
    let fixture = Fixture::new();
    fixture.touch("a", -5);
    let before = fs::read(fixture.index()).unwrap();
    let limits = Limits::default().with_max_worktree_bytes(1);
    let error = fixture
        .repo()
        .refresh_index_with(None, limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error, Error::Limit(_)), "{error:?}");
    assert_eq!(fs::read(fixture.index()).unwrap(), before);
    assert!(!fixture.root.join(".git/index.lock").exists());
}

#[rstest]
#[case::minimal_ignores_nanoseconds_and_inode("minimal", true)]
#[case::default_compares_every_word("default", false)]
fn check_stat_decides_which_words_are_trusted(#[case] value: &str, #[case] trusted: bool) {
    let fixture = Fixture::new();
    git(&fixture.root, &["config", "core.checkStat", value]);
    // Rewrite the cached inode only: Git's minimal mode does not compare it.
    let repo = fixture.repo();
    let mut edit = repo.edit_index(Default::default()).unwrap();
    let mut entries = edit.index().entries().to_vec();
    for entry in &mut entries {
        entry.stat.inode ^= 1;
    }
    edit.replace_entries(entries).unwrap();
    edit.commit().unwrap();
    assert_eq!(status_without_reading(&fixture.repo()).is_ok(), trusted);
}

#[test]
fn untrusted_ctime_is_not_compared() {
    let fixture = Fixture::new();
    // Changing permissions and back changes only the ctime.
    next_second();
    for name in ["a", "b", "dir/c"] {
        let path = fixture.root.join(name);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert!(status_without_reading(&fixture.repo()).is_err());
    git(&fixture.root, &["config", "core.trustCtime", "false"]);
    assert!(status_without_reading(&fixture.repo()).is_ok());
}

#[test]
fn invalid_check_stat_is_refused() {
    let fixture = Fixture::new();
    git(&fixture.root, &["config", "core.checkStat", "bogus"]);
    let error = fixture.repo().refresh_index().unwrap_err();
    assert!(matches!(error, Error::Unsupported { .. }), "{error:?}");
}

#[test]
fn records_the_identity_git_reports() {
    let fixture = Fixture::new();
    fixture.touch("dir/c", -5);
    fixture.repo().refresh_index().unwrap();
    let index = fixture
        .repo()
        .read_index(Default::default())
        .unwrap()
        .unwrap();
    let entry = index
        .entries()
        .iter()
        .find(|entry| entry.path == b"dir/c")
        .unwrap();
    let staged = git(&fixture.root, &["rev-parse", ":dir/c"]);
    assert_eq!(
        entry.id,
        String::from_utf8(staged)
            .unwrap()
            .trim()
            .parse::<ObjectId>()
            .unwrap()
    );
    // Git reads girt's index and finds nothing to rehash or rewrite.
    let before = fs::read(fixture.index()).unwrap();
    git(&fixture.root, &["update-index", "--refresh"]);
    assert_eq!(fs::read(fixture.index()).unwrap(), before);
}

/// A clean filter runs while the scan is in progress: it records whether `index.lock` exists then,
/// and rewrites `b` after girt has read and hashed it.
#[test]
fn lock_is_held_while_files_are_read_and_late_writes_stay_visible() {
    let fixture = Fixture::new();
    let probe = fixture.root.parent().unwrap().join("probe");
    let script = fixture.root.parent().unwrap().join("clean.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n[ -e '{lock}' ] && echo held >> '{probe}'\ncat\n[ \"$1\" = b ] && printf 'late\\n' > '{b}'\nexit 0\n",
            lock = fixture.root.join(".git/index.lock").display(),
            probe = probe.display(),
            b = fixture.root.join("b").display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &fixture.root,
        &[
            "config",
            "filter.probe.clean",
            &format!("{} %f", script.display()),
        ],
    );
    fs::write(
        fixture.root.join(".git/info/attributes"),
        "a filter=probe\nb filter=probe\n",
    )
    .unwrap();
    fixture.touch("a", -5);
    fixture.touch("b", -5);
    let before = fixture.stat("b");

    let repo = fixture.repo();
    let environment = Environment::default().with("GIT_CONFIG_NOSYSTEM", "1");
    let mut filters = repo
        .filters(&environment, girt::filter::Commands::Run)
        .unwrap();
    let refresh = repo
        .refresh_index_with(
            Some(&mut filters),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(fs::read_to_string(&probe).unwrap(), "held\nheld\n");
    assert!(!fixture.root.join(".git/index.lock").exists());
    // `b` matched when hashed, so its pre-read stat words were recorded; the later write changed
    // them, so neither girt nor Git trusts the entry.
    assert_eq!(refresh.recorded, 2);
    assert_ne!(fixture.stat("b"), before);
    let report = fixture
        .repo()
        .status_with(
            Baseline::Head,
            UntrackedFiles::Omit,
            &repo.ignore_rules(&environment).unwrap(),
            &Pathspec::all(),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(report.unstaged.len(), 1);
    assert_eq!(report.unstaged[0].path, b"b");
    assert_eq!(git(&fixture.root, &["diff-files", "--name-only"]), b"b\n");
}

/// A competing writer during the scan is refused, and the refresh then releases its lock.
#[test]
fn competing_edit_during_the_scan_is_locked_out() {
    let fixture = Fixture::new();
    let script = fixture.root.parent().unwrap().join("clean.sh");
    let probe = fixture.root.parent().unwrap().join("probe");
    // A competing writer creates the lock exclusively, as every cooperating writer does.
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n(set -C; : > '{lock}') 2>/dev/null && echo acquired > '{probe}' || echo locked > '{probe}'\ncat\nexit 0\n",
            lock = fixture.root.join(".git/index.lock").display(),
            probe = probe.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &fixture.root,
        &[
            "config",
            "filter.probe.clean",
            &script.display().to_string(),
        ],
    );
    fs::write(
        fixture.root.join(".git/info/attributes"),
        "a filter=probe\n",
    )
    .unwrap();
    fixture.touch("a", -5);
    let repo = fixture.repo();
    let environment = Environment::default().with("GIT_CONFIG_NOSYSTEM", "1");
    let mut filters = repo
        .filters(&environment, girt::filter::Commands::Run)
        .unwrap();
    let refresh = repo
        .refresh_index_with(
            Some(&mut filters),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(fs::read_to_string(&probe).unwrap(), "locked\n");
    assert!(refresh.written);
    assert!(!fixture.root.join(".git/index.lock").exists());
}
