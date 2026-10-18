//! `GIT_ASKPASS`, `core.askPass` and `SSH_ASKPASS` compared with the Git CLI against a loopback
//! smart HTTP server that requires Basic authentication. Askpass programs are disposable shell
//! scripts that record each question; no real credential helper or terminal is used.
#![cfg(all(feature = "http", unix))]
#[allow(dead_code)]
#[path = "support/http_git.rs"]
mod http_git;
#[allow(dead_code)]
#[path = "support/pack_git.rs"]
mod pack_git;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::Repository;
use girt::workflow::{Environment, NoCallbacks};
use http_git::Server;
use pack_git::{Fixture, git};
use rstest::rstest;

// Writes an askpass script that logs `<name>:<question>` and answers `u`/`p`, or `answer`.
fn script(root: &Path, name: &str, answer: &str) {
    let body = match answer {
        "fail" => "exit 3".to_owned(),
        "empty" => String::new(),
        _ => "case \"$1\" in Username*) echo u;; *) echo p;; esac".to_owned(),
    };
    let path = root.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s:%s\\n' {name} \"$1\" >> \"{}/log\"\n{body}\n",
            root.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn take_log(root: &Path, port: &str) -> Vec<String> {
    let log = std::fs::read_to_string(root.join("log")).unwrap_or_default();
    let _ = std::fs::remove_file(root.join("log"));
    log.lines().map(|line| line.replace(port, "PORT")).collect()
}

// `{askpass}` in values becomes the script directory.
#[rstest]
#[case::git_askpass_first(&[("GIT_ASKPASS", "{askpass}/a"), ("SSH_ASKPASS", "{askpass}/c")], Some("{askpass}/b"), None, true)]
#[case::config_before_ssh(&[("SSH_ASKPASS", "{askpass}/c")], Some("{askpass}/b"), None, true)]
#[case::ssh_askpass(&[("SSH_ASKPASS", "{askpass}/c")], None, None, true)]
#[case::empty_git_askpass_disables(&[("GIT_ASKPASS", "")], Some("{askpass}/b"), None, false)]
#[case::empty_config_disables(&[("SSH_ASKPASS", "{askpass}/c")], Some(""), None, false)]
#[case::empty_answer(&[("GIT_ASKPASS", "{askpass}/empty")], None, None, false)]
#[case::failing_program(&[("GIT_ASKPASS", "{askpass}/fail")], None, None, false)]
#[case::configured_username(&[("GIT_ASKPASS", "{askpass}/a")], None, Some("u"), true)]
#[case::http_path(&[("GIT_ASKPASS", "{askpass}/a")], None, Some("path"), true)]
fn askpass_matches_git(
    #[case] variables: &[(&str, &str)],
    #[case] core_askpass: Option<&str>,
    // A username for `credential.username`, or `path` for `credential.useHttpPath`.
    #[case] username: Option<&str>,
    #[case] succeeds: bool,
) {
    let scripts = tempfile::tempdir().unwrap();
    for (name, answer) in [
        ("a", ""),
        ("b", ""),
        ("c", ""),
        ("empty", "empty"),
        ("fail", "fail"),
    ] {
        script(scripts.path(), name, answer);
    }
    let expand = |value: &str| value.replace("{askpass}", scripts.path().to_str().unwrap());
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let server = Server::new(source.root.path(), "", "Basic dTpw", None);
    let port = server
        .url
        .split(':')
        .nth(2)
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .to_owned();

    let local = tempfile::tempdir().unwrap();
    git(local.path(), &["init", "--quiet", "."], b"");
    git(local.path(), &["remote", "add", "origin", &server.url], b"");
    if let Some(program) = core_askpass {
        git(
            local.path(),
            &["config", "core.askPass", &expand(program)],
            b"",
        );
    }
    match username {
        Some("path") => git(
            local.path(),
            &["config", "credential.useHttpPath", "true"],
            b"",
        ),
        Some(username) => git(
            local.path(),
            &["config", "credential.username", username],
            b"",
        ),
        None => Vec::new(),
    };

    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        let key_text = key.to_string_lossy().to_ascii_lowercase();
        if key_text.starts_with("git_") || key_text.ends_with("_proxy") || key_text == "ssh_askpass"
        {
            command.env_remove(key);
        }
    }
    command
        .current_dir(local.path())
        .env("HOME", local.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", local.path().join("absent-config"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("SSH_AUTH_SOCK")
        .args(["-c", "credential.helper=", "ls-remote", "origin"])
        .stdin(Stdio::null());
    let mut environment = Environment::default()
        .with("HOME", local.path())
        .with("PATH", "/usr/bin:/bin");
    for (name, value) in variables {
        command.env(name, expand(value));
        environment = environment.with(*name, expand(value));
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.success(),
        succeeds,
        "git: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let git_questions = take_log(scripts.path(), &port);

    let repository = Repository::open(local.path()).unwrap();
    let result = girt::workflow::remote_head(&repository, "origin", &environment, &mut NoCallbacks);
    assert_eq!(result.is_ok(), succeeds, "girt: {:?}", result.err());
    assert_eq!(take_log(scripts.path(), &port), git_questions);
}
