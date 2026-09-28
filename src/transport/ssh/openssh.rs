//! Caller-owned OpenSSH configuration and process environment.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;

use super::{ApprovedSshCommand, LaunchPolicy, SshError, SshRemote};
use crate::Config;
use crate::remote::{Destination, Protocol};

/// Explicit executable approval and complete environment for ordinary OpenSSH policy.
///
/// The caller trusts the selected executable, its arguments, configuration files and environment.
/// OpenSSH controls identities, agents, proxies, host verification and prompting. No restrictive
/// options or default user/port are added. This is not an executable or configuration sandbox.
/// Missing `ssh.variant` means the caller asserts that the chosen executable speaks OpenSSH.
/// Shell parsing, executable-name inference and automatic variant probing are not performed.
#[derive(Default)]
pub struct OpenSshOptions {
    /// Absolute OpenSSH executable when no command override is selected.
    pub default_executable: PathBuf,
    /// Exact application-approved mapping for a selected command override.
    pub approved_command: Option<ApprovedSshCommand>,
    /// Complete child environment, replacing inheritance from the running process.
    ///
    /// Also supplies `GIT_SSH_COMMAND`, `GIT_SSH` and `GIT_SSH_VARIANT` for selection. The caller
    /// can capture its environment explicitly, including `PATH`, `HOME`, `SSH_AUTH_SOCK`, askpass
    /// and display variables. Values are passed literally. `GIT_PROTOCOL` must be absent or
    /// `version=0`; OpenSSH configuration must not send another protocol version either.
    pub environment: BTreeMap<OsString, OsString>,
}

impl std::fmt::Debug for OpenSshOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenSshOptions").finish_non_exhaustive()
    }
}

impl SshRemote {
    /// Resolves an SSH destination while retaining ordinary OpenSSH configuration policy.
    ///
    /// Selects `GIT_SSH_COMMAND`, the last `core.sshCommand`, `GIT_SSH`, then the supplied default
    /// executable. Selected overrides require an exact [`ApprovedSshCommand`] mapping. An absent
    /// variant or case-insensitive `ssh` is accepted. No shell is used by girt. Missing URL user
    /// and port remain absent so OpenSSH can resolve them from its configuration.
    ///
    /// The command consists of approved arguments, an optional explicit `-p`, `[user@]host`, and
    /// a shell-quoted fixed Git service/path. OpenSSH may read normal config/identity/trust files,
    /// run configured local/proxy commands, update host keys and prompt according to its policy.
    /// The caller owns the complete environment and must approve these effects.
    ///
    /// Protocol pipes, discarded stderr and process-group cleanup use the same session owner as
    /// [`Self::configured`]. A controlling-terminal prompt may be inaccessible from that process
    /// group; interactive terminal parity is not promised. Askpass can use the supplied
    /// environment. Do not automatically retry after a launched failure: authentication and
    /// trust side effects may already have occurred. Only Git protocol v0 is supported.
    ///
    /// # Errors
    ///
    /// Rejects unsupported endpoint syntax, malformed environment, nonzero protocol versions,
    /// variants, empty/valueless commands, missing/mismatched approvals, invalid arguments and
    /// non-absolute executables before spawning. Error and Debug values redact caller inputs.
    pub fn openssh(
        config: &Config,
        destination: &Destination,
        options: OpenSshOptions,
    ) -> Result<Self, SshError> {
        if destination.protocol() != Protocol::Ssh {
            return Err(SshError::Configuration("SSH destination"));
        }
        validate_environment(&options.environment)?;
        let value = |name: &str| {
            options
                .environment
                .get(std::ffi::OsStr::new(name))
                .map(|value| value.as_os_str().as_bytes())
        };
        let variant = match value("GIT_SSH_VARIANT") {
            Some(value) => Some(value),
            None => super::configured_value(config, "ssh", "variant")?,
        };
        if variant.is_some_and(|value| !value.eq_ignore_ascii_case(b"ssh")) {
            return Err(SshError::Configuration("SSH variant"));
        }
        let selected = match value("GIT_SSH_COMMAND") {
            Some(value) => Some(value),
            None => super::configured_value(config, "core", "sshcommand")?.or(value("GIT_SSH")),
        };
        let (executable, arguments) = super::approve_command(
            selected,
            options.approved_command,
            options.default_executable,
        )?;
        super::validate_file(&executable)?;
        let (host, user, port, path) = super::parse_endpoint(destination.bytes())?;
        super::validate_endpoint(&host, user.as_deref(), port, &path)?;
        Ok(Self {
            host,
            user,
            port,
            path,
            executable,
            arguments,
            policy: LaunchPolicy::OpenSsh(options.environment),
        })
    }
}

