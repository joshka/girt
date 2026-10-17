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
    case(
        &["object", "list"],
        &["cat-file", "--batch-all-objects", "--batch-check"],
    ),
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
        &["diff", "--no-renames", "first", "main"],
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
        &["history", "count", "first..main"],
        &["rev-list", "--count", "first..main"],
    ),
    case(
        &["history", "count", "main", "^v1~1"],
        &["rev-list", "--count", "main", "^v1~1"],
    ),
    case(&["log", "first..HEAD"], &["log", "first..HEAD"]),
    case(&["log", "HEAD...first"], &["log", "HEAD...first"]),
    case(
        &["object", "id", "v1^{}"],
        &["rev-parse", "--verify", "v1^{}"],
    ),
    case(&["object", "id", "@~1"], &["rev-parse", "--verify", "@~1"]),
    case(
        &["object", "id", "first:dir/b.txt"],
        &["rev-parse", "--verify", "first:dir/b.txt"],
    ),
    case(&["tree", "list", "HEAD~0:dir"], &["ls-tree", "HEAD~0:dir"]),
    case(
        &["object", "show", "main:dir/c.txt"],
        &["cat-file", "-p", "main:dir/c.txt"],
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

/// Lists and counts a store mixing packs, loose objects, a loose copy of a packed object and
/// an alternate, as Git does.
#[test]
fn object_listing_matches_git_across_packs_and_alternates() {
    let scratch = tempfile::tempdir().unwrap();
    let repository = scratch.path().join("repo");
    fixture(scratch.path(), &repository);
    let git = |args: &[&str], stdin: Option<&str>| {
        let output = run(
            Command::new("git").args(args),
            scratch.path(),
            &repository,
            stdin,
        );
        assert!(output.status.success(), "git {args:?}");
        output.stdout
    };
    // Pack the first commit's objects, keep the second's loose and re-add one packed blob.
    git(&["repack", "--quiet", "-d"], None);
    fs::write(repository.join("loose.txt"), "loose only\n").unwrap();
    let loose = String::from_utf8(git(&["hash-object", "-w", "loose.txt"], None)).unwrap();
    let packed = git(&["rev-parse", "first:dir/b.txt"], None);
    let packed = String::from_utf8(packed).unwrap();
    let data = git(&["cat-file", "blob", packed.trim()], None);
    let objects = repository.join(".git/objects");
    // A loose copy of a packed object, written by Git under its own name.
    fs::write(repository.join("copy.txt"), &data).unwrap();
    git(&["hash-object", "-w", "copy.txt"], None);

    let borrowed = scratch.path().join("borrowed.git");
    let init = run(
        Command::new("git").args(["init", "--quiet", "--bare", borrowed.to_str().unwrap()]),
        scratch.path(),
        scratch.path(),
        None,
    );
    assert!(init.status.success());
    let borrowed_git = |args: &[&str]| {
        run(
            Command::new("git").args(args),
            scratch.path(),
            &borrowed,
            None,
        )
    };
    fs::write(borrowed.join("only.txt"), "borrowed only\n").unwrap();
    assert!(
        borrowed_git(&["hash-object", "-w", "only.txt"])
            .status
            .success()
    );
    fs::write(
        objects.join("info/alternates"),
        format!("{}\n", borrowed.join("objects").display()),
    )
    .unwrap();

    let compare = |girt_args: &[&str], git_args: &[&str]| {
        let girt = run(
            Command::new(env!("CARGO_BIN_EXE_girt"))
                .arg("--git")
                .args(girt_args),
            scratch.path(),
            &repository,
            None,
        );
        let expected = String::from_utf8(git(git_args, None)).unwrap();
        // girt does not report garbage files.
        let expected: String = expected
            .lines()
            .filter(|line| !line.starts_with("garbage:") && !line.starts_with("size-garbage:"))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(
            String::from_utf8(girt.stdout).unwrap(),
            expected,
            "girt --git {girt_args:?}: {}",
            String::from_utf8_lossy(&girt.stderr)
        );
    };
    compare(
        &["object", "list"],
        &["cat-file", "--batch-all-objects", "--batch-check"],
    );
    // After listing, which Git refuses for them: object-named directories and symlinks are garbage
    // to Git, not loose objects.
    fs::create_dir_all(objects.join("ef/0000000000000000000000000000000000000a")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        "missing",
        objects.join("ef/0000000000000000000000000000000000000b"),
    )
    .unwrap();

    compare(&["object", "count"], &["count-objects", "-v"]);

    let packed_only = run(
        Command::new(env!("CARGO_BIN_EXE_girt")).args(["object", "list", "--packed"]),
        scratch.path(),
        &repository,
        None,
    );
    let listed = String::from_utf8(packed_only.stdout).unwrap();
    assert!(listed.contains(packed.trim()));
    assert!(!listed.contains(loose.trim()));
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

#[test]
fn abbreviation_config_and_short_revisions_match_git() {
    let scratch = tempfile::tempdir().unwrap();
    let repository = scratch.path().join("repo");
    fixture(scratch.path(), &repository);
    let output = run(
        Command::new("git").args(["rev-parse", "HEAD"]),
        scratch.path(),
        &repository,
        None,
    );
    let id = String::from_utf8(output.stdout).unwrap();
    let short = &id[..5];
    let cases: &[(&[&str], &[&str])] = &[
        (
            &["log", "--oneline", "--limit", "1"],
            &["log", "--oneline", "-1"],
        ),
        (&["ref", "log", "main"], &["reflog", "show", "main"]),
        (
            &["diff", "--patch", "first", "main"],
            &["diff", "--no-renames", "first", "main"],
        ),
        (&["worktree", "list"], &["worktree", "list"]),
        (&["object", "show", short], &["cat-file", "-p", short]),
    ];
    for value in ["auto", "4", "9", "40", "64", "7k", "false", "no", "off", ""] {
        assert!(
            run(
                Command::new("git").args(["config", "core.abbrev", value]),
                scratch.path(),
                &repository,
                None
            )
            .status
            .success()
        );
        for (girt_args, git_args) in cases {
            let actual = run(
                Command::new(env!("CARGO_BIN_EXE_girt"))
                    .arg("--git")
                    .args(*girt_args),
                scratch.path(),
                &repository,
                None,
            );
            let expected = run(
                Command::new("git").args(*git_args),
                scratch.path(),
                &repository,
                None,
            );
            assert!(
                actual.status.success(),
                "{value} {girt_args:?}: {}",
                String::from_utf8_lossy(&actual.stderr)
            );
            assert_eq!(actual.stdout, expected.stdout, "{value} {girt_args:?}");
        }
    }
    for value in ["0", "3", "true", "-1", "garbage"] {
        assert!(
            run(
                Command::new("git").args(["config", "core.abbrev", value]),
                scratch.path(),
                &repository,
                None
            )
            .status
            .success()
        );
        let actual = run(
            Command::new(env!("CARGO_BIN_EXE_girt")).args(["--git", "log", "--oneline"]),
            scratch.path(),
            &repository,
            None,
        );
        assert!(!actual.status.success(), "accepted {value}");
    }
}

#[test]
fn worktree_columns_align_when_one_id_needs_more_digits() {
    let scratch = tempfile::tempdir().unwrap();
    let repository = scratch.path().join("repo");
    fixture(scratch.path(), &repository);
    let linked = scratch.path().join("linked");
    assert!(
        run(
            Command::new("git")
                .args(["worktree", "add", "--detach"])
                .arg(&linked)
                .arg("first"),
            scratch.path(),
            &repository,
            None
        )
        .status
        .success()
    );
    let output = run(
        Command::new("git").args(["rev-parse", "HEAD"]),
        scratch.path(),
        &repository,
        None,
    );
    let head = String::from_utf8(output.stdout).unwrap();
    let repo = girt::Repository::open(&repository).unwrap();
    let prefix = girt::ObjectIdPrefix::from_hex(repo.object_format(), &head[..4]).unwrap();
    let bytes = (0u32..2_000_000)
        .find(|i| {
            prefix.matches(girt::ObjectId::for_blob(
                repo.object_format(),
                &i.to_le_bytes(),
            ))
        })
        .unwrap()
        .to_le_bytes();
    repo.loose_objects().write_blob(&bytes).unwrap();
    assert!(
        run(
            Command::new("git").args(["config", "core.abbrev", "4"]),
            scratch.path(),
            &repository,
            None
        )
        .status
        .success()
    );
    let actual = run(
        Command::new(env!("CARGO_BIN_EXE_girt")).args(["--git", "worktree", "list"]),
        scratch.path(),
        &repository,
        None,
    );
    let expected = run(
        Command::new("git").args(["worktree", "list"]),
        scratch.path(),
        &repository,
        None,
    );
    assert!(
        actual.status.success(),
        "{}",
        String::from_utf8_lossy(&actual.stderr)
    );
    assert_eq!(
        String::from_utf8(actual.stdout).unwrap(),
        String::from_utf8(expected.stdout).unwrap()
    );
}

/// Obstructed tracked paths report `M` for a special file at the leaf and `D` for a directory at
/// the leaf or a file blocking an ancestor, as `git status --porcelain` does.
#[cfg(unix)]
#[test]
fn status_classifies_obstructions_like_git() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path();
    let repository = home.join("repo");
    fs::create_dir_all(repository.join("blocked")).unwrap();
    let git = |args: &[&str]| run(Command::new("git").args(args), home, &repository, None);
    git(&["init", "--quiet"]);
    for path in ["fifo", "directory", "blocked/file"] {
        fs::write(repository.join(path), "tracked\n").unwrap();
    }
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "Tracked"]);
    fs::remove_file(repository.join("fifo")).unwrap();
    let fifo = Command::new("mkfifo")
        .arg(repository.join("fifo"))
        .status()
        .unwrap();
    assert!(fifo.success());
    fs::remove_file(repository.join("directory")).unwrap();
    fs::create_dir(repository.join("directory")).unwrap();
    fs::write(repository.join("directory/inner"), "untracked\n").unwrap();
    fs::remove_dir_all(repository.join("blocked")).unwrap();
    fs::write(repository.join("blocked"), "file\n").unwrap();

    let expected = git(&["status", "--porcelain", "-uno"]);
    let girt = run(
        Command::new(env!("CARGO_BIN_EXE_girt")).args(["--git", "status"]),
        home,
        &repository,
        None,
    );

    assert_eq!(
        String::from_utf8_lossy(&girt.stdout),
        String::from_utf8_lossy(&expected.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&expected.stdout),
        " D blocked/file\n D directory\n M fifo\n"
    );
}

