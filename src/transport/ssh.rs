//! Async protocol v0 over a caller-approved OpenSSH executable and configuration.
//!
//! Available with `ssh` on macOS/Linux. No runtime, credential discovery, keychain or cryptography
//! is implemented here. Callers own a Tokio runtime with I/O/time enabled and trust the executable,
//! config (including includes and Match exec), remote account and service. The configured path
//! accepts an R21 [`Destination`] and [`SshEnvironment`]. See [`SshRemote`].

mod process;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) use process::Session;

use super::TransportControl;
use crate::Config;
use crate::remote::{Destination, Protocol};

/// Application approval for one configured SSH command, without shell interpretation.
///
/// `configured` must exactly match the selected `GIT_SSH_COMMAND`, `GIT_SSH`, or
/// `core.sshCommand` value. The application parses and approves its meaning, then supplies an
/// absolute executable and literal arguments. The command never passes through a shell in girt.
pub struct ApprovedSshCommand {
    /// Exact configured value, including whitespace and case.
    pub configured: Vec<u8>,
    /// Trusted absolute executable path.
    pub executable: PathBuf,
    /// Application-approved literal arguments before the destination and service.
    pub arguments: Vec<String>,
}

impl std::fmt::Debug for ApprovedSshCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovedSshCommand").finish_non_exhaustive()
    }
}

/// Explicit application environment and trust inputs for a configured SSH destination.
#[derive(Default)]
pub struct SshEnvironment {
    /// `GIT_SSH_COMMAND`, taking precedence over `GIT_SSH` and `core.sshCommand`.
    pub git_ssh_command: Option<Vec<u8>>,
    /// `GIT_SSH`, used when no command override exists.
    pub git_ssh: Option<Vec<u8>>,
    /// `GIT_SSH_VARIANT`, taking precedence over `ssh.variant`.
    pub git_ssh_variant: Option<Vec<u8>>,
    /// One exact approved mapping for the selected command, if any.
    pub approved_command: Option<ApprovedSshCommand>,
    /// Trusted default OpenSSH executable if there is no selected command.
    pub default_executable: PathBuf,
    /// Trusted OpenSSH `-F` file; its includes and `Match exec` are caller policy.
    pub config_file: PathBuf,
    /// Default user for an endpoint without an explicit user.
    pub default_user: String,
    /// Application-selected agent socket; no process-global socket is consulted.
    pub agent_socket: Option<PathBuf>,
    /// Application-approved executable for encrypted identity passphrase prompting.
    pub askpass: Option<PathBuf>,
    /// Explicit executable search path for approved command dependencies.
    pub path_environment: Option<OsString>,
}

impl std::fmt::Debug for SshEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshEnvironment").finish_non_exhaustive()
    }
}

/// Explicit repository components and trusted OpenSSH configuration, with redacted Debug output.
///
/// Use with [`crate::fetch::receive_ssh`] and [`crate::push::send_ssh`]. Components are literal:
/// host is a DNS name, IPv4 or bare IPv6 address; user is explicit; port is nonzero; path is a
/// UTF-8 absolute or account-relative path. URL/scp syntax, percent decoding and tilde expansion
/// are not supported. Spaces, quotes and shell metacharacters in paths are preserved literally.
/// The server must use a POSIX-compatible shell and provide `git-upload-pack`/`git-receive-pack`.
///
/// The executable and `-F` configuration file must be explicit absolute paths. [`Self::new`]
/// accepts no configured command, agent or askpass; [`Self::configured`] accepts them through
/// application-supplied inputs. Configuration may choose public-key identity and known-host files.
/// Default identity filenames, password, keyboard-interactive and PKCS#11 discovery are disabled.
/// Security-key identities can still require hardware interaction; cancellation/deadlines bound
/// their waits. Encrypted keys requiring a prompt fail unless askpass is supplied. Caller config is
/// trusted executable policy: includes and Match exec can run local commands. Do not pass untrusted
/// config files.
///
/// Enforced command-line policy overrides config: strict host-key verification, batch mode unless
/// askpass is supplied,
/// no TTY, forwarding, proxies, local commands, multiplexing or backgrounding. Unknown/changed
/// keys fail; pre-provision trust in the selected known-host file. No host-key updates are made.
/// OpenSSH config may choose algorithms and trust authorities, but cannot disable strict checking.
/// The service command is fixed. Approved arguments are trusted application policy and can affect
/// OpenSSH behavior. Config environment directives are caller-owned and must not request another
/// Git protocol version. Server-side forced commands may impose
/// their own policy. No automatic retry occurs.
///
/// Async waits poll [`TransportControl`] at 20 ms intervals, subject to scheduling. The absolute
/// deadline includes handshake, service pipes, stderr disposal and server exit. Spawn/config path
/// lookup and OS reaping are synchronous; no hard whole-call latency bound is promised. Dropping
/// the future kills/reaps the local SSH process group; remote mutations cannot be rolled back or
/// guaranteed stopped. Prefer cancellation followed by awaiting a push's classified outcome.
/// Callers must not globally reap these children. Descendants escaping the group are excluded.
///
/// Stderr is drained and discarded to prevent blockage and credential/path leakage. Errors expose
/// only static categories, I/O kinds and exit codes; Git progress/status bytes remain untrusted.
/// An exit code of 255 indicates an SSH/service failure, not a Git ref rejection. OpenSSH cannot
/// reliably distinguish authentication, host trust and network failures without parsing stderr.
/// Debug and errors never include configuration values. Caller concurrency bounds total resources.
pub struct SshRemote {
    host: String,
    user: String,
    port: u16,
    path: String,
    executable: PathBuf,
    config: PathBuf,
    arguments: Vec<String>,
    agent_socket: Option<PathBuf>,
    askpass: Option<PathBuf>,
    path_environment: Option<OsString>,
}