fn validate_environment(environment: &BTreeMap<OsString, OsString>) -> Result<(), SshError> {
    for (name, value) in environment {
        let name = name.as_os_str().as_bytes();
        if name.is_empty()
            || name.contains(&b'=')
            || name.contains(&0)
            || value.as_os_str().as_bytes().contains(&0)
        {
            return Err(SshError::Configuration("SSH process environment"));
        }
    }
    if environment
        .get(std::ffi::OsStr::new("GIT_PROTOCOL"))
        .is_some_and(|value| value != "version=0")
    {
        return Err(SshError::Configuration("SSH protocol version"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::remote::{ProtocolEnvironment, Remote};

    fn remote(url: &str, extra: &str, options: OpenSshOptions) -> Result<SshRemote, SshError> {
        let source = format!("[remote \"r\"]\nurl = {url}\n{extra}");
        let config = Config::parse(source.as_bytes()).unwrap();
        let destination = Remote::find(&config, b"r")
            .unwrap()
            .unwrap()
            .fetch_destination(&config, &ProtocolEnvironment::default())
            .unwrap();
        SshRemote::openssh(&config, &destination, options)
    }

    fn options() -> OpenSshOptions {
        OpenSshOptions {
            default_executable: "/usr/bin/ssh".into(),
            ..Default::default()
        }
    }

    #[rstest]
    #[case::omitted("ssh://host/repo", vec!["host", "git-upload-pack '/repo'"])]
    #[case::explicit("ssh://user@host:2222/repo", vec!["-p", "2222", "user@host", "git-upload-pack '/repo'"])]
    #[case::scp("user@host:repo", vec!["user@host", "git-upload-pack 'repo'"])]
    #[case::ipv6("ssh://[::1]/repo", vec!["::1", "git-upload-pack '/repo'"])]
    fn preserves_endpoint_choices(#[case] url: &str, #[case] expected: Vec<&str>) {
        let remote = remote(url, "", options()).unwrap();
        let command = remote.command("git-upload-pack");
        assert_eq!(command.get_args().collect::<Vec<_>>(), expected);
        assert_eq!(command.get_envs().count(), 0);
    }

    #[rstest]
    #[case::environment(Some("command"), Some("legacy"), "sshCommand = configured", "command")]
    #[case::configured(None, Some("legacy"), "sshCommand = configured", "configured")]
    #[case::legacy(None, Some("legacy"), "", "legacy")]
    #[case::last_value(
        None,
        Some("legacy"),
        "sshCommand = old\nsshCommand = configured",
        "configured"
    )]
    #[case::implicit_overridden(Some("command"), None, "sshCommand", "command")]
    fn selects_exact_approved_command(
        #[case] command: Option<&str>,
        #[case] legacy: Option<&str>,
        #[case] setting: &str,
        #[case] selected: &str,
    ) {
        let environment = [("GIT_SSH_COMMAND", command), ("GIT_SSH", legacy)]
            .into_iter()
            .filter_map(|(name, value)| value.map(|value| (name.into(), value.into())))
            .collect();
        let options = OpenSshOptions {
            environment,
            approved_command: Some(ApprovedSshCommand {
                configured: selected.as_bytes().to_vec(),
                executable: "/approved/ssh".into(),
                arguments: vec!["literal ; $(secret)".into()],
            }),
            ..options()
        };
        let remote = remote("ssh://host/repo", &format!("[core]\n{setting}"), options).unwrap();
        let command = remote.command("git-upload-pack");
        assert_eq!(command.get_program(), "/approved/ssh");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["literal ; $(secret)", "host", "git-upload-pack '/repo'"]
        );
    }

    #[rstest]
    #[case::unapproved(
        "[core]\nsshCommand = secret",
        "SSH command requires application approval"
    )]
    #[case::implicit("[core]\nsshCommand", "SSH configuration requires a value")]
    #[case::empty("[core]\nsshCommand =", "empty SSH command")]
    #[case::implicit_variant("[ssh]\nvariant", "SSH configuration requires a value")]
    #[case::variant("[ssh]\nvariant = plink", "SSH variant")]
    fn refuses_unsupported_configuration(#[case] setting: &str, #[case] expected: &str) {
        let error = remote("ssh://host/repo", setting, options()).unwrap_err();
        assert!(matches!(error, SshError::Configuration(value) if value == expected));
        assert!(!format!("{error:?} {error}").contains("secret"));
    }

    #[rstest]
    #[case::protocol("GIT_PROTOCOL", "version=2", "SSH protocol version")]
    #[case::key("bad=key", "secret", "SSH process environment")]
    #[case::nul("KEY", "secret\0value", "SSH process environment")]
    #[case::empty_command("GIT_SSH_COMMAND", "", "empty SSH command")]
    #[case::variant("GIT_SSH_VARIANT", "auto", "SSH variant")]
    fn rejects_invalid_environment(#[case] key: &str, #[case] value: &str, #[case] expected: &str) {
        let options = OpenSshOptions {
            environment: [(key.into(), value.into())].into(),
            ..options()
        };
        let error = remote("ssh://host/repo", "", options).unwrap_err();
        assert!(matches!(error, SshError::Configuration(value) if value == expected));
    }

    #[test]
    fn variant_environment_bypasses_implicit_config() {
        let options = OpenSshOptions {
            environment: [("GIT_SSH_VARIANT".into(), "SSH".into())].into(),
            ..options()
        };
        assert!(remote("ssh://host/repo", "[ssh]\nvariant", options).is_ok());
    }

    #[test]
    fn rejects_relative_executable_before_spawning() {
        let options = OpenSshOptions {
            default_executable: "ssh".into(),
            ..options()
        };
        assert!(matches!(
            remote("ssh://host/repo", "", options),
            Err(SshError::Configuration(_))
        ));
    }

    #[test]
    fn environment_is_literal_and_debug_redacts() {
        let environment = [
            ("SSH_AUTH_SOCK".into(), "/secret/socket".into()),
            ("SSH_ASKPASS".into(), "/secret/askpass".into()),
            ("GIT_PROTOCOL".into(), "version=0".into()),
        ]
        .into();
        let options = OpenSshOptions {
            environment,
            ..options()
        };
        assert!(!format!("{options:?}").contains("secret"));
        let remote = remote("ssh://host/repo", "", options).unwrap();
        let command = remote.command("git-upload-pack");
        assert_eq!(command.get_envs().count(), 3);
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == "SSH_AUTH_SOCK"
                    && value == Some(std::ffi::OsStr::new("/secret/socket")))
        );
        assert!(!format!("{remote:?}").contains("secret"));
    }

    #[test]
    fn lower_priority_approval_cannot_authorize_selected_command() {
        let options = OpenSshOptions {
            environment: [("GIT_SSH".into(), "legacy".into())].into(),
            approved_command: Some(ApprovedSshCommand {
                configured: b"legacy".to_vec(),
                executable: "/approved/ssh".into(),
                arguments: Vec::new(),
            }),
            ..options()
        };
        let error =
            remote("ssh://host/repo", "[core]\nsshCommand = selected", options).unwrap_err();
        assert!(matches!(
            error,
            SshError::Configuration("SSH command approval mismatch")
        ));
    }
}
