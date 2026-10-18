//! `girt::hooks` discovery and execution compared with where and how the `git` executable runs
//! the same hook scripts.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::AtomicBool;

use girt::Repository;
use girt::hooks::{Error, Found, Hooks, Invocation, Outcome, Policy};
use girt::transport::TransportControl;
use girt::workflow::Environment;

fn run_git(directory: &Path, home: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Hook Author")
        .env("GIT_AUTHOR_EMAIL", "hook@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Hook Author")
        .env("GIT_COMMITTER_EMAIL", "hook@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn git(directory: &Path, home: &Path, args: &[&str]) {
    let output = run_git(directory, home, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Writes a hook that appends its working directory, `GIT_PREFIX`, arguments and input to `log`.
fn recording_hook(path: &Path, log: &Path, exit: i32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\n{{ echo \"cwd=$(pwd -P) prefix=[$GIT_PREFIX] args=$*\"; cat; }} >> '{}'\nexit {exit}\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn take_log(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let _ = std::fs::remove_file(log);
    text
}

fn environment(current: &Path) -> Environment {
    [("PATH", "/usr/bin:/bin")]
        .into_iter()
        .collect::<Environment>()
        .with_current_dir(current)
}

fn run(
    hooks: &Hooks,
    name: &str,
    invocation: &Invocation,
    policy: Policy,
) -> Result<Outcome, Error> {
    let cancel = AtomicBool::new(false);
    hooks.run(name, invocation, policy, TransportControl::new(&cancel))
}

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        git(
            &repo,
            root.path(),
            &["init", "-q", "-b", "main", "--template="],
        );
        git(
            &repo,
            root.path(),
            &["commit", "-q", "--allow-empty", "-m", "base"],
        );
        Self { root }
    }

    fn home(&self) -> &Path {
        self.root.path()
    }

    fn repo(&self) -> std::path::PathBuf {
        self.root.path().join("repo").canonicalize().unwrap()
    }

    fn log(&self) -> std::path::PathBuf {
        self.root.path().join("log")
    }

    /// The line Git's `pre-commit` writes when committing from `directory`.
    fn git_pre_commit(&self, directory: &Path) -> String {
        git(
            directory,
            self.home(),
            &["commit", "-q", "--allow-empty", "-m", "x"],
        );
        take_log(&self.log())
    }

    fn girt_pre_commit(&self, current: &Path) -> String {
        let repository = Repository::discover(current).unwrap();
        let hooks = Hooks::new(&repository, &environment(current)).unwrap();
        assert!(
            run(&hooks, "pre-commit", &Invocation::new(), Policy::Run)
                .unwrap()
                .allows()
        );
        take_log(&self.log())
    }
}

#[test]
fn default_and_configured_directories_match_git() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let log = fixture.log();

    recording_hook(&repo.join(".git/hooks/pre-commit"), &log, 0);
    for current in [repo.clone(), repo.join("sub")] {
        let expected = fixture.git_pre_commit(&current);
        assert!(!expected.is_empty());
        assert_eq!(
            fixture.girt_pre_commit(&current),
            expected,
            "from {}",
            current.display()
        );
    }

    // A relative core.hooksPath resolves against the top of the working tree, even from below.
    git(
        &repo,
        fixture.home(),
        &["config", "core.hooksPath", "../shared-hooks"],
    );
    recording_hook(&fixture.home().join("shared-hooks/pre-commit"), &log, 0);
    let expected = fixture.git_pre_commit(&repo.join("sub"));
    assert!(!expected.is_empty());
    assert_eq!(fixture.girt_pre_commit(&repo.join("sub")), expected);
}

#[test]
fn linked_worktrees_share_the_common_hooks_directory() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    git(
        &repo,
        fixture.home(),
        &["worktree", "add", "-q", "../linked"],
    );
    let linked = fixture.home().join("linked").canonicalize().unwrap();
    recording_hook(&repo.join(".git/hooks/pre-commit"), &fixture.log(), 0);

    let expected = fixture.git_pre_commit(&linked);
    assert!(expected.contains(&linked.display().to_string()));
    assert_eq!(fixture.girt_pre_commit(&linked), expected);
}

#[test]
fn bare_repositories_run_hooks_in_the_git_directory() {
    let fixture = Fixture::new();
    let bare = fixture.home().join("bare.git");
    git(
        fixture.home(),
        fixture.home(),
        &["init", "-q", "--bare", "bare.git"],
    );
    let bare = bare.canonicalize().unwrap();
    recording_hook(&bare.join("hooks/post-update"), &fixture.log(), 0);
    git(
        &fixture.repo(),
        fixture.home(),
        &["push", "-q", &bare.display().to_string(), "main"],
    );
    let expected = take_log(&fixture.log());

    let repository = Repository::open(&bare).unwrap();
    let hooks = Hooks::new(&repository, &environment(&bare)).unwrap();
    let invocation = Invocation::new().with_argument("refs/heads/main");
    run(&hooks, "post-update", &invocation, Policy::Run).unwrap();
    assert_eq!(take_log(&fixture.log()), expected);
}

#[test]
fn non_executable_hooks_are_skipped_as_git_skips_them() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let hook = repo.join(".git/hooks/pre-commit");
    recording_hook(&hook, &fixture.log(), 1);
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o644)).unwrap();

    let output = run_git(
        &repo,
        fixture.home(),
        &["commit", "-q", "--allow-empty", "-m", "x"],
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not set as executable"));

    let repository = Repository::open(&repo).unwrap();
    let hooks = Hooks::new(&repository, &environment(&repo)).unwrap();
    assert_eq!(hooks.find("pre-commit"), Found::NotExecutable(hook.clone()));
    let outcome = run(&hooks, "pre-commit", &Invocation::new(), Policy::Run).unwrap();
    assert!(matches!(outcome, Outcome::NotExecutable(path) if path == hook));
    assert!(matches!(
        run(&hooks, "commit-msg", &Invocation::new(), Policy::Refuse).unwrap(),
        Outcome::Absent
    ));
}

