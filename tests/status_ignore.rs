//! Ignore-aware status compared with `git status`, on original fixtures built with Git's CLI.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use girt::Repository;
use girt::config::ConfigInputs;
use girt::ignore::{InputKind, LoadError, SourceKind};
use girt::status::{Baseline, Change, Error, Limits, Report, UntrackedFiles, UntrackedKind};
use girt::workflow::Environment;
use rstest::rstest;

/// A worktree plus a private home directory, so global configuration is controlled.
struct Fixture {
    _temp: tempfile::TempDir,
    home: std::path::PathBuf,
    root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let root = temp.path().join("repo");
        fs::create_dir(&home).unwrap();
        let fixture = Self {
            _temp: temp,
            home,
            root,
        };
        fixture.git(fixture.home.as_path(), &["init", "-q", "../repo"]);
        // Pin Git's platform-dependent default so both implementations match exactly.
        fixture.git_in_repo(&["config", "core.ignoreCase", "false"]);
        fixture
    }

    fn environment(&self) -> Environment {
        Environment::default()
            .with("HOME", &self.home)
            .with("GIT_CONFIG_NOSYSTEM", "1")
    }

    fn git(&self, directory: &Path, args: &[&str]) -> Vec<u8> {
        let output = self.command(directory, args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    /// Runs Git in the repository with `input` on standard input, returning its trimmed output.
    fn git_stdin(&self, args: &[&str], input: &str) -> String {
        use std::io::Write as _;
        use std::process::Stdio;
        let mut child = self
            .command(&self.root, args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn command(&self, directory: &Path, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") || key == "XDG_CONFIG_HOME" {
                command.env_remove(key);
            }
        }
        command
            .current_dir(directory)
            .args(["-c", "commit.gpgsign=false", "-c", "core.fsmonitor=false"])
            .args(args)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Status Fixture")
            .env("GIT_AUTHOR_EMAIL", "status@example.invalid")
            .env("GIT_COMMITTER_NAME", "Status Fixture")
            .env("GIT_COMMITTER_EMAIL", "status@example.invalid")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C");
        command
    }

    fn git_in_repo(&self, args: &[&str]) -> Vec<u8> {
        self.git(&self.root, args)
    }

    /// Writes files, creating parents; a trailing `/` creates an empty directory.
    fn write(&self, files: &[(&str, &str)]) {
        for (path, content) in files {
            let full = self.root.join(path);
            if path.ends_with('/') {
                fs::create_dir_all(full).unwrap();
            } else {
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(full, content).unwrap();
            }
        }
    }

    fn track(&self, paths: &[&str]) {
        let mut args = vec!["add", "-f", "--"];
        args.extend_from_slice(paths);
        self.git_in_repo(&args);
    }

    fn repository(&self) -> Repository {
        Repository::open_with_config(
            &self.root,
            &ConfigInputs::git_default(&self.environment()).unwrap(),
        )
        .unwrap()
    }

    fn status(&self, untracked: UntrackedFiles) -> Report {
        let repo = self.repository();
        let rules = repo.ignore_rules(&self.environment()).unwrap();
        repo.status(Baseline::Head, untracked, &rules).unwrap()
    }

    /// Git's untracked entries, which end with `/` for directories and nested repositories.
    fn git_untracked(&self, mode: &str) -> Vec<String> {
        let output = self.git_in_repo(&[
            "status",
            "--porcelain=v1",
            "-z",
            &format!("--untracked-files={mode}"),
        ]);
        let mut entries: Vec<String> = output
            .split(|byte| *byte == 0)
            .filter_map(|entry| entry.strip_prefix(b"?? "))
            .map(|path| String::from_utf8(path.to_vec()).unwrap())
            .collect();
        entries.sort();
        entries
    }
}

/// Renders untracked entries as Git displays them.
fn displayed(report: &Report) -> Vec<String> {
    let mut entries: Vec<String> = report
        .untracked
        .iter()
        .map(|entry| {
            let path = String::from_utf8(entry.path.clone()).unwrap();
            match entry.kind {
                UntrackedKind::File => path,
                _ => format!("{path}/"),
            }
        })
        .collect();
    entries.sort();
    entries
}

/// Each scenario lists files to write (a trailing `/` makes an empty directory) and paths to track.
#[rstest]
#[case::root_patterns(&[(".gitignore", "*.log\n"), ("a.log", ""), ("a.txt", "")], &[])]
#[case::negation(
    &[(".gitignore", "*.log\n!keep.log\n"), ("keep.log", ""), ("drop.log", "")],
    &[]
)]
#[case::excluded_parent_wins_over_negation(
    &[(".gitignore", "build/\n!build/keep\n"), ("build/keep", ""), ("build/other", "")],
    &[]
)]
#[case::deeper_file_overrides(
    &[(".gitignore", "*.tmp\n"), ("sub/.gitignore", "!*.tmp\n"), ("sub/a.tmp", ""), ("b.tmp", "")],
    &[]
)]
#[case::anchored_and_directory_only(
    &[(".gitignore", "/top\nonly/\n"), ("top", ""), ("sub/top", ""), ("x/only/f", ""), ("y/only", "")],
    &[]
)]
#[case::tracked_paths_are_never_ignored(
    &[(".gitignore", "*.log\nbuild/\n"), ("kept.log", "x"), ("build/tracked", "x"), ("build/new", "")],
    &["kept.log", "build/tracked"]
)]
#[case::directory_of_ignored_files_is_hidden(
    &[(".gitignore", "*.o\n"), ("objs/a.o", ""), ("objs/b.o", ""), ("empty/", "")],
    &[]
)]
#[case::untracked_beside_tracked(
    &[("src/main.rs", "x"), ("src/new.rs", ""), ("src/gen/out.rs", ""), ("docs/a.md", "")],
    &["src/main.rs"]
)]
#[case::nested_ignore_in_untracked_directory(
    &[("new/.gitignore", "*\n!.gitignore\n"), ("new/a", ""), ("other/.gitignore", "*\n"), ("other/b", "")],
    &[]
)]
#[case::double_star(
    &[(".gitignore", "**/cache/**\n"), ("a/b/cache/f", ""), ("a/cache.txt", "")],
    &[]
)]
fn untracked_listing_matches_git(#[case] files: &[(&str, &str)], #[case] tracked: &[&str]) {
    let fixture = Fixture::new();
    fixture.write(files);
    if !tracked.is_empty() {
        fixture.track(tracked);
    }
    assert_eq!(
        displayed(&fixture.status(UntrackedFiles::All)),
        fixture.git_untracked("all"),
        "--untracked-files=all"
    );
    assert_eq!(
        displayed(&fixture.status(UntrackedFiles::Collapsed)),
        fixture.git_untracked("normal"),
        "--untracked-files=normal"
    );
}

