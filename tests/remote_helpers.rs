//! Opt-in `git-remote-<name>` helpers compared with the Git CLI. The helper is a disposable shell
//! script that records its arguments and commands and implements only `connect`, by running the
//! requested Git service on the repository the URL names.
#![cfg(unix)]
#[allow(dead_code)]
#[path = "support/pack_git.rs"]
mod pack_git;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::Repository;
use girt::workflow::{Environment, RemoteHelpers, TransferCallbacks, TransferError};
use pack_git::{Fixture, git};
use rstest::rstest;

struct Helpers(RemoteHelpers);

impl TransferCallbacks for Helpers {
    fn remote_helpers(&self) -> RemoteHelpers {
        self.0.clone()
    }
}

// `bin/git-remote-mem` logs to `log` and serves `mem://<path>`; `connect` is offered unless
// `connect` is false, in which case the helper offers only `fetch`.
fn helper(root: &Path, connect: bool) {
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let capability = if connect { "connect" } else { "fetch" };
    let script = format!(
        "#!/bin/sh\nprintf 'argv %s %s\\n' \"$1\" \"$2\" >> '{log}'\n\
         [ -n \"$GIT_DIR\" ] && echo 'env GIT_DIR' >> '{log}'\n\
         while read line; do\n  printf 'cmd %s\\n' \"$line\" >> '{log}'\n  case \"$line\" in\n\
         \x20   capabilities) printf '{capability}\\n\\n';;\n\
         \x20   'connect '*) printf '\\n'; exec git \"${{line#connect git-}}\" \"${{2#mem://}}\";;\n\
         \x20   '') exit 0;;\n  esac\ndone\n",
        log = root.join("log").display(),
    );
    let path = root.join("bin/git-remote-mem");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn take_log(root: &Path) -> Vec<String> {
    let log = std::fs::read_to_string(root.join("log")).unwrap_or_default();
    let _ = std::fs::remove_file(root.join("log"));
    log.lines().map(str::to_owned).collect()
}

fn path_with(root: &Path) -> String {
    format!(
        "{}:/usr/bin:/bin:/opt/homebrew/bin",
        root.join("bin").display()
    )
}

fn git_cli(directory: &Path, root: &Path, args: &[&str]) -> bool {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .env("PATH", path_with(root))
        .env("HOME", directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

// A repository whose `origin` is `mem://<fixture>`, allowed by `protocol.mem.allow`.
fn local(fixture: &Fixture, root: &Path, name: &str) -> std::path::PathBuf {
    let directory = root.join(name);
    std::fs::create_dir(&directory).unwrap();
    git(&directory, &["init", "--quiet", "."], b"");
    let url = format!("mem://{}", fixture.root.path().display());
    git(&directory, &["remote", "add", "origin", &url], b"");
    git(&directory, &["config", "protocol.mem.allow", "always"], b"");
    directory
}

fn environment(root: &Path, directory: &Path) -> Environment {
    Environment::default()
        .with("PATH", path_with(root))
        .with("HOME", directory)
}

fn approved() -> Helpers {
    Helpers(RemoteHelpers::Run(vec!["mem".to_owned()]))
}

#[rstest]
#[case::listing(&["ls-remote", "origin"])]
#[case::fetch(&["fetch", "origin", "refs/heads/*:refs/remotes/origin/*"])]
fn helper_is_run_like_git(#[case] git_args: &[&str]) {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    helper(root, true);
    let git_directory = local(&fixture, root, "git");
    assert!(git_cli(&git_directory, root, git_args));
    let git_log = take_log(root);

    let directory = local(&fixture, root, "girt");
    let repository = Repository::open(&directory).unwrap();
    let environment = environment(root, &directory);
    if git_args[0] == "ls-remote" {
        girt::workflow::remote_refs(&repository, "origin", &environment, &mut approved()).unwrap();
    } else {
        girt::workflow::fetch(
            &repository,
            "origin",
            &girt::workflow::FetchOptions::new(["refs/heads/*:refs/remotes/origin/*"]),
            &environment,
            &mut approved(),
        )
        .unwrap();
        let refs = |directory: &Path| {
            git(
                directory,
                &[
                    "for-each-ref",
                    "--format=%(objectname) %(refname)",
                    "refs/remotes",
                ],
                b"",
            )
        };
        assert_eq!(refs(&directory), refs(&git_directory));
    }
    assert_eq!(take_log(root), git_log);
}

#[test]
fn push_runs_receive_pack_through_the_helper() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    helper(root, true);
    let directory = local(&fixture, root, "girt");
    // The pushing repository needs the object; fetch it first, without the helper.
    let source = fixture.root.path().to_str().unwrap();
    git(&directory, &["fetch", "--quiet", source, "HEAD"], b"");
    let main = String::from_utf8(git(fixture.root.path(), &["rev-parse", "HEAD"], b"")).unwrap();
    let repository = Repository::open(&directory).unwrap();
    let report = girt::workflow::push(
        &repository,
        "origin",
        vec![girt::push::PushCommand::new(
            girt::refs::RefName::new("refs/heads/copy").unwrap(),
            None,
            main.trim().parse().unwrap(),
        )],
        &girt::workflow::PushOptions::default(),
        &environment(root, &directory),
        &mut approved(),
    );
    // The new branch names an object the receiver already has, so no pack is needed.
    let report = report.unwrap();
    assert!(report.all_succeeded(), "{report:?}");
    assert!(take_log(root).contains(&"cmd connect git-receive-pack".to_owned()));
    let copied = git(fixture.root.path(), &["rev-parse", "refs/heads/copy"], b"");
    assert_eq!(copied, main.as_bytes());
}

#[test]
fn helpers_are_refused_by_default() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let scratch = tempfile::tempdir().unwrap();
    helper(scratch.path(), true);
    let directory = local(&fixture, scratch.path(), "girt");
    let repository = Repository::open(&directory).unwrap();
    let result = girt::workflow::remote_refs(
        &repository,
        "origin",
        &environment(scratch.path(), &directory),
        &mut Helpers(RemoteHelpers::Refuse),
    );
    assert!(matches!(
        result,
        Err(TransferError::UnsupportedTransport(_))
    ));
    assert!(
        take_log(scratch.path()).is_empty(),
        "the helper must not run"
    );
}

#[test]
fn helpers_without_connect_are_refused() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let scratch = tempfile::tempdir().unwrap();
    helper(scratch.path(), false);
    let directory = local(&fixture, scratch.path(), "girt");
    let repository = Repository::open(&directory).unwrap();
    let result = girt::workflow::remote_refs(
        &repository,
        "origin",
        &environment(scratch.path(), &directory),
        &mut approved(),
    );
    assert!(
        matches!(result, Err(TransferError::Transport(_))),
        "{result:?}"
    );
}

#[test]
fn missing_helper_is_a_configuration_error() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let scratch = tempfile::tempdir().unwrap();
    let directory = local(&fixture, scratch.path(), "girt");
    let repository = Repository::open(&directory).unwrap();
    let result = girt::workflow::remote_refs(
        &repository,
        "origin",
        &environment(scratch.path(), &directory),
        &mut approved(),
    );
    assert!(
        matches!(result, Err(TransferError::Configuration(_))),
        "{result:?}"
    );
}