#[test]
fn policies_exit_codes_input_and_environment() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let repository = Repository::open(&repo).unwrap();
    let hooks_dir = repo.join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let write = |name: &str, body: &str| {
        let path = hooks_dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    write(
        "pre-push",
        "echo out; echo err >&2; echo \"$1 $2 ${EXTRA-unset} ${HOME-unset}\"; exit 7",
    );
    write("post-checkout", "exit 0");
    write("noisy", "head -c 100000 /dev/zero");

    let hooks = Hooks::new(&repository, &environment(&repo)).unwrap();
    let invocation = Invocation::new()
        .with_argument("origin")
        .with_argument("url")
        .with_stdin(vec![b'x'; 1 << 20])
        .with_env("EXTRA", "set");

    assert!(matches!(
        run(&hooks, "pre-push", &invocation, Policy::Ignore).unwrap(),
        Outcome::Ignored(name) if name == "pre-push"
    ));
    assert!(matches!(
        run(&hooks, "pre-push", &invocation, Policy::Refuse),
        Err(Error::Refused { hook, .. }) if hook == "pre-push"
    ));

    let outcome = run(&hooks, "pre-push", &invocation, Policy::Run).unwrap();
    assert!(!outcome.allows());
    let Outcome::Ran(ran) = outcome else {
        panic!("pre-push did not run");
    };
    assert_eq!(ran.code(), Some(7));
    // Unread input is not an error; only the supplied environment reaches the hook.
    assert_eq!(ran.output(), b"out\norigin url set unset\nerr\n");

    let limited = Hooks::new(&repository, &environment(&repo))
        .unwrap()
        .with_output_limit(10);
    let Outcome::Ran(ran) = run(&limited, "noisy", &Invocation::new(), Policy::Run).unwrap() else {
        panic!()
    };
    assert!(ran.success());
    assert_eq!(ran.output().len(), 10);
}

#[test]
fn home_relative_hooks_path_expands_from_the_supplied_environment() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let home = fixture.home().canonicalize().unwrap();
    git(
        &repo,
        fixture.home(),
        &["config", "core.hooksPath", "~/home-hooks"],
    );
    recording_hook(&home.join("home-hooks/pre-commit"), &fixture.log(), 0);
    let output = {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command
            .current_dir(&repo)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", home.join("absent-config"))
            .env("GIT_AUTHOR_NAME", "Hook Author")
            .env("GIT_AUTHOR_EMAIL", "hook@example.com")
            .env("GIT_COMMITTER_NAME", "Hook Author")
            .env("GIT_COMMITTER_EMAIL", "hook@example.com")
            .args(["commit", "-q", "--allow-empty", "-m", "x"])
            .output()
            .unwrap()
    };
    assert!(output.status.success());
    let expected = take_log(&fixture.log());
    assert!(!expected.is_empty());

    let repository = Repository::open(&repo).unwrap();
    let with_home = environment(&repo).with("HOME", &home);
    let hooks = Hooks::new(&repository, &with_home).unwrap();
    assert_eq!(hooks.directory(), home.join("home-hooks"));
    run(&hooks, "pre-commit", &Invocation::new(), Policy::Run).unwrap();
    assert_eq!(take_log(&fixture.log()), expected);

    assert!(matches!(
        Hooks::new(&repository, &environment(&repo)),
        Err(Error::HooksPath(_))
    ));
    git(
        &repo,
        fixture.home(),
        &["config", "core.hooksPath", "~someone/hooks"],
    );
    assert!(matches!(
        Hooks::new(&Repository::open(&repo).unwrap(), &with_home),
        Err(Error::HooksPath(_))
    ));
}