/// Nested repositories are listed as directories unless ignored, and never entered.
#[test]
fn nested_repositories_match_git() {
    let fixture = Fixture::new();
    fixture.write(&[(".gitignore", "hidden/\n"), ("deep/", ""), ("hidden/", "")]);
    for nested in ["nested", "deep/inner", "hidden/repo"] {
        fixture.git_in_repo(&["init", "-q", nested]);
    }
    let all = fixture.status(UntrackedFiles::All);
    assert_eq!(displayed(&all), fixture.git_untracked("all"));
    assert_eq!(
        displayed(&fixture.status(UntrackedFiles::Collapsed)),
        fixture.git_untracked("normal")
    );
    assert!(
        all.untracked
            .iter()
            .any(|entry| entry.path == b"nested" && entry.kind == UntrackedKind::Repository)
    );
}

/// Untracked paths that replace tracked ones are listed beside the tracked change.
#[test]
fn obstructions_list_their_untracked_replacements() {
    let fixture = Fixture::new();
    fixture.write(&[("leaf", "x"), ("tree/file", "x"), ("kept", "x")]);
    fixture.track(&["leaf", "tree/file", "kept"]);
    fs::remove_file(fixture.root.join("leaf")).unwrap();
    fs::remove_dir_all(fixture.root.join("tree")).unwrap();
    fixture.write(&[("leaf/inner", ""), ("tree", "")]);
    assert_eq!(
        displayed(&fixture.status(UntrackedFiles::All)),
        fixture.git_untracked("all")
    );
    assert_eq!(
        displayed(&fixture.status(UntrackedFiles::Collapsed)),
        fixture.git_untracked("normal")
    );
    assert!(fixture.status(UntrackedFiles::Omit).untracked.is_empty());
}