impl std::fmt::Debug for SshRemote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshRemote").finish_non_exhaustive()
    }
}

/// Sanitized process/SSH failure, distinct from Git protocol rejection reports.
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    /// Unsupported or unsafe endpoint/configuration shape; values are not included.
    #[error("invalid SSH configuration: {0}")]
    Configuration(&'static str),
    /// Spawn, pipe, poll or reap error, retaining only the non-sensitive I/O kind.
    #[error("SSH I/O failure: {0:?}")]
    Io(std::io::ErrorKind),
    /// SSH or remote service exited unsuccessfully; 255 is ambiguous, `None` means a signal.
    #[error(
        "SSH or remote service failed (exit {0:?}); check explicit trust and authentication configuration"
    )]
    Exit(Option<i32>),
    /// Invalid pkt-line envelope before service parsing.
    #[error("invalid SSH service framing: {0}")]
    Protocol(&'static str),
    /// Retained advertisement/response exceeded its explicit bound.
    #[error("SSH response byte limit exceeded")]
    Limit,
    /// Caller cancellation observed.
    #[error("SSH cancelled")]
    Cancelled,
    /// Absolute caller deadline expired.
    #[error("SSH deadline expired")]
    Deadline,
}
impl From<std::io::Error> for SshError {
    fn from(error: std::io::Error) -> Self {
        match super::interruption(&error) {
            Some(super::Interruption::Cancelled) => Self::Cancelled,
            Some(super::Interruption::Deadline) => Self::Deadline,
            None => Self::Io(error.kind()),
        }
    }
}

impl SshRemote {
    /// Validates literal endpoint components and explicit absolute executable/config paths.
    ///
    /// `config` selects OpenSSH's `-F` file; `/dev/null` selects no config. Identity and host trust
    /// must already be usable noninteractively. Construction performs no I/O and creates no
    /// runtime. Network methods require the caller's Tokio I/O/time context.
    ///
    /// # Errors
    ///
    /// Rejects empty/oversized components, control characters, invalid host/user syntax, zero port,
    /// paths beginning with `-` or `~`, non-absolute executable/config paths, URLs and scp syntax.
    /// No arbitrary SSH options or service command overrides are accepted by this API.
    pub fn new(
        host: &str,
        user: &str,
        port: u16,
        path: &str,
        executable: &Path,
        config: &Path,
    ) -> Result<Self, SshError> {
        Self::from_components(
            host,
            user,
            port,
            path,
            executable,
            config,
            std::env::var_os("PATH"),
        )
    }

