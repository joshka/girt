//! Original recording wrappers compare Git command selection with girt's public SSH API.
//! No network connection, upstream source, or upstream fixture is used.
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use girt::Config;
use girt::fetch::{self, FetchLimits};
use girt::remote::{ProtocolEnvironment, Remote};
use girt::transport::TransportControl;
use girt::transport::ssh::{ApprovedSshCommand, SshEnvironment, SshRemote};
use rstest::rstest;

struct Commands {
    root: tempfile::TempDir,
}

impl Commands {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for name in ["command", "configured", "legacy", "default"] {
            let path = root.path().join(name);
            // Advertise no refs, then drain the client flush before exiting to avoid a pipe race.
            let script = format!(
                "#!/bin/sh\nprintf '%s\\n' '{name}' \"$@\" > '{}'/capture\nprintf '0000'\n/bin/cat > /dev/null\n",
                root.path().display()
            );
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn value(&self, name: &str) -> Vec<u8> {
        self.path(name).as_os_str().as_encoded_bytes().to_vec()
    }

    fn capture(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("capture"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn git(&self, command: bool, config: bool, legacy: bool) -> Command {
        let mut git = Command::new("git");
        git.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", self.root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_SSH_VARIANT", "ssh")
            .current_dir(self.root.path())
            .args(["-c", "protocol.version=0"]);
        if command {
            git.env("GIT_SSH_COMMAND", self.path("command"));
        }
        if config {
            git.arg("-c").arg(format!(
                "core.sshCommand={}",
                self.path("configured").display()
            ));
        }
        if legacy {
            git.env("GIT_SSH", self.path("legacy"));
        }
        git.args(["ls-remote", "ssh://user@host.test:2222/repo"]);
        git
    }

    fn native(&self, command: bool, config: bool, legacy: bool, selected: &str) -> SshRemote {
        let mut source = "[remote \"r\"]\nurl = ssh://user@host.test:2222/repo\n".to_owned();
        if config {
            source.push_str(&format!(
                "[core]\nsshCommand = {}\n",
                self.path("configured").display()
            ));
        }
        let config = Config::parse(source.as_bytes()).unwrap();
        let destination = Remote::find(&config, b"r")
            .unwrap()
            .unwrap()
            .fetch_destination(&config, &ProtocolEnvironment::default())
            .unwrap();
        SshRemote::configured(
            &config,
            &destination,
            SshEnvironment {
                git_ssh_command: command.then(|| self.value("command")),
                git_ssh: legacy.then(|| self.value("legacy")),
                git_ssh_variant: Some(b"ssh".to_vec()),
                approved_command: Some(ApprovedSshCommand {
                    configured: self.value(selected),
                    executable: self.path(selected),
                    arguments: vec!["literal ; $(touch must-not-exist)".into()],
                }),
                default_executable: self.path("default"),
                config_file: "/dev/null".into(),
                default_user: "default".into(),
                ..SshEnvironment::default()
            },
        )
        .unwrap()
    }
}

#[rstest]
#[case::all_overrides(true, true, true, "command")]
#[case::config_over_legacy(false, true, true, "configured")]
#[case::legacy_only(false, false, true, "legacy")]
#[case::config_only(false, true, false, "configured")]
fn command_selection_matches_git(
    #[case] command: bool,
    #[case] config: bool,
    #[case] legacy: bool,
    #[case] selected: &str,
) {
    let fixture = Commands::new();
    let output = fixture.git(command, config, legacy).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let git_arguments = fixture.capture();
    assert_eq!(
        git_arguments,
        [
            selected,
            "-p",
            "2222",
            "user@host.test",
            "git-upload-pack '/repo'"
        ]
    );

    let remote = fixture.native(command, config, legacy, selected);
    let cancel = AtomicBool::new(false);
    let control = TransportControl {
        cancel: &cancel,
        deadline: Some(Instant::now() + Duration::from_secs(5)),
    };
    let result = super::runtime().block_on(fetch::discover_ssh(
        &remote,
        FetchLimits::default(),
        control,
    ));
    assert!(result.is_ok(), "{result:?}");
    let native_arguments = fixture.capture();
    assert_eq!(native_arguments[0], selected);
    assert_eq!(
        &native_arguments[native_arguments.len() - 8..],
        [
            "literal ; $(touch must-not-exist)",
            "-p",
            "2222",
            "-l",
            "user",
            "--",
            "host.test",
            "git-upload-pack '/repo'",
        ]
    );
}

#[rstest]
#[case::valueless_command("core.sshCommand", "missing value")]
#[case::valueless_variant("ssh.variant", "missing value")]
#[case::empty_command("core.sshCommand=", "cannot run")]
fn git_rejects_selected_invalid_configuration(#[case] setting: &str, #[case] diagnostic: &str) {
    let fixture = Commands::new();
    let output = Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", fixture.root.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .env("GIT_SSH", fixture.path("legacy"))
        .current_dir(fixture.root.path())
        .args(["-c", setting, "ls-remote", "host.test:repo"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(diagnostic),
        "{output:?}"
    );
    assert!(!fixture.path("capture").exists());
}

#[test]
fn git_environment_overrides_valueless_configuration() {
    let fixture = Commands::new();
    let output = Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", fixture.root.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_SSH_COMMAND", fixture.path("command"))
        .env("GIT_SSH_VARIANT", "ssh")
        .current_dir(fixture.root.path())
        .args([
            "-c",
            "core.sshCommand",
            "-c",
            "ssh.variant",
            "-c",
            "protocol.version=0",
            "ls-remote",
            "host.test:repo",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fixture.capture(),
        ["command", "host.test", "git-upload-pack 'repo'"]
    );
}
