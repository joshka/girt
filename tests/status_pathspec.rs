//! Path-limited status compared with `git status --porcelain -- <pathspec>`.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::Repository;
use girt::pathspec::{Context, Pathspec};
use girt::status::{Baseline, Limits, UntrackedFiles, UntrackedKind};
use girt::workflow::Environment;

fn git(home: &Path, directory: &Path, args: &[&str]) -> (bool, Vec<u8>) {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") || key == "XDG_CONFIG_HOME" {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(directory)
        .args(["-c", "commit.gpgsign=false", "-c", "core.fsmonitor=false"])
        .args(["-c", "status.relativePaths=false"])
        .args(args)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Status Fixture")
        .env("GIT_AUTHOR_EMAIL", "status@example.invalid")
        .env("GIT_COMMITTER_NAME", "Status Fixture")
        .env("GIT_COMMITTER_EMAIL", "status@example.invalid")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (output.status.success(), output.stdout)
}

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: std::path::PathBuf,
    root: std::path::PathBuf,
}

/// Tracked, modified, staged, deleted and untracked paths at several depths, plus an ignored
/// directory.
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = temp.path().join("repo");
    fs::create_dir(&home).unwrap();
    let run = |args: &[&str]| assert!(git(&home, &root, args).0, "{args:?}");
    assert!(git(&home, &home, &["init", "-q", "../repo"]).0);
    run(&["config", "core.ignoreCase", "false"]);
    write(
        &root,
        &[
            ("t/a", "a\n"),
            ("t/b", "b\n"),
            ("t/deep/d", "d\n"),
            ("root", "r\n"),
            ("src/lib.rs", "lib\n"),
            ("src/gone.rs", "gone\n"),
            ("obs/file", "o\n"),
            (".gitignore", "ign/\n"),
        ],
    );
    run(&["add", "."]);
    run(&["commit", "-qm", "initial"]);
    write(
        &root,
        &[
            ("t/a", "changed\n"),
            ("t/c", "new\n"),
            ("new/a", "x\n"),
            ("new/deep/b", "y\n"),
            ("only/x", "1\n"),
            ("ign/f", "i\n"),
            ("staged.rs", "s\n"),
            ("src/new.rs", "n\n"),
        ],
    );
    fs::remove_file(root.join("src/gone.rs")).unwrap();
    // An untracked nested repository, and a tracked directory replaced by a file.
    assert!(git(&home, &home, &["init", "-q", "../repo/nested"]).0);
    write(&root, &[("nested/file", "n\n")]);
    fs::remove_dir_all(root.join("obs")).unwrap();
    write(&root, &[("obs", "now a file\n")]);
    run(&["add", "staged.rs"]);
    Fixture {
        _temp: temp,
        home,
        root,
    }
}

const SPECS: &[&[&str]] = &[
    &["."],
    &["new"],
    &["new/"],
    &["new/a"],
    &["new/*"],
    &[":!new"],
    &["*a"],
    &["n*"],
    &["t/c"],
    &["t"],
    &[":(glob)*"],
    &["new/deep"],
    &["new/deep/b"],
    &["ne"],
    &["*/b"],
    &[":(glob)new/*"],
    &["ign"],
    &["ign/f"],
    &["staged.rs"],
    &[":!t"],
    &["new", ":!new/a"],
    &["new", ":!new/deep"],
    &["only", ":!only/x"],
    &["o*", ":!*/x"],
    &["new/", ":(exclude)n*"],
    &[":(icase)NEW"],
    &["*.rs"],
    &[":(glob)**/*.rs"],
    &["src/gone.rs"],
    &["src"],
    &[":!src", ":!t"],
    &["nested/file"],
    &["nested/*"],
    &["nested"],
    &["obs/file"],
    &["obs"],
];

