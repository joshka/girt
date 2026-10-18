//! `HostKeyPolicy` compared with `git ls-remote` given the same OpenSSH options, against the
//! isolated loopback sshd fixture. Each case works on copies of the fixture's `known_hosts` files.
#![cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
#[allow(dead_code)]
#[path = "support/pack_git.rs"]
mod pack_git;
#[allow(dead_code)]
#[path = "support/ssh_git.rs"]
mod ssh_git;

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use girt::fetch::{self, FetchLimits};
use girt::transport::TransportControl;
use girt::transport::ssh::{HostKeyPolicy, UnknownHost};
use pack_git::Fixture;
use rstest::rstest;
use ssh_git::Server;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

// The OpenSSH options girt documents for `policy`, for Git's `GIT_SSH_COMMAND`.
fn options(policy: &HostKeyPolicy) -> String {
    let unknown = match policy.unknown {
        UnknownHost::Reject => "yes",
        _ => "accept-new",
    };
    let files: Vec<String> = policy
        .files
        .iter()
        .map(|file| format!("\"{}\"", file.display()))
        .collect();
    format!(
        "-o StrictHostKeyChecking={unknown} -o UpdateHostKeys=no -o 'UserKnownHostsFile={}' \
         -o GlobalKnownHostsFile=none",
        files.join(" ")
    )
}

fn git_ls_remote(server: &Server, policy: &HostKeyPolicy, home: &Path) -> bool {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let ssh = format!(
        "ssh -F '{}' {}",
        server.root.join("config").display(),
        options(policy)
    );
    let url = format!(
        "ssh://{}@127.0.0.1:{}{}",
        server.user, server.port, server.repository
    );
    command
        .current_dir(home)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("absent-config"))
        .env("GIT_SSH_COMMAND", ssh)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("SSH_AUTH_SOCK")
        .env_remove("SSH_ASKPASS")
        .args(["ls-remote", &url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn girt_discover(server: &Server, policy: &HostKeyPolicy) -> bool {
    let remote = server.remote("config").with_host_keys(policy).unwrap();
    let cancel = AtomicBool::new(false);
    let control = TransportControl::new(&cancel)
        .with_deadline(Some(Instant::now() + Duration::from_secs(30)));
    runtime()
        .block_on(fetch::discover_ssh(
            &remote,
            FetchLimits::default(),
            control,
        ))
        .is_ok()
}

// `trust` names the fixture file the policy starts from.
#[rstest]
#[case::known_key("known_hosts", UnknownHost::Reject, true)]
#[case::unknown_rejected("empty_hosts", UnknownHost::Reject, false)]
#[case::unknown_accepted("empty_hosts", UnknownHost::AcceptNew, true)]
#[case::changed_key_refused("changed_hosts", UnknownHost::AcceptNew, false)]
fn host_key_policy_matches_git(
    #[case] trust: &str,
    #[case] unknown: UnknownHost,
    #[case] succeeds: bool,
) {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let server = Server::new(fixture.root.path(), "none");
    let scratch = tempfile::tempdir().unwrap();
    let copy = |name: &str| {
        let path = scratch.path().join(name);
        std::fs::copy(server.root.join(trust), &path).unwrap();
        path
    };
    let girt_file = copy("girt_hosts");
    let git_file = copy("git hosts with space");
    let girt_policy = HostKeyPolicy::new(unknown).with_files(vec![girt_file.clone()]);
    let git_policy = HostKeyPolicy::new(unknown).with_files(vec![git_file.clone()]);

    assert_eq!(
        git_ls_remote(&server, &git_policy, scratch.path()),
        succeeds
    );
    assert_eq!(girt_discover(&server, &girt_policy), succeeds);
    // An accepted new key is recorded the same way.
    let recorded = |path: &Path| {
        let mut lines: Vec<String> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        lines.sort();
        lines
    };
    assert_eq!(recorded(&girt_file), recorded(&git_file));
}

#[test]
fn relative_known_hosts_paths_are_refused() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let server = Server::new(fixture.root.path(), "none");
    let policy = HostKeyPolicy::new(UnknownHost::Reject).with_files(vec!["known_hosts".into()]);
    assert!(server.remote("config").with_host_keys(&policy).is_err());
}
