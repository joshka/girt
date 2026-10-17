//! Every girt command reads Git's system, global and environment configuration the way `git` does.
//!
//! Each test isolates both tools to a scratch `HOME` and a selected global file, so the machine's
//! own configuration never leaks in.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

/// A scratch home with a global config file and an empty worktree repository at `repo`.
struct Scratch {
    _root: tempfile::TempDir,
    home: std::path::PathBuf,
    repo: std::path::PathBuf,
}

impl Scratch {
    fn new(global: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let repo = root.path().join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&repo).unwrap();
        fs::write(home.join(".gitconfig"), global).unwrap();
        Self {
            _root: root,
            home,
            repo,
        }
    }

    fn girt(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.run(Command::new(env!("CARGO_BIN_EXE_girt")).args(args), env)
    }

    fn git(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.run(Command::new("git").args(args), env)
    }

    fn run(&self, command: &mut Command, env: &[(&str, &str)]) -> Output {
        self.run_in(&self.repo, command, env)
    }

    /// Runs in `directory` with only `PATH`, the scratch `HOME` and the given variables.
    fn run_in(&self, directory: &Path, command: &mut Command, env: &[(&str, &str)]) -> Output {
        command
            .current_dir(directory)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(env.iter().copied());
        if let Some(system) = std::env::var_os("SYSTEMROOT") {
            command.env("SYSTEMROOT", system);
        }
        command.output().unwrap()
    }
}

fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn commit(scratch: &Scratch) {
    let identity = [
        ("GIT_AUTHOR_NAME", "Fixture"),
        ("GIT_AUTHOR_EMAIL", "fixture@example.com"),
        ("GIT_COMMITTER_NAME", "Fixture"),
        ("GIT_COMMITTER_EMAIL", "fixture@example.com"),
    ];
    success(scratch.git(&["init", "--quiet", "--initial-branch=main"], &[]));
    fs::write(scratch.repo.join("tracked.txt"), "tracked\n").unwrap();
    success(scratch.git(&["add", "tracked.txt"], &identity));
    success(scratch.git(&["commit", "--quiet", "-m", "initial"], &identity));
}

#[test]
fn global_excludes_file_reaches_ignore_handling() {
    let scratch = Scratch::new("[core]\n\texcludesFile = ~/global-ignore\n");
    fs::write(scratch.home.join("global-ignore"), "*.secret\n").unwrap();
    commit(&scratch);
    fs::write(scratch.repo.join("key.secret"), "hidden\n").unwrap();

    let girt = scratch.girt(&["--git", "ignore", "check", "key.secret"], &[]);
    let git = scratch.git(&["check-ignore", "-v", "key.secret"], &[]);
    assert_eq!(success(girt), success(git));

    let status = success(scratch.girt(&["--git", "status", "--untracked"], &[]));
    assert!(
        !String::from_utf8_lossy(&status).contains("key.secret"),
        "ignored by the global excludes file: {}",
        String::from_utf8_lossy(&status)
    );
}

#[test]
fn committer_identity_uses_global_and_environment_config() {
    let scratch = Scratch::new("[user]\n\tname = Global Person\n\temail = global@example.com\n");
    commit(&scratch);

    success(scratch.girt(
        &["ref", "set", "refs/heads/global", "HEAD", "-m", "global"],
        &[],
    ));
    let reflog = fs::read_to_string(scratch.repo.join(".git/logs/refs/heads/global")).unwrap();
    assert!(
        reflog.contains(" Global Person <global@example.com> "),
        "{reflog}"
    );

    // Runtime pairs (as `git -c` would pass them to a child) override the global file.
    let pairs = [
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "user.name"),
        ("GIT_CONFIG_VALUE_0", "Pair Person"),
    ];
    success(scratch.girt(
        &["ref", "set", "refs/heads/pair", "HEAD", "-m", "pair"],
        &pairs,
    ));
    let reflog = fs::read_to_string(scratch.repo.join(".git/logs/refs/heads/pair")).unwrap();
    assert!(
        reflog.contains(" Pair Person <global@example.com> "),
        "{reflog}"
    );
}