/// Sockets and FIFOs are not untracked files for Git.
#[test]
fn special_files_are_not_listed() {
    let fixture = Fixture::new();
    fixture.write(&[("fifo-only/", ""), ("regular", "")]);
    for fifo in ["pipe", "fifo-only/pipe"] {
        let status = Command::new("mkfifo")
            .arg(fixture.root.join(fifo))
            .status()
            .unwrap();
        assert!(status.success());
    }
    assert_eq!(
        displayed(&fixture.status(UntrackedFiles::All)),
        fixture.git_untracked("all")
    );
    assert_eq!(
        displayed(&fixture.status(UntrackedFiles::Collapsed)),
        ["regular"]
    );
}

/// Like Git, a symlinked `.gitignore` is skipped rather than followed.
#[test]
fn symlinked_gitignore_is_not_followed() {
    let fixture = Fixture::new();
    fixture.write(&[
        ("rules", "*.a\n"),
        ("x.a", ""),
        ("sub/rules", "*.b\n"),
        ("sub/y.b", ""),
    ]);
    symlink("rules", fixture.root.join(".gitignore")).unwrap();
    symlink("rules", fixture.root.join("sub/.gitignore")).unwrap();
    assert_eq!(
        displayed(&fixture.status(UntrackedFiles::All)),
        fixture.git_untracked("all")
    );
}

/// The excludes file, `info/exclude` and `.gitignore` apply in Git's precedence order.
#[rstest]
#[case::default_global(None, &[("global.x", ""), ("info.x", ""), ("local.x", "")])]
#[case::configured_relative(Some("rules/excludes"), &[("global.x", ""), ("rules/excludes", "global.x\n")])]
#[case::configured_home(Some("~/excludes"), &[("global.x", "")])]
#[case::empty_names_no_file(Some(""), &[("global.x", "")])]
fn configuration_sources_match_git(#[case] excludes: Option<&str>, #[case] files: &[(&str, &str)]) {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.home.join(".config/git")).unwrap();
    fs::write(
        fixture.home.join(".config/git/ignore"),
        "global.x\n!local.x\n",
    )
    .unwrap();
    fs::write(fixture.home.join("excludes"), "global.x\n").unwrap();
    fs::write(
        fixture.root.join(".git/info/exclude"),
        "info.x\nlocal.x\n!global.x\n",
    )
    .unwrap();
    if let Some(excludes) = excludes {
        // Global configuration, read only through `ConfigInputs::git_default`.
        fs::write(
            fixture.home.join(".gitconfig"),
            format!("[core]\n\texcludesFile = \"{excludes}\"\n"),
        )
        .unwrap();
    }
    fixture.write(files);
    fixture.write(&[(".gitignore", "local.x\n")]);
    assert_eq!(
        displayed(&fixture.status(UntrackedFiles::All)),
        fixture.git_untracked("all")
    );
}

/// `core.ignoreCase` folds ASCII case in patterns, as Git does.
#[rstest]
#[case::sensitive("false")]
#[case::insensitive("true")]
fn ignore_case_matches_git(#[case] value: &str) {
    let fixture = Fixture::new();
    fixture.git_in_repo(&["config", "core.ignoreCase", value]);
    fixture.write(&[
        (".gitignore", "*.LOG\nBuild/\n"),
        ("a.log", ""),
        ("build/x", ""),
    ]);
    assert_eq!(
        displayed(&fixture.status(UntrackedFiles::All)),
        fixture.git_untracked("all")
    );
}