    fn from_components(
        host: &str,
        user: &str,
        port: u16,
        path: &str,
        executable: &Path,
        config: &Path,
        path_environment: Option<OsString>,
    ) -> Result<Self, SshError> {
        let host_name = safe_name(host) && !host.contains('_');
        if host.len() > 255 || !(host_name || host.parse::<std::net::Ipv6Addr>().is_ok()) {
            return Err(SshError::Configuration("host"));
        }
        if user.len() > 255 || !safe_name(user) {
            return Err(SshError::Configuration("user"));
        }
        if port == 0 {
            return Err(SshError::Configuration("port"));
        }
        if path.is_empty()
            || path.len() > 8192
            || path.starts_with(['-', '~'])
            || path.chars().any(char::is_control)
        {
            return Err(SshError::Configuration("repository path"));
        }
        for file in [executable, config] {
            if !file.is_absolute()
                || file.as_os_str().len() > 8192
                || file
                    .as_os_str()
                    .as_encoded_bytes()
                    .iter()
                    .any(|b| b.is_ascii_control())
            {
                return Err(SshError::Configuration("absolute executable/config file"));
            }
        }
        Ok(Self {
            host: host.into(),
            user: user.into(),
            port,
            path: path.into(),
            executable: executable.into(),
            config: config.into(),
            arguments: Vec::new(),
            agent_socket: None,
            askpass: None,
            path_environment,
        })
    }

    /// Resolves an R21 SSH destination with an explicit command and authentication policy.
    ///
    /// Only OpenSSH's `ssh` variant is supported. Configured commands require an exact
    /// application-approved mapping; shell snippets are never executed or parsed by girt. The
    /// default executable and config file are likewise supplied by the application. Configured
    /// command arguments remain trusted application policy and must be checked before approval.
    /// An agent socket or askpass executable may be supplied independently. Password and
    /// keyboard-interactive authentication remain disabled; askpass serves encrypted keys only.
    ///
    /// # Errors
    ///
    /// Refuses malformed endpoints, variants, command approvals or authentication paths before
    /// spawning a process. Error and Debug output never contain endpoint or command bytes.
    pub fn configured(
        config: &Config,
        destination: &Destination,
        environment: SshEnvironment,
    ) -> Result<Self, SshError> {
        if destination.protocol() != Protocol::Ssh {
            return Err(SshError::Configuration("SSH destination"));
        }
        let variant = environment
            .git_ssh_variant
            .as_deref()
            .or_else(|| config.values("ssh", None, "variant").last().flatten());
        if variant.is_some_and(|value| !value.eq_ignore_ascii_case(b"ssh")) {
            return Err(SshError::Configuration("SSH variant"));
        }
        let selected = environment
            .git_ssh_command
            .as_deref()
            .or(environment.git_ssh.as_deref())
            .or_else(|| config.values("core", None, "sshcommand").last().flatten());
        let (executable, arguments) = match selected {
            Some(value) => {
                let approved = environment.approved_command.ok_or(SshError::Configuration(
                    "SSH command requires application approval",
                ))?;
                if approved.configured != value {
                    return Err(SshError::Configuration("SSH command approval mismatch"));
                }
                if approved.arguments.iter().any(|arg| {
                    arg.is_empty() || arg.len() > 8192 || arg.chars().any(char::is_control)
                }) {
                    return Err(SshError::Configuration("SSH command argument"));
                }
                (approved.executable, approved.arguments)
            }
            None => {
                if environment.approved_command.is_some() {
                    return Err(SshError::Configuration("unused SSH command approval"));
                }
                (environment.default_executable, Vec::new())
            }
        };
        let (host, user, port, path) =
            parse_destination(destination.bytes(), &environment.default_user)?;
        let mut remote = Self::from_components(
            &host,
            &user,
            port,
            &path,
            &executable,
            &environment.config_file,
            environment.path_environment,
        )?;
        for file in [
            environment.agent_socket.as_deref(),
            environment.askpass.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_file(file)?;
        }
        remote.arguments = arguments;
        remote.agent_socket = environment.agent_socket;
        remote.askpass = environment.askpass;
        Ok(remote)
    }

    pub(crate) fn connect(
        &self,
        service: &str,
        control: TransportControl<'_>,
    ) -> Result<Session, SshError> {
        control.check()?;
        Session::spawn(&mut self.command(service))
    }

