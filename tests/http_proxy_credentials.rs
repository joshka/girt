//! Proxy credentials compared with the Git CLI. The loopback fixture acts as an HTTP proxy that
//! answers 407 until it sees `pu:pp`; credential helpers and askpass programs are disposable shell
//! scripts that record what they are asked.
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

fn executable(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// The helper records each action and the `username`/`host` it was given, and answers `get` with
// `password`. A `password` of `user:<name>/<password>` also answers with a username.
fn scripts(root: &Path, password: &str) {
    let answer = match password.strip_prefix("user:") {
        Some(rest) => {
            let (user, password) = rest.split_once('/').unwrap();
            format!("username={user}\\npassword={password}")
        }
        None => format!("password={password}"),
    };
    let log = root.join("log");
    executable(
        &root.join("helper"),
        &format!(
            "input=$(cat)\nprintf '%s %s\\n' \"$1\" \"$(printf '%s\\n' \"$input\" | grep -E '^(username|host)=' | tr '\\n' ' ')\" >> '{}'\n\
             [ \"$1\" = get ] && printf '{answer}\\n'\nexit 0",
            log.display()
        ),
    );
    executable(
        &root.join("askpass"),
        &format!(
            "printf 'ask %s\\n' \"$1\" >> '{}'\necho {password}",
            log.display()
        ),
    );
}

fn take_log(root: &Path, port: &str) -> Vec<String> {
    let log = std::fs::read_to_string(root.join("log")).unwrap_or_default();
    let _ = std::fs::remove_file(root.join("log"));
    log.lines()
        .map(|line| line.trim_end().replace(port, "PORT"))
        .collect()
}

// `{proxy}` becomes the fixture's `host:port`.
#[rstest]
#[case::helper("http://pu@{proxy}", true, false, "pp", true)]
#[case::askpass("http://pu@{proxy}", false, true, "pp", true)]
// Like Git, a helper's username replaces the one the proxy URL names.
#[case::helper_username_replaces_url_user("http://pu@{proxy}", true, false, "user:other/pp", false)]
#[case::wrong_password("http://pu@{proxy}", true, false, "nope", false)]
#[case::no_user_asks_nobody("http://{proxy}", true, true, "pp", false)]
fn proxy_credentials_match_git(
    #[case] proxy: &str,
    #[case] helper: bool,
    #[case] askpass: bool,
    #[case] password: &str,
    #[case] succeeds: bool,
) {
    let scratch = tempfile::tempdir().unwrap();
    scripts(scratch.path(), password);
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let server = Server::new(source.root.path(), "proxy-auth", "", None);
    let address = server
        .url
        .trim_start_matches("http://")
        .trim_end_matches("/repo")
        .to_owned();
    let port = address.rsplit(':').next().unwrap().to_owned();
    let proxy = proxy.replace("{proxy}", &address);

    let local = tempfile::tempdir().unwrap();
    git(local.path(), &["init", "--quiet", "."], b"");
    git(
        local.path(),
        &["remote", "add", "origin", "http://example.invalid/repo"],
        b"",
    );
    git(local.path(), &["config", "http.proxy", &proxy], b"");
    let helper_path = scratch.path().join("helper");
    let helper_value = if helper {
        helper_path.to_str().unwrap()
    } else {
        ""
    };
    git(
        local.path(),
        &["config", "credential.helper", helper_value],
        b"",
    );
    let askpass_path = scratch.path().join("askpass");

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
        .args(["ls-remote", "origin"])
        .stdin(Stdio::null());
    let mut environment = Environment::default()
        .with("HOME", local.path())
        .with("PATH", "/usr/bin:/bin");
    if askpass {
        command.env("GIT_ASKPASS", &askpass_path);
        environment = environment.with("GIT_ASKPASS", &askpass_path);
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.success(),
        succeeds,
        "git: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let git_log = take_log(scratch.path(), &port);

    let repository = Repository::open(local.path()).unwrap();
    let result = girt::workflow::remote_head(&repository, "origin", &environment, &mut NoCallbacks);
    assert_eq!(result.is_ok(), succeeds, "girt: {:?}", result.err());
    assert_eq!(take_log(scratch.path(), &port), git_log);
}