/// Omit performs no ignore work and lists nothing; tracked changes are still reported.
#[test]
fn omit_reports_tracked_changes_only() {
    let fixture = Fixture::new();
    fixture.write(&[(".gitignore", "*.log\n"), ("kept.log", "old"), ("new", "")]);
    fixture.track(&["kept.log"]);
    fixture.write(&[("kept.log", "new")]);
    let report = fixture.status(UntrackedFiles::Omit);
    assert!(report.untracked.is_empty());
    assert!(report.raw_untracked.is_empty());
    assert_eq!(report.untracked_files, Some(UntrackedFiles::Omit));
    assert_eq!(report.unstaged[0].path, b"kept.log");
}

/// Ignored directories are never traversed, so their contents cost no directory-entry budget.
#[test]
fn ignored_directories_are_not_traversed() {
    let fixture = Fixture::new();
    fixture.write(&[(".gitignore", "/target/\n")]);
    for index in 0..50 {
        fixture.write(&[(&format!("target/{index}"), "")]);
    }
    let repo = fixture.repository();
    let rules = repo.ignore_rules(&fixture.environment()).unwrap();
    // `.git` contents are not visited; the root holds `.git`, `.gitignore` and `target`.
    let limits = Limits::default().with_max_directory_entries(3);
    let report = repo
        .status_with(
            Baseline::Head,
            UntrackedFiles::All,
            &rules,
            &girt::pathspec::Pathspec::all(),
            limits,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(displayed(&report), [".gitignore"]);
    let raw = repo.raw_status_with(
        Baseline::Head,
        girt::status::Untracked::RawFilesWithoutIgnores,
        &girt::pathspec::Pathspec::all(),
        limits,
        &std::sync::atomic::AtomicBool::new(false),
    );
    assert!(matches!(raw, Err(Error::Limit("directory entries"))));
}

/// Rule limits apply to `.gitignore` files read during traversal.
#[test]
fn gitignore_beyond_rule_limits_fails() {
    let fixture = Fixture::new();
    fixture.write(&[("sub/.gitignore", "a\nb\n"), ("sub/f", "")]);
    let repo = fixture.repository();
    let rules = repo
        .ignore_rules_with(
            &fixture.environment(),
            girt::ignore::Limits::default().with_patterns(1),
        )
        .unwrap();
    let result = repo.status(Baseline::Head, UntrackedFiles::All, &rules);
    assert!(matches!(result, Err(Error::Ignore { path, .. }) if path == b"sub/.gitignore"));
}

/// `check` loads ancestor `.gitignore` files lazily and explains the decisive line.
#[test]
fn check_explains_the_decisive_source() {
    let fixture = Fixture::new();
    fixture.write(&[
        (".gitignore", "*.log\n"),
        ("sub/.gitignore", "# keep\n!keep.log\n"),
    ]);
    let repo = fixture.repository();
    let mut rules = repo.ignore_rules(&fixture.environment()).unwrap();
    let found = rules
        .check(b"sub/keep.log", InputKind::File)
        .unwrap()
        .unwrap();
    assert!(!found.ignored);
    let source = rules.source(found.source).unwrap();
    assert_eq!(source.kind(), &SourceKind::Directory(b"sub".to_vec()));
    assert_eq!(source.line(found.line), Some(&b"!keep.log"[..]));
    let output = fixture.git_in_repo(&["check-ignore", "-v", "--no-index", "sub/keep.log"]);
    assert!(output.starts_with(b"sub/.gitignore:2:!keep.log\t"));
}

/// Git refuses `~user/` excludes paths it cannot expand; girt refuses every one explicitly.
#[test]
fn user_home_expansion_is_unsupported() {
    let fixture = Fixture::new();
    fixture.git_in_repo(&["config", "core.excludesFile", "~nosuchuser/ignore"]);
    let result = fixture.repository().ignore_rules(&fixture.environment());
    assert!(matches!(result, Err(LoadError::Unsupported(_))));
}

/// Repeating a query never reads a `.gitignore` through a symlinked directory.
#[test]
fn check_never_reads_through_a_symlink_on_repeat() {
    let fixture = Fixture::new();
    let outside = fixture.home.join("outside");
    fs::create_dir_all(outside.join("sub")).unwrap();
    fs::write(outside.join(".gitignore"), "*.log\n").unwrap();
    fs::write(outside.join("sub/.gitignore"), "*.log\n").unwrap();
    symlink(&outside, fixture.root.join("link")).unwrap();
    let mut rules = fixture
        .repository()
        .ignore_rules(&fixture.environment())
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            rules.check(b"link/sub/a.log", InputKind::File).unwrap(),
            None
        );
    }
}