    fn command(&self, service: &str) -> Command {
        let mut command = Command::new(&self.executable);
        command.env_clear();
        // OpenSSH config can explicitly invoke programs. Pass only the caller-selected search
        // path and authentication inputs; never inherit loader or Git-protocol variables.
        if let Some(path) = &self.path_environment {
            command.env("PATH", path);
        }
        if let Some(socket) = &self.agent_socket {
            command.env("SSH_AUTH_SOCK", socket);
        }
        if let Some(askpass) = &self.askpass {
            command.env("SSH_ASKPASS", askpass);
            command.env("SSH_ASKPASS_REQUIRE", "force");
            command.env("DISPLAY", "girt-askpass");
        } else {
            command.env("SSH_ASKPASS_REQUIRE", "never");
        }
        command.args(["-T", "-F"]).arg(&self.config);
        command.args([
            "-o",
            if self.askpass.is_some() {
                "BatchMode=no"
            } else {
                "BatchMode=yes"
            },
        ]);
        command.args([
            "-o",
            if self.agent_socket.is_some() {
                "IdentityAgent=SSH_AUTH_SOCK"
            } else {
                "IdentityAgent=none"
            },
        ]);
        for option in [
            "StrictHostKeyChecking=yes",
            "UpdateHostKeys=no",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "PreferredAuthentications=publickey",
            "IdentityFile=none",
            "IdentitiesOnly=yes",
            "PKCS11Provider=none",
            "SecurityKeyProvider=internal",
            "ConnectionAttempts=1",
            "Tunnel=no",
            "ForwardAgent=no",
            "ForwardX11=no",
            "ClearAllForwardings=yes",
            "PermitLocalCommand=no",
            "ProxyCommand=none",
            "ProxyJump=none",
            "ControlMaster=no",
            "ControlPath=none",
            "ControlPersist=no",
            "ForkAfterAuthentication=no",
            "RequestTTY=no",
            "RemoteCommand=none",
            "SessionType=default",
            "StdinNull=no",
            "EscapeChar=none",
        ] {
            command.args(["-o", option]);
        }
        command.args(&self.arguments);
        command.args([
            "-p",
            &self.port.to_string(),
            "-l",
            &self.user,
            "--",
            &self.host,
        ]);
        command.arg(format!("{service} '{}'", self.path.replace('\'', "'\\''")));
        command
    }
}
fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
}

fn validate_file(file: &Path) -> Result<(), SshError> {
    if !file.is_absolute()
        || file.as_os_str().len() > 8192
        || file
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(|byte| byte.is_ascii_control())
    {
        return Err(SshError::Configuration("SSH authentication path"));
    }
    Ok(())
}

fn parse_destination(
    raw: &[u8],
    default_user: &str,
) -> Result<(String, String, u16, String), SshError> {
    let raw =
        std::str::from_utf8(raw).map_err(|_| SshError::Configuration("SSH endpoint encoding"))?;
    let (authority, path, url) = if let Some(rest) = raw.strip_prefix("ssh://") {
        let (authority, path) = rest
            .split_once('/')
            .ok_or(SshError::Configuration("SSH URL path"))?;
        (authority, format!("/{path}"), true)
    } else {
        let (authority, path) = raw
            .split_once(':')
            .ok_or(SshError::Configuration("SSH scp endpoint"))?;
        (authority, path.to_owned(), false)
    };
    let (user, host_port) = authority
        .split_once('@')
        .map_or((default_user, authority), |(user, host)| (user, host));
    if authority.matches('@').count() > 1 || raw.contains(['?', '#', '%']) {
        return Err(SshError::Configuration("SSH endpoint syntax"));
    }
    let (host, port) = if url {
        if let Some(bracketed) = host_port.strip_prefix('[') {
            let (host, suffix) = bracketed
                .split_once(']')
                .ok_or(SshError::Configuration("SSH host"))?;
            let port = suffix.strip_prefix(':').map_or(Ok(22), parse_port)?;
            (host, port)
        } else if let Some((host, port)) = host_port.split_once(':') {
            (host, parse_port(port)?)
        } else {
            (host_port, 22)
        }
    } else {
        (host_port, 22)
    };
    if host.contains(['[', ']', ':']) && host.parse::<std::net::Ipv6Addr>().is_err() {
        return Err(SshError::Configuration("SSH host"));
    }
    Ok((host.to_owned(), user.to_owned(), port, path))
}

fn parse_port(port: &str) -> Result<u16, SshError> {
    port.parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or(SshError::Configuration("SSH port"))
}

#[cfg(test)]
mod tests;
