//! `girt --git <command>` must print exactly what the equivalent `git` command prints.
//!
//! Each case runs both tools in the same scratch repository, with Git isolated from the
//! machine's configuration, and compares stdout byte for byte.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

/// A girt command and the Git command whose output it reproduces.
struct Case {
    girt: &'static [&'static str],
    git: &'static [&'static str],
    stdin: Option<&'static str>,
}

const fn case(girt: &'static [&'static str], git: &'static [&'static str]) -> Case {
    Case {
        girt,
        git,
        stdin: None,
    }
}

const CASES: &[Case] = &[
    case(&["object", "show", "HEAD"], &["cat-file", "-p", "HEAD"]),
    case(&["object", "show", "v1"], &["cat-file", "-p", "v1"]),
    case(
        &["object", "info", "--kind", "v1"],
        &["cat-file", "-t", "v1"],
    ),
    case(
        &["object", "info", "--size", "HEAD"],
        &["cat-file", "-s", "HEAD"],
    ),
    case(&["object", "hash", "a.txt"], &["hash-object", "a.txt"]),
    case(
        &["-C", "dir", "object", "hash", "c.txt"],
        &["-C", "dir", "hash-object", "c.txt"],
    ),
    Case {
        girt: &["object", "batch"],
        git: &["cat-file", "--batch"],
        stdin: Some("HEAD\nv1\nmain\n0000000000000000000000000000000000000001\n"),
    },
    Case {
        girt: &["object", "batch", "--check"],
        git: &["cat-file", "--batch-check"],
        stdin: Some("HEAD\nv1\n"),
    },
    case(&["tree", "list", "HEAD"], &["ls-tree", "HEAD"]),
    case(
        &["tree", "list", "--recursive", "HEAD"],
        &["ls-tree", "-r", "HEAD"],
    ),
    case(
        &["tree", "list", "--recursive", "--name-only", "HEAD"],
        &["ls-tree", "-r", "--name-only", "HEAD"],
    ),
    case(
        &["diff", "first", "main"],
        &["diff-tree", "-r", "--name-status", "first", "main"],
    ),
    case(
        &["diff", "--renames", "first", "main"],
        &["diff-tree", "-r", "-M", "--name-status", "first", "main"],
    ),
    case(
        &["diff", "--patch", "first", "main"],
        &[
            "-c",
            "core.abbrev=7",
            "diff",
            "--no-renames",
            "first",
            "main",
        ],
    ),
    case(&["index", "list"], &["ls-files"]),
    case(&["index", "list", "--stage"], &["ls-files", "-s"]),
    case(&["status", "--untracked"], &["status", "--porcelain"]),
    case(&["status"], &["status", "--porcelain", "-uno"]),
    case(
        &["ignore", "check", "debug.log", "a.txt"],
        &["check-ignore", "-v", "debug.log", "a.txt"],
    ),
    case(&["ref", "list"], &["for-each-ref"]),
    case(
        &["ref", "list", "refs/tags"],
        &["for-each-ref", "refs/tags"],
    ),
    case(&["ref", "show", "v1"], &["rev-parse", "v1"]),
    case(&["ref", "log", "main"], &["reflog", "show", "main"]),
    case(&["log"], &["log"]),
    case(
        &["log", "--oneline", "--limit", "1"],
        &["log", "--oneline", "-n", "1"],
    ),
    case(
        &["history", "count", "main"],
        &["rev-list", "--count", "main"],
    ),
    case(
        &["history", "merge-base", "first", "main"],
        &["merge-base", "first", "main"],
    ),
    case(
        &["history", "contains", "first", "main"],
        &["merge-base", "--is-ancestor", "first", "main"],
    ),
    case(
        &["history", "contains", "main", "first"],
        &["merge-base", "--is-ancestor", "main", "first"],
    ),
    case(&["remote", "list", "--urls"], &["remote", "-v"]),
    case(&["remote", "heads", "."], &["ls-remote", "."]),
];

#[test]
fn girt_matches_git() {
    let scratch = tempfile::tempdir().unwrap();
    let repository = scratch.path().join("repo");
    fixture(scratch.path(), &repository);

    let mut failures = Vec::new();
    for case in CASES {
        let girt = run(
            Command::new(env!("CARGO_BIN_EXE_girt"))
                .arg("--git")
                .args(case.girt),
            scratch.path(),
            &repository,
            case.stdin,
        );
        let git = run(
            Command::new("git").args(case.git),
            scratch.path(),
            &repository,
            case.stdin,
        );
        if girt.stdout != git.stdout || girt.status.code() != git.status.code() {
            failures.push(format!(
                "girt --git {}\n--- girt ({:?}, stderr: {})\n{}--- git {} ({:?})\n{}",
                case.girt.join(" "),
                girt.status.code(),
                String::from_utf8_lossy(&girt.stderr).trim(),
                String::from_utf8_lossy(&girt.stdout),
                case.git.join(" "),
                git.status.code(),
                String::from_utf8_lossy(&git.stdout),
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Builds a small repository: two commits with an edit, a rename, an ignored file and an
/// annotated tag, plus uncommitted and untracked changes.
fn fixture(home: &Path, repository: &Path) {
    let git = |args: &[&str]| {
        let output = run(Command::new("git").args(args), home, repository, None);
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    fs::create_dir_all(repository.join("dir")).unwrap();
    git(&["init", "--quiet", "--initial-branch=main"]);
    fs::write(
        repository.join("a.txt"),
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\n",
    )
    .unwrap();
    fs::write(
        repository.join("dir/b.txt"),
        "alpha\nbeta\ngamma\ndelta\nepsilon\n",
    )
    .unwrap();
    fs::write(repository.join("gone.txt"), "removed later\n").unwrap();
    fs::write(repository.join(".gitignore"), "*.log\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "First commit"]);
    git(&["tag", "first"]);

    fs::write(
        repository.join("a.txt"),
        "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\neight",
    )
    .unwrap();
    fs::rename(repository.join("dir/b.txt"), repository.join("dir/c.txt")).unwrap();
    fs::write(
        repository.join("dir/c.txt"),
        "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\n",
    )
    .unwrap();
    fs::remove_file(repository.join("gone.txt")).unwrap();
    git(&["add", "--all"]);
    git(&["commit", "--quiet", "-m", "Second commit\n\nWith a body."]);
    git(&["tag", "--annotate", "v1", "-m", "Version one"]);
    git(&["remote", "add", "origin", "https://example.com/repo.git"]);

    fs::write(repository.join("a.txt"), "changed in the worktree\n").unwrap();
    fs::write(repository.join("new.txt"), "untracked\n").unwrap();
    fs::write(repository.join("debug.log"), "ignored\n").unwrap();
}

/// Runs a command in `repository` with Git configuration isolated to `home`.
fn run(command: &mut Command, home: &Path, repository: &Path, stdin: Option<&str>) -> Output {
    use std::io::Write;
    use std::process::Stdio;

    command
        .current_dir(repository)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env("GIT_AUTHOR_NAME", "Ada Lovelace")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-02T03:04:05+01:00")
        .env("GIT_COMMITTER_NAME", "Ada Lovelace")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_DATE", "2026-01-02T03:04:05+01:00")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn");
    let input = stdin.unwrap_or_default().as_bytes().to_vec();
    let mut pipe = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || pipe.write_all(&input));
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();
    output
}