/// A source over the byte budget fails before it is buffered, and keeps failing on retry.
#[rstest]
#[case::bytes(girt::ignore::Limits::default().with_bytes(4), "*.log\n")]
#[case::patterns(girt::ignore::Limits::default().with_patterns(1), "a\nb\n")]
fn rejected_gitignore_is_not_skipped_on_retry(
    #[case] limits: girt::ignore::Limits,
    #[case] content: &str,
) {
    let fixture = Fixture::new();
    // Git's template `info/exclude` would exceed the small budgets first.
    fs::remove_file(fixture.root.join(".git/info/exclude")).unwrap();
    fixture.write(&[(".gitignore", content)]);
    let mut rules = fixture
        .repository()
        .ignore_rules_with(&fixture.environment(), limits)
        .unwrap();
    for _ in 0..2 {
        let result = rules.check(b"a", InputKind::File);
        assert!(matches!(result, Err(LoadError::Rules { .. })), "{result:?}");
    }
}

/// Configured sources are bounded too, and a FIFO is refused instead of blocking.
#[test]
fn configured_sources_are_bounded_and_never_block() {
    let fixture = Fixture::new();
    let exclude = fixture.root.join(".git/info/exclude");
    fs::write(&exclude, "x".repeat(64)).unwrap();
    let repo = fixture.repository();
    let small = girt::ignore::Limits::default().with_bytes(8);
    let result = repo.ignore_rules_with(&fixture.environment(), small);
    assert!(matches!(result, Err(LoadError::Rules { .. })));
    fs::remove_file(&exclude).unwrap();
    let status = Command::new("mkfifo").arg(&exclude).status().unwrap();
    assert!(status.success());
    let result = repo.ignore_rules(&fixture.environment());
    assert!(matches!(result, Err(LoadError::Unsupported(_))));
}

/// A real user's home resolves independently of HOME in Git, unlike girt's explicit refusal.
///
/// This fixture deliberately creates a temporary directory in the real home so Git can resolve
/// `~user` through the user database. It requires a writable HOME matching that user's home.
#[test]
fn existing_user_home_excludes_are_a_git_compatibility_gap() {
    let user = Command::new("id").arg("-un").output().unwrap();
    assert!(user.status.success());
    let user = std::str::from_utf8(&user.stdout).unwrap().trim();
    let home = std::env::var_os("HOME").expect("HOME is required for this Unix fixture");
    let excludes = tempfile::Builder::new()
        .prefix(".girt-ignore-audit-")
        .tempdir_in(&home)
        .unwrap();
    fs::write(excludes.path().join("ignore"), "audit-ignored\n").unwrap();
    let tail = excludes.path().file_name().unwrap().to_str().unwrap();
    let fixture = Fixture::new();
    fixture.git_in_repo(&[
        "config",
        "core.excludesFile",
        &format!("~{user}/{tail}/ignore"),
    ]);
    fixture.write(&[("audit-ignored", "payload")]);
    assert_eq!(
        fixture.git_in_repo(&["check-ignore", "audit-ignored"]),
        b"audit-ignored\n"
    );
    assert!(matches!(
        fixture.repository().ignore_rules(&fixture.environment()),
        Err(LoadError::Unsupported(_))
    ));
}