/// Renders a report as `git status --porcelain` lines with repository-relative paths.
fn girt_status(
    fixture: &Fixture,
    specs: &[&str],
    prefix: &str,
    untracked: UntrackedFiles,
) -> Vec<String> {
    let environment = Environment::default()
        .with("HOME", &fixture.home)
        .with("GIT_CONFIG_NOSYSTEM", "1");
    let repository = Repository::open(&fixture.root).unwrap();
    let rules = repository.ignore_rules(&environment).unwrap();
    let context = Context::new()
        .with_prefix(prefix.as_bytes())
        .with_excludes_from_root(true);
    let paths = Pathspec::parse(specs, &context).unwrap();
    let report = repository
        .status_with(
            Baseline::Head,
            untracked,
            &rules,
            &paths,
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let mut lines = std::collections::BTreeMap::<Vec<u8>, [char; 2]>::new();
    for change in &report.staged {
        lines.entry(change.path.clone()).or_insert([' '; 2])[0] = change.status().letter();
    }
    for change in &report.unstaged {
        lines.entry(change.path.clone()).or_insert([' '; 2])[1] =
            change.status().map_or('D', |status| status.letter());
    }
    let mut out: Vec<String> = lines
        .into_iter()
        .map(|(path, [x, y])| format!("{x}{y} {}", String::from_utf8_lossy(&path)))
        .collect();
    for entry in &report.untracked {
        let slash = if entry.kind == UntrackedKind::File {
            ""
        } else {
            "/"
        };
        out.push(format!(
            "?? {}{slash}",
            String::from_utf8_lossy(&entry.path)
        ));
    }
    out
}

fn git_status(fixture: &Fixture, specs: &[&str], prefix: &str, all: bool) -> Vec<String> {
    let mut args = vec!["status", "--porcelain"];
    args.push(if all { "-uall" } else { "-unormal" });
    args.push("--");
    args.extend(specs);
    let (ok, stdout) = git(&fixture.home, &fixture.root.join(prefix), &args);
    assert!(ok, "git status {specs:?}");
    String::from_utf8(stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn matches_git_status() {
    let fixture = fixture();
    for specs in SPECS {
        for (all, untracked) in [
            (false, UntrackedFiles::Collapsed),
            (true, UntrackedFiles::All),
        ] {
            assert_eq!(
                girt_status(&fixture, specs, "", untracked),
                git_status(&fixture, specs, "", all),
                "{specs:?} all={all}"
            );
        }
    }
}

#[test]
fn matches_git_status_from_a_subdirectory() {
    let fixture = fixture();
    for specs in [
        &["."][..],
        &["a"],
        &["../new"],
        &[":/src"],
        &[":!deep"],
        &[":!a"],
        &["*"],
    ] {
        assert_eq!(
            girt_status(&fixture, specs, "t", UntrackedFiles::Collapsed),
            git_status(&fixture, specs, "t", false),
            "{specs:?}"
        );
    }
}

#[test]
fn refuses_attribute_magic() {
    let fixture = fixture();
    let repository = Repository::open(&fixture.root).unwrap();
    let paths = Pathspec::parse([":(attr:text)"], &Context::new()).unwrap();
    let result = repository.raw_status_with(
        Baseline::Head,
        girt::status::Untracked::Omit,
        &paths,
        Limits::default(),
        &AtomicBool::new(false),
    );
    assert!(matches!(
        result,
        Err(girt::status::Error::Unsupported { .. })
    ));
}

/// A selected path below a tracked directory that became a file is reported as obstructed, as an
/// unlimited scan reports it, not as a plain deletion.
#[test]
fn keeps_obstruction_below_unselected_ancestor() {
    let fixture = fixture();
    let repository = Repository::open(&fixture.root).unwrap();
    let report = |specs: &[&str]| {
        let paths = Pathspec::parse(specs, &Context::new()).unwrap();
        repository
            .raw_status_with(
                Baseline::Head,
                girt::status::Untracked::Omit,
                &paths,
                Limits::default(),
                &AtomicBool::new(false),
            )
            .unwrap()
            .unstaged
            .into_iter()
            .find(|change| change.path == b"obs/file")
            .unwrap()
            .change
    };
    assert_eq!(report(&["obs/file"]), report(&["."]));
    assert!(matches!(
        report(&["obs/file"]),
        girt::status::Change::Obstructed { .. }
    ));
}