/// A raw imported zone survives the Git-style log presentation path.
#[test]
fn log_preserves_noncanonical_zone() {
    assert_log_date_parity("1700000000 +0090");
}

#[test]
fn log_substitutes_epoch_for_malformed_date() {
    assert_log_date_parity("unknown");
}

#[test]
fn log_substitutes_epoch_for_negative_seconds() {
    assert_log_date_parity("-60 +0100");
}

#[test]
fn log_substitutes_epoch_for_explicit_plus_seconds() {
    assert_log_date_parity("+5 +0000");
}

fn assert_log_date_parity(date: &str) {
    let scratch = tempfile::tempdir().unwrap();
    let repository = scratch.path().join("repo");
    fixture(scratch.path(), &repository);
    let tree = run(
        Command::new("git").args(["rev-parse", "HEAD^{tree}"]),
        scratch.path(),
        &repository,
        None,
    );
    let tree = String::from_utf8(tree.stdout).unwrap();
    let raw = format!(
        "tree {}\nauthor A <a@example.com> {date}\ncommitter C <c@example.com> 1700000000 +0000\n\nm\n",
        tree.trim()
    );
    let commit = run(
        Command::new("git").args([
            "hash-object",
            "-t",
            "commit",
            "-w",
            "--literally",
            "--stdin",
        ]),
        scratch.path(),
        &repository,
        Some(&raw),
    );
    assert!(commit.status.success());
    let commit = String::from_utf8(commit.stdout).unwrap();
    let ours = run(
        Command::new(env!("CARGO_BIN_EXE_girt")).args(["--git", "log", commit.trim()]),
        scratch.path(),
        &repository,
        None,
    );
    let expected = run(
        Command::new("git").args(["log", commit.trim()]),
        scratch.path(),
        &repository,
        None,
    );
    assert!(
        ours.status.success(),
        "{}",
        String::from_utf8_lossy(&ours.stderr)
    );
    assert!(expected.status.success());
    assert_eq!(ours.stdout, expected.stdout);
}
