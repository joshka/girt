//! Original executable wrappers and disposable OpenSSH server; no upstream source fixtures.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use girt::Config;
use girt::fetch::{self, FetchError, FetchLimits};
use girt::remote::{ProtocolEnvironment, Remote};
use girt::transport::TransportControl;
use girt::transport::ssh::{ApprovedSshCommand, OpenSshOptions, SshError, SshRemote};
use rstest::rstest;

fn remote(url: &str, options: OpenSshOptions) -> SshRemote {
    let mut document = girt::config::Document::parse(b"").unwrap();
    document
        .append("remote", Some(b"r"), "url", url.as_bytes())
        .unwrap();
    let config: &Config = document.config();
    let destination = Remote::find(config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(config, &ProtocolEnvironment::default())
        .unwrap();
    SshRemote::openssh(config, &destination, options).unwrap()
}

fn script(root: &Path, body: &str) -> PathBuf {
    let path = root.join("ssh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn control(cancel: &AtomicBool) -> TransportControl<'_> {
    TransportControl {
        cancel,
        deadline: Some(Instant::now() + Duration::from_secs(5)),
    }
}

#[rstest]
#[case::omitted("ssh://host.test/repo")]
#[case::explicit("ssh://user@host.test:2222/repo")]
#[case::scp("user@host.test:repo")]
#[case::ipv6("ssh://user@[::1]:2222/repo")]
fn ordinary_argv_matches_git_and_environment_is_explicit(#[case] url: &str) {
    let root = tempfile::tempdir().unwrap();
    let executable = script(
        root.path(),
        "printf '%s\\n' \"$@\" > \"$CAPTURE\"\nprintf '%s\\n' \"${SSH_AUTH_SOCK-unset}\" \"${SSH_ASKPASS-unset}\" \"${PATH-unset}\" > \"$CAPTURE.env\"\nprintf 0000\n/bin/cat >/dev/null",
    );
    let capture = root.path().join("capture");
    let output = Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", root.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_SSH", &executable)
        .env("GIT_SSH_VARIANT", "ssh")
        .env("CAPTURE", &capture)
        .current_dir(root.path())
        .args(["-c", "protocol.version=0", "ls-remote", url])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let expected = std::fs::read(&capture).unwrap();
    let environment = [
        ("CAPTURE".into(), capture.clone().into_os_string()),
        ("SSH_AUTH_SOCK".into(), "/literal/socket".into()),
        ("SSH_ASKPASS".into(), "/literal/askpass".into()),
        ("PATH".into(), "/caller/path".into()),
    ]
    .into();
    let remote = remote(
        url,
        OpenSshOptions {
            default_executable: executable,
            environment,
            ..Default::default()
        },
    );
    let cancel = AtomicBool::new(false);
    super::runtime()
        .block_on(fetch::discover_ssh(
            &remote,
            FetchLimits::default(),
            control(&cancel),
        ))
        .unwrap();
    assert_eq!(std::fs::read(capture).unwrap(), expected);
    assert_eq!(
        std::fs::read_to_string(root.path().join("capture.env")).unwrap(),
        "/literal/socket\n/literal/askpass\n/caller/path\n"
    );
}

fn server_remote(server: &super::Server, config_name: &str) -> SshRemote {
    let config = server.root.join("ordinary_config");
    let content = std::fs::read_to_string(server.root.join(config_name)).unwrap();
    std::fs::write(&config, format!("{content}\nHost fixture\nHostName 127.0.0.1\nUser {}\nPort {}\nBatchMode yes\nStrictHostKeyChecking yes\n", server.user, server.port)).unwrap();
    let environment: BTreeMap<OsString, OsString> = [
        ("GIT_SSH_COMMAND".into(), "approved".into()),
        ("PATH".into(), std::env::var_os("PATH").unwrap()),
    ]
    .into();
    remote(
        &format!("ssh://fixture{}", server.repository),
        OpenSshOptions {
            approved_command: Some(ApprovedSshCommand {
                configured: b"approved".to_vec(),
                executable: "/usr/bin/ssh".into(),
                arguments: vec!["-F".into(), config.to_str().unwrap().into()],
            }),
            environment,
            ..Default::default()
        },
    )
}

#[test]
fn ordinary_discovery_uses_configured_user_port_identity_and_trust() {
    let fixture = super::Fixture::new(girt::ObjectFormat::Sha1, true, 8);
    let server = super::Server::new(fixture.root.path(), "none");
    let remote = server_remote(&server, "config");
    let cancel = AtomicBool::new(false);
    let result = super::runtime()
        .block_on(fetch::discover_ssh(
            &remote,
            FetchLimits::default(),
            control(&cancel),
        ))
        .unwrap();
    assert!(matches!(result.head, fetch::RemoteHead::Symbolic { .. }));
}

#[rstest]
#[case::unknown("unknown")]
#[case::changed("changed")]
fn ordinary_host_verification_remains_openssh_policy(#[case] config: &str) {
    let fixture = super::Fixture::new(girt::ObjectFormat::Sha1, true, 8);
    let server = super::Server::new(fixture.root.path(), "none");
    let remote = server_remote(&server, config);
    let cancel = AtomicBool::new(false);
    let error = super::runtime()
        .block_on(fetch::discover_ssh(
            &remote,
            FetchLimits::default(),
            control(&cancel),
        ))
        .unwrap_err();
    assert!(matches!(error, FetchError::Ssh(SshError::Exit(Some(255)))));
}

#[test]
fn ordinary_launched_failure_is_not_retried() {
    let root = tempfile::tempdir().unwrap();
    let executable = script(root.path(), "printf attempt >> \"$CAPTURE\"\nexit 42");
    let capture = root.path().join("capture");
    let remote = remote(
        "host:repo",
        OpenSshOptions {
            default_executable: executable,
            environment: [("CAPTURE".into(), capture.clone().into_os_string())].into(),
            ..Default::default()
        },
    );
    let cancel = AtomicBool::new(false);
    let error = super::runtime()
        .block_on(fetch::discover_ssh(
            &remote,
            FetchLimits::default(),
            control(&cancel),
        ))
        .unwrap_err();
    assert!(matches!(error, FetchError::Ssh(SshError::Exit(Some(42)))));
    assert_eq!(std::fs::read(capture).unwrap(), b"attempt");
}

#[test]
fn ordinary_cancellation_reaps_the_launched_process() {
    let root = tempfile::tempdir().unwrap();
    let executable = script(
        root.path(),
        "printf '%s' $$ > \"$CAPTURE\"\nexec /bin/sleep 60",
    );
    let capture = root.path().join("pid");
    let remote = remote(
        "host:repo",
        OpenSshOptions {
            default_executable: executable,
            environment: [("CAPTURE".into(), capture.clone().into_os_string())].into(),
            ..Default::default()
        },
    );
    let cancel = AtomicBool::new(false);
    let (error, pid) = super::runtime().block_on(async {
        tokio::join!(
            fetch::discover_ssh(&remote, FetchLimits::default(), control(&cancel)),
            async {
                let deadline = Instant::now() + Duration::from_secs(4);
                loop {
                    if let Ok(pid) = std::fs::read_to_string(&capture)
                        && let Ok(pid) = pid.parse::<i32>()
                    {
                        cancel.store(true, Ordering::Relaxed);
                        break rustix::process::Pid::from_raw(pid).unwrap();
                    }
                    assert!(Instant::now() < deadline, "wrapper did not start");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        )
    });
    assert!(matches!(error, Err(FetchError::Cancelled)));
    assert!(matches!(
        rustix::process::waitid(
            rustix::process::WaitId::Pid(pid),
            rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOHANG
        ),
        Err(rustix::io::Errno::CHILD)
    ));
}