/// Git's porcelain lines for working-tree changes and untracked files, sorted.
fn git_lines(fixture: &Fixture) -> Vec<Vec<u8>> {
    let output = fixture.git_in_repo(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    let mut lines: Vec<Vec<u8>> = output
        .split(|byte| *byte == 0)
        .filter(|line| !line.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    lines.sort();
    lines
}

/// Girt's report rendered as [`git_lines`] renders Git's.
fn girt_lines(report: &Report) -> Vec<Vec<u8>> {
    assert!(report.staged.is_empty());
    let mut lines: Vec<Vec<u8>> = report
        .unstaged
        .iter()
        .map(|change| {
            let code = match change.change {
                Change::Deleted => " D ",
                Change::Modified(_) => " M ",
                ref other => panic!("unexpected change {other:?}"),
            };
            [code.as_bytes(), &change.path].concat()
        })
        .chain(
            report
                .untracked
                .iter()
                .map(|entry| [b"?? ".as_slice(), &entry.path].concat()),
        )
        .collect();
    lines.sort();
    lines
}

/// Settings of `core.ignoreCase` and `core.precomposeUnicode` to compare under; `None` unsets.
fn name_settings() -> Vec<(Option<&'static str>, Option<&'static str>)> {
    let values = [None, Some("true"), Some("false")];
    values
        .iter()
        .flat_map(|case| values.iter().map(move |compose| (*case, *compose)))
        .collect()
}

fn configure_names(fixture: &Fixture, case: Option<&str>, compose: Option<&str>) {
    for (key, value) in [
        ("core.ignoreCase", case),
        ("core.precomposeUnicode", compose),
    ] {
        match value {
            Some(value) => fixture.git_in_repo(&["config", key, value]),
            // Exits 5 when the key is already absent.
            None => {
                let status = fixture
                    .command(&fixture.root, &["config", "--unset-all", key])
                    .status()
                    .unwrap();
                assert!(matches!(status.code(), Some(0 | 5)));
                Vec::new()
            }
        };
    }
}

/// Renames tracked paths to other case and normalization spellings, then compares status with Git
/// under every `core.ignoreCase` and `core.precomposeUnicode` setting. On a case-insensitive macOS
/// volume the renamed files are aliases of their index paths; on Linux they are new names.
#[test]
fn renamed_spellings_match_git_under_name_settings() {
    let composed = "caf\u{e9}";
    let decomposed = "cafe\u{301}";
    for (case, compose) in name_settings() {
        let fixture = Fixture::new();
        // Track the composed spelling exactly as written.
        fixture.git_in_repo(&["config", "core.precomposeUnicode", "false"]);
        fixture.write(&[
            ("file", "same"),
            ("Dir/f", "same"),
            (composed, "same"),
            ("keep", "same"),
        ]);
        fixture.track(&["file", "Dir/f", composed, "keep"]);
        fixture.git_in_repo(&["commit", "-qm", "spellings"]);
        let root = &fixture.root;
        fs::rename(root.join("file"), root.join("FILE")).unwrap();
        fs::rename(root.join("Dir"), root.join("dir")).unwrap();
        fs::rename(root.join(composed), root.join(decomposed)).unwrap();
        configure_names(&fixture, case, compose);
        let report = fixture.status(UntrackedFiles::All);
        assert_eq!(
            girt_lines(&report),
            git_lines(&fixture),
            "ignoreCase={case:?} precomposeUnicode={compose:?}"
        );
    }
}

/// After Git checks out colliding paths, status reports each shadowed path as Git does.
#[test]
fn collision_shadows_match_git_under_name_settings() {
    for (case, compose) in name_settings() {
        let fixture = Fixture::new();
        let mut info = String::new();
        for (path, content) in [
            ("Dir/f", "upper dir"),
            ("X.h", "upper"),
            ("caf\u{e9}", "composed"),
            ("cafe\u{301}", "decomposed"),
            ("dir/f", "lower dir"),
            ("same", "same"),
            ("x.h", "lower"),
        ] {
            let blob = fixture.git_stdin(&["hash-object", "-w", "--stdin"], content);
            info.push_str(&format!("100644 {}\t{path}\0", blob.trim()));
        }
        fixture.git_stdin(&["update-index", "-z", "--index-info"], &info);
        fixture.git_in_repo(&["commit", "-qm", "collisions"]);
        fs::remove_file(fixture.root.join(".git/index")).unwrap();
        fixture.git_in_repo(&["read-tree", "--reset", "-u", "HEAD"]);
        configure_names(&fixture, case, compose);
        let report = fixture.status(UntrackedFiles::All);
        assert_eq!(
            girt_lines(&report),
            git_lines(&fixture),
            "ignoreCase={case:?} precomposeUnicode={compose:?}"
        );
    }
}
