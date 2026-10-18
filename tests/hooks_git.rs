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

/// A hook that logs its arguments, `GIT_INDEX_FILE`, `GIT_EDITOR` and input, then exits `exit`.
fn logging_hook(path: &Path, name: &str, log: &Path, extra: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\n{{ echo \"{name} cwd=$(pwd -P) index=[$GIT_INDEX_FILE] editor=[$GIT_EDITOR] args=$*\"; if [ {name} = pre-push ]; then cat; fi; }} >> '{}'\n{extra}\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn identity(environment: Environment) -> Environment {
    environment
        .with("GIT_AUTHOR_NAME", "Hook Author")
        .with("GIT_AUTHOR_EMAIL", "hook@example.com")
        .with("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .with("GIT_COMMITTER_NAME", "Hook Author")
        .with("GIT_COMMITTER_EMAIL", "hook@example.com")
        .with("GIT_COMMITTER_DATE", "@1700000000 +0000")
}

#[test]
fn repo_commit_runs_commit_hooks_as_git_commit_does() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let log = fixture.log();
    let hooks = repo.join(".git/hooks");
    for name in ["pre-commit", "prepare-commit-msg", "post-commit"] {
        logging_hook(&hooks.join(name), name, &log, "");
    }
    logging_hook(
        &hooks.join("commit-msg"),
        "commit-msg",
        &log,
        "echo edited >> \"$1\"",
    );

    git(
        &repo,
        fixture.home(),
        &["commit", "-q", "--allow-empty", "-m", "subject"],
    );
    let expected = take_log(&log);
    let git_message = run_git(&repo, fixture.home(), &["log", "-1", "--format=%B"]).stdout;

    let mut handle = girt::Repo::new(Repository::open(&repo).unwrap())
        .with_environment(identity(environment(&repo)))
        .with_hooks(Policy::Run);
    let tree = handle
        .repository()
        .objects()
        .unwrap()
        .read_commit(handle.rev_parse("HEAD").unwrap())
        .unwrap()
        .tree();
    let id = handle.commit(tree, "subject\n").unwrap();
    assert_eq!(take_log(&log), expected);
    let message = handle
        .repository()
        .objects()
        .unwrap()
        .read_commit(id)
        .unwrap()
        .message()
        .to_vec();
    assert_eq!(
        String::from_utf8(message).unwrap() + "\n",
        String::from_utf8(git_message).unwrap()
    );

    // A failing pre-commit stops the commit before anything is published.
    logging_hook(&hooks.join("pre-commit"), "pre-commit", &log, "exit 1");
    let error = handle.commit(tree, "refused\n").unwrap_err();
    assert!(matches!(
        error.downcast_ref::<Error>(),
        Some(Error::Rejected { hook, code: Some(1), .. }) if hook == "pre-commit"
    ));
    assert_eq!(handle.rev_parse("HEAD").unwrap(), id);
    take_log(&log);

    // Ignoring hooks, the default, runs none.
    let mut quiet = girt::Repo::new(Repository::open(&repo).unwrap())
        .with_environment(identity(environment(&repo)));
    quiet.commit(tree, "quiet\n").unwrap();
    assert_eq!(take_log(&log), "");
}

#[test]
fn repo_switch_runs_post_checkout_as_git_switch_does() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let log = fixture.log();
    git(&repo, fixture.home(), &["branch", "topic"]);
    git(
        &repo,
        fixture.home(),
        &["commit", "-q", "--allow-empty", "-m", "second"],
    );
    logging_hook(
        &repo.join(".git/hooks/post-checkout"),
        "post-checkout",
        &log,
        "exit 3",
    );

    let output = run_git(&repo, fixture.home(), &["switch", "-q", "topic"]);
    assert!(!output.status.success());
    let expected = take_log(&log);
    git(
        &repo,
        fixture.home(),
        &["-c", "core.hooksPath=/nonexistent", "switch", "-q", "main"],
    );

    let mut handle = girt::Repo::new(Repository::open(&repo).unwrap())
        .with_environment(identity(environment(&repo)))
        .with_hooks(Policy::Run);
    let error = handle.switch("topic").unwrap_err();
    assert!(matches!(
        error.downcast_ref::<girt::repo::Error>(),
        Some(girt::repo::Error::PostCheckout { .. })
    ));
    assert_eq!(take_log(&log), expected);
    assert_eq!(
        handle.repository().head().unwrap().name,
        girt::refs::RefName::branch("topic").unwrap()
    );
}

#[test]
fn push_runs_pre_push_as_git_push_does() {
    use girt::push::PushCommand;
    use girt::workflow::{NoCallbacks, PushOptions};
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let log = fixture.log();
    let bare = fixture.home().join("remote.git");
    git(
        fixture.home(),
        fixture.home(),
        &["init", "-q", "--bare", "remote.git"],
    );
    git(
        &repo,
        fixture.home(),
        &["remote", "add", "origin", "../remote.git"],
    );
    git(&repo, fixture.home(), &["push", "-q", "origin", "main:old"]);
    let head = run_git(&repo, fixture.home(), &["rev-parse", "HEAD"]).stdout;
    let head: girt::ObjectId = String::from_utf8(head).unwrap().trim().parse().unwrap();
    logging_hook(
        &repo.join(".git/hooks/pre-push"),
        "pre-push",
        &log,
        "exit 0",
    );

    git(
        &repo,
        fixture.home(),
        &[
            "push",
            "-q",
            "origin",
            &format!("{head}:refs/heads/new"),
            ":refs/heads/old",
            "main:refs/heads/main",
        ],
    );
    let expected = take_log(&log);
    git(
        &bare,
        fixture.home(),
        &["update-ref", "-d", "refs/heads/new"],
    );
    git(
        &bare,
        fixture.home(),
        &["update-ref", "-d", "refs/heads/main"],
    );
    git(
        &bare,
        fixture.home(),
        &["update-ref", "refs/heads/old", &head.to_string()],
    );

    let repository = Repository::open(&repo).unwrap();
    let null = girt::ObjectId::null(repository.object_format());
    let name = |name: &str| girt::refs::RefName::branch(name).unwrap();
    let commands = vec![
        PushCommand::new(name("new"), None, head),
        PushCommand::new(name("old"), Some(head), null),
        PushCommand::new(name("main"), None, head),
    ];
    let options = PushOptions::default().with_hooks(Policy::Run);
    let environment = environment(&repo);
    girt::workflow::push(
        &repository,
        "origin",
        commands.clone(),
        &options,
        &environment,
        &mut NoCallbacks,
    )
    .unwrap();
    // Git names `main` by its local reference; girt pushes IDs, so it names the ID instead.
    let expected = expected.replace(
        &format!("refs/heads/main {head}"),
        &format!("{head} {head}"),
    );
    // Line order is not part of the contract; Git's differs.
    let lines = |text: &str| {
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        lines.sort();
        lines
    };
    assert_eq!(lines(&take_log(&log)), lines(&expected));

    // A failing pre-push stops the push before anything is sent.
    logging_hook(
        &repo.join(".git/hooks/pre-push"),
        "pre-push",
        &log,
        "exit 1",
    );
    let again = vec![PushCommand::new(name("blocked"), None, head)];
    let error = girt::workflow::push(
        &repository,
        "origin",
        again,
        &options,
        &environment,
        &mut NoCallbacks,
    )
    .unwrap_err();
    assert!(matches!(error, girt::workflow::TransferError::Hook(_)));
    assert!(
        run_git(
            &bare,
            fixture.home(),
            &["rev-parse", "--verify", "-q", "refs/heads/blocked"]
        )
        .stdout
        .is_empty()
    );
}

#[test]
fn pre_push_sees_the_resolved_url_and_omits_stale_leases_as_git_does() {
    use girt::push::PushCommand;
    use girt::workflow::{NoCallbacks, PushOptions};
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let log = fixture.log();
    git(
        fixture.home(),
        fixture.home(),
        &["init", "-q", "--bare", "remote.git"],
    );
    git(
        &repo,
        fixture.home(),
        &["config", "url.../remote.git.pushInsteadOf", "alias:"],
    );
    // `pushInsteadOf` rewrites `remote.<name>.url` for pushes, never an explicit `pushurl`.
    git(
        &repo,
        fixture.home(),
        &["remote", "add", "origin", "alias:"],
    );
    git(
        &repo,
        fixture.home(),
        &["push", "-q", "origin", "main:taken"],
    );
    git(
        &repo,
        fixture.home(),
        &["commit", "-q", "--allow-empty", "-m", "newer"],
    );
    let head = run_git(&repo, fixture.home(), &["rev-parse", "HEAD"]).stdout;
    let head: girt::ObjectId = String::from_utf8(head).unwrap().trim().parse().unwrap();
    logging_hook(
        &repo.join(".git/hooks/pre-push"),
        "pre-push",
        &log,
        "exit 0",
    );

    // Expect `taken` to be absent although it exists: Git's lease check leaves it out.
    let output = run_git(
        &repo,
        fixture.home(),
        &[
            "push",
            "-q",
            "--force-with-lease=refs/heads/taken:",
            "origin",
            &format!("{head}:refs/heads/taken"),
        ],
    );
    assert!(!output.status.success());
    let expected = take_log(&log);
    assert!(
        expected.contains("args=origin ../remote.git\n"),
        "{expected}"
    );

    let repository = Repository::open(&repo).unwrap();
    let name = girt::refs::RefName::branch("taken").unwrap();
    let commands = vec![PushCommand::new(name, None, head)];
    let options = PushOptions::default().with_hooks(Policy::Run);
    let report = girt::workflow::push(
        &repository,
        "origin",
        commands,
        &options,
        &environment(&repo),
        &mut NoCallbacks,
    )
    .unwrap();
    assert!(!report.all_succeeded());
    assert_eq!(take_log(&log), expected);
}

#[test]
fn post_commit_failures_do_not_fail_a_published_commit() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let hook = repo.join(".git/hooks/post-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    // A missing interpreter makes the hook fail to start at all.
    std::fs::write(&hook, "#!/nonexistent/interpreter\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(repo.join(".git/COMMIT_EDITMSG"), "stale\n").unwrap();

    let mut handle = girt::Repo::new(Repository::open(&repo).unwrap())
        .with_environment(identity(environment(&repo)))
        .with_hooks(Policy::Run);
    let head = handle.rev_parse("HEAD").unwrap();
    let tree = handle
        .repository()
        .objects()
        .unwrap()
        .read_commit(head)
        .unwrap()
        .tree();
    let id = handle.commit(tree, "published\n").unwrap();
    assert_eq!(handle.rev_parse("HEAD").unwrap(), id);
    // Git records every commit message, hooks or not.
    assert_eq!(
        std::fs::read(repo.join(".git/COMMIT_EDITMSG")).unwrap(),
        b"published\n"
    );
}