// Git logs the branch an unlogged alias names; repeating the same value logs nothing.
#[test]
fn ref_set_through_alias_logs_the_branch_once() {
    let scratch = Scratch::new("[user]\n\tname = Global Person\n\temail = global@example.com\n");
    commit(&scratch);
    success(scratch.git(&["commit", "--quiet", "--allow-empty", "-m", "second"], &[]));
    success(scratch.git(
        &["symbolic-ref", "refs/custom/alias", "refs/heads/main"],
        &[],
    ));
    let log = || fs::read_to_string(scratch.repo.join(".git/logs/refs/heads/main")).unwrap();
    let before = log().lines().count();

    success(scratch.girt(
        &["ref", "set", "refs/custom/alias", "HEAD~1", "-m", "back"],
        &[],
    ));
    assert_eq!(log().lines().count(), before + 1);
    assert!(log().ends_with("\tback\n"), "{}", log());
    success(scratch.girt(
        &["ref", "set", "refs/custom/alias", "HEAD~0", "-m", "again"],
        &[],
    ));
    assert_eq!(log().lines().count(), before + 1);
}

#[test]
fn init_uses_global_default_branch() {
    let scratch = Scratch::new("[init]\n\tdefaultBranch = trunk\n");
    let head = |path: &Path| fs::read_to_string(path.join("HEAD")).unwrap();

    success(scratch.girt(&["init", "--bare", "from-girt.git"], &[]));
    success(scratch.git(&["init", "--quiet", "--bare", "from-git.git"], &[]));
    assert_eq!(
        head(&scratch.repo.join("from-girt.git")),
        "ref: refs/heads/trunk\n"
    );
    assert_eq!(
        head(&scratch.repo.join("from-girt.git")),
        head(&scratch.repo.join("from-git.git"))
    );

    // An explicit branch still wins.
    success(scratch.girt(&["init", "--bare", "-b", "chosen", "chosen.git"], &[]));
    assert_eq!(
        head(&scratch.repo.join("chosen.git")),
        "ref: refs/heads/chosen\n"
    );
}

#[test]
fn explicit_initial_branch_overrides_invalid_default_branch() {
    let scratch = Scratch::new("[init]\n\tdefaultBranch = bad name\n");
    let girt = scratch.girt(&["init", "--bare", "-b", "chosen", "girt.git"], &[]);
    let git = scratch.git(
        &["init", "--quiet", "--bare", "-b", "chosen", "git.git"],
        &[],
    );
    assert!(
        girt.status.success(),
        "{}",
        String::from_utf8_lossy(&girt.stderr)
    );
    assert!(git.status.success());

    // Without `-b`, both refuse the invalid default.
    assert!(
        !scratch
            .girt(&["init", "--bare", "girt-default.git"], &[])
            .status
            .success()
    );
    assert!(
        !scratch
            .git(&["init", "--quiet", "--bare", "git-default.git"], &[])
            .status
            .success()
    );
}

/// Inside a working tree, Git reads a relative `GIT_CONFIG_GLOBAL` from the top of the tree.
#[test]
fn relative_global_selector_resolves_from_the_top_of_the_worktree() {
    let scratch = Scratch::new("");
    commit(&scratch);
    fs::write(scratch.repo.join("selected"), "[demo]\n\tvalue = top\n").unwrap();
    let sub = scratch.repo.join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("selected"), "[demo]\n\tvalue = sub\n").unwrap();

    let env = [("GIT_CONFIG_GLOBAL", "selected")];
    let girt = scratch.run_in(
        &sub,
        Command::new(env!("CARGO_BIN_EXE_girt")).args(["config", "get", "demo.value"]),
        &env,
    );
    let git = scratch.run_in(
        &sub,
        Command::new("git").args(["config", "--get", "demo.value"]),
        &env,
    );
    assert_eq!(success(girt), b"top\n");
    assert_eq!(success(git), b"top\n");
}

#[cfg(unix)]
#[test]
fn unreadable_global_file_is_skipped() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new("[demo]\n\tvalue = unreadable\n");
    commit(&scratch);
    let global = scratch.home.join(".gitconfig");
    fs::set_permissions(&global, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&global).is_ok() {
        return; // Running as a user that bypasses permissions, such as root.
    }
    success(scratch.girt(&["status"], &[]));
    success(scratch.git(&["status", "--porcelain"], &[]));
}
