//! Async Git protocol over a caller-approved OpenSSH executable and configuration: upload-pack
//! speaks v0, v1 or v2 as selected by [`SshRemote::with_protocol`]; receive-pack speaks v0/v1.
//!
//! [`SshRemote::new`] accepts explicit endpoint and trusted executable inputs;
//! [`SshRemote::configured`] resolves a selected [`Destination`] with [`crate::Config`] and
//! [`SshEnvironment`]. [`SshRemote::openssh`] retains ordinary OpenSSH configuration with a
//! caller-supplied environment. [`ApprovedSshCommand`] lets the application authorize a configured
//! command before girt starts a process. [`TransportControl`] governs
//! owned waits; the application owns the Tokio runtime and synchronous fetch validation worker.
//!
//! Available with `ssh` on macOS/Linux. No runtime, credential discovery, keychain or cryptography
//! is implemented here. Callers own a Tokio runtime with I/O/time enabled and trust the executable,
//! config (including includes and Match exec), remote account and service. The configured path
//! accepts a [`Destination`] and [`SshEnvironment`]. See [`SshRemote`].
//!
//! # Choosing a constructor
//!
//! - [`SshRemote::openssh`] runs `ssh` the way Git does, with the user's own `~/.ssh/config`, keys,
//!   agent and host-key policy. Interactive clients such as jj use it, through [`crate::workflow`].
//! - [`SshRemote::configured`] resolves the same URLs, but pins an explicit configuration file and
//!   forces restrictive options: strict host-key checking, no password prompts, forwarding or
//!   connection sharing. Services that must not depend on a user's settings use it.
//! - [`SshRemote::new`] takes host, user, port and path as separate, validated values, with the
//!   same restrictions as [`SshRemote::configured`].
//!
//! SSH authentication is entirely OpenSSH's; girt never sees a key or password. Because the SSH
//! process runs in its own process group, it cannot prompt on the terminal unless the application
//! attaches one with [`SshRemote::with_terminal`]. Otherwise passphrase prompts need an agent or
//! `SSH_ASKPASS` in the supplied environment. OpenSSH's standard error explains authentication
//! and host-key failures; receive it with [`crate::fetch::discover_ssh_with_diagnostics`] or the
//! `*_with_progress` adapters and show it to the user.
//!
//! # Example
//!
//! List a server's references through an approved OpenSSH executable and configuration file. The
//! caller's Tokio runtime drives the future; the example compiles but does not connect:
//!
//! ```no_run
//! use std::path::Path;
//! use std::sync::atomic::AtomicBool;
//!
//! use girt::fetch::{FetchLimits, discover_ssh};
//! use girt::transport::TransportControl;
//! use girt::transport::ssh::SshRemote;
//!
//! async fn list() -> Result<(), Box<dyn std::error::Error>> {
//!     let remote = SshRemote::new(
//!         "git.example.com",
//!         "git",
//!         22,
//!         "/srv/project.git",
//!         Path::new("/usr/bin/ssh"),
//!         Path::new("/etc/girt/ssh_config"),
//!     )?;
//!     let cancel = AtomicBool::new(false);
//!     let discovery = discover_ssh(
//!         &remote,
//!         FetchLimits::default(),
//!         TransportControl::new(&cancel),
//!     )
//!     .await?;
//!     println!("remote HEAD: {:?}", discovery.head);
//!     Ok(())
//! }
//! ```

mod openssh;
mod process;
mod terminal;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use openssh::OpenSshOptions;
pub(crate) use process::Session;
pub use terminal::{ForegroundTerminal, StoppedTerminal, TerminalEvents};

use super::TransportControl;
use crate::Config;
use crate::remote::{Destination, Protocol};

/// Application approval for one configured SSH command, without shell interpretation.
///
/// Git runs `GIT_SSH_COMMAND` and `core.sshCommand` through a shell. girt never does: the
/// application decides what the configured text means and supplies the executable and literal
/// arguments. [`crate::workflow`] approves the configured command by running it through `/bin/sh`,
/// as Git does; an application that does not trust its configuration can refuse instead.
///
/// # Examples
///
/// ```
/// use girt::transport::ssh::ApprovedSshCommand;
///
/// // `GIT_SSH_COMMAND="ssh -i ~/.ssh/deploy"`, approved with the path expanded by the application.
/// let approved = ApprovedSshCommand::new(
///     b"ssh -i ~/.ssh/deploy".to_vec(),
///     "/usr/bin/ssh".into(),
///     vec!["-i".into(), "/home/ci/.ssh/deploy".into()],
/// );
/// # let _ = approved;
/// ```
///
/// `configured` must exactly match the selected `GIT_SSH_COMMAND`, `GIT_SSH`, or
/// `core.sshCommand` value. The application parses and approves its meaning, then supplies an
/// absolute executable and literal arguments. The command never passes through a shell in girt.
#[non_exhaustive]
pub struct ApprovedSshCommand {
    /// Exact configured value, including whitespace and case.
    pub configured: Vec<u8>,
    /// Trusted absolute executable path.
    pub executable: PathBuf,
    /// Application-approved literal arguments before the destination and service.
    pub arguments: Vec<String>,
}

impl ApprovedSshCommand {
    /// Records the application-approved command spelling, executable and literal arguments.
    pub fn new(configured: Vec<u8>, executable: PathBuf, arguments: Vec<String>) -> Self {
        Self {
            configured,
            executable,
            arguments,
        }
    }

    /// Sets [`Self::configured`].
    #[must_use]
    pub fn with_configured(mut self, configured: Vec<u8>) -> Self {
        self.configured = configured;
        self
    }

    /// Sets [`Self::executable`].
    #[must_use]
    pub fn with_executable(mut self, executable: PathBuf) -> Self {
        self.executable = executable;
        self
    }

    /// Sets [`Self::arguments`].
    #[must_use]
    pub fn with_arguments(mut self, arguments: Vec<String>) -> Self {
        self.arguments = arguments;
        self
    }
}

impl std::fmt::Debug for ApprovedSshCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovedSshCommand").finish_non_exhaustive()
    }
}

/// Explicit application environment and trust inputs for a configured SSH destination.
#[derive(Default)]
#[non_exhaustive]
pub struct SshEnvironment {
    /// `GIT_SSH_COMMAND`, taking precedence over `core.sshCommand` and `GIT_SSH`.
    pub git_ssh_command: Option<Vec<u8>>,
    /// `GIT_SSH`, used when neither `GIT_SSH_COMMAND` nor `core.sshCommand` exists.
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

impl SshEnvironment {
    /// Sets [`Self::git_ssh_command`].
    #[must_use]
    pub fn with_git_ssh_command(mut self, git_ssh_command: Option<Vec<u8>>) -> Self {
        self.git_ssh_command = git_ssh_command;
        self
    }

    /// Sets [`Self::git_ssh`].
    #[must_use]
    pub fn with_git_ssh(mut self, git_ssh: Option<Vec<u8>>) -> Self {
        self.git_ssh = git_ssh;
        self
    }

    /// Sets [`Self::git_ssh_variant`].
    #[must_use]
    pub fn with_git_ssh_variant(mut self, git_ssh_variant: Option<Vec<u8>>) -> Self {
        self.git_ssh_variant = git_ssh_variant;
        self
    }

    /// Sets [`Self::approved_command`].
    #[must_use]
    pub fn with_approved_command(mut self, approved_command: Option<ApprovedSshCommand>) -> Self {
        self.approved_command = approved_command;
        self
    }

    /// Sets [`Self::default_executable`].
    #[must_use]
    pub fn with_default_executable(mut self, default_executable: PathBuf) -> Self {
        self.default_executable = default_executable;
        self
    }

    /// Sets [`Self::config_file`].
    #[must_use]
    pub fn with_config_file(mut self, config_file: PathBuf) -> Self {
        self.config_file = config_file;
        self
    }

    /// Sets [`Self::default_user`].
    #[must_use]
    pub fn with_default_user(mut self, default_user: String) -> Self {
        self.default_user = default_user;
        self
    }

    /// Sets [`Self::agent_socket`].
    #[must_use]
    pub fn with_agent_socket(mut self, agent_socket: Option<PathBuf>) -> Self {
        self.agent_socket = agent_socket;
        self
    }

    /// Sets [`Self::askpass`].
    #[must_use]
    pub fn with_askpass(mut self, askpass: Option<PathBuf>) -> Self {
        self.askpass = askpass;
        self
    }

    /// Sets [`Self::path_environment`].
    #[must_use]
    pub fn with_path_environment(mut self, path_environment: Option<OsString>) -> Self {
        self.path_environment = path_environment;
        self
    }
}

impl std::fmt::Debug for SshEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshEnvironment").finish_non_exhaustive()
    }
}

/// An SSH endpoint with explicit launch policy and redacted Debug output.
///
/// Use with [`crate::fetch::receive_ssh`] and [`crate::push::send_ssh`]. [`Self::openssh`]
/// delegates configuration and authentication policy to OpenSSH; the restrictions below apply to
/// [`Self::new`] and [`Self::configured`]. Components are literal:
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
/// [`Self::with_terminal`] opts ordinary OpenSSH into foreground terminal leasing; its paired
/// [`TerminalEvents`] must be driven independently of blocked display work. No default deadline
/// is added, including while waiting for input or a resume permit.
///
/// Stderr is drained and discarded by default to prevent blockage and credential/path leakage.
/// [`crate::fetch::discover_ssh_with_diagnostics`] explicitly opts into raw local stderr delivery.
/// Errors expose
/// only static categories, I/O kinds and exit codes; Git progress/status bytes remain untrusted.
/// An exit code of 255 indicates an SSH/service failure, not a Git ref rejection. OpenSSH cannot
/// reliably distinguish authentication, host trust and network failures without parsing stderr.
/// Debug and errors never include configuration values. Caller concurrency bounds total resources.
pub struct SshRemote {
    host: String,
    user: Option<String>,
    port: Option<u16>,
    path: String,
    executable: PathBuf,
    arguments: Vec<String>,
    policy: LaunchPolicy,
    terminal: Option<std::sync::Arc<ForegroundTerminal>>,
    protocol: super::RequestedProtocol,
}

enum LaunchPolicy {
    Restricted {
        config: PathBuf,
        agent_socket: Option<PathBuf>,
        askpass: Option<PathBuf>,
        path_environment: Option<OsString>,
    },
    OpenSsh(std::collections::BTreeMap<OsString, OsString>),
}

impl std::fmt::Debug for SshRemote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshRemote").finish_non_exhaustive()
    }
}

/// Sanitized process/SSH failure, distinct from Git protocol rejection reports.
///
/// [`Configuration`](Self::Configuration) errors are raised before any process starts. Once `ssh`
/// has run, an [`Exit`](Self::Exit) code of 255 can mean an authentication, host-key or network
/// failure; OpenSSH's standard error, delivered to the diagnostics callbacks, says which.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use girt::transport::ssh::{SshError, SshRemote};
///
/// // A relative executable path is refused before anything runs.
/// let result = SshRemote::new(
///     "git.example.com",
///     "git",
///     22,
///     "srv/project.git",
///     Path::new("ssh"),
///     Path::new("/etc/girt/ssh_config"),
/// );
/// assert!(matches!(result, Err(SshError::Configuration(_))));
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
        validate_endpoint(host, Some(user), Some(port), path)?;
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
            user: Some(user.into()),
            port: Some(port),
            path: path.into(),
            executable: executable.into(),
            arguments: Vec::new(),
            terminal: None,
            protocol: super::RequestedProtocol::V0,
            policy: LaunchPolicy::Restricted {
                config: config.into(),
                agent_socket: None,
                askpass: None,
                path_environment,
            },
        })
    }

    /// Resolves an SSH destination with an explicit command and authentication policy.
    ///
    /// Commands select `GIT_SSH_COMMAND`, then the last `core.sshCommand`, then `GIT_SSH`,
    /// then the trusted default executable. Empty commands and selected valueless config keys
    /// fail rather than falling through. Environment inputs override even valueless config keys.
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
        let variant = match environment.git_ssh_variant.as_deref() {
            Some(value) => Some(value),
            None => configured_value(config, "ssh", "variant")?,
        };
        if variant.is_some_and(|value| !value.eq_ignore_ascii_case(b"ssh")) {
            return Err(SshError::Configuration("SSH variant"));
        }
        let selected = match environment.git_ssh_command.as_deref() {
            Some(value) => Some(value),
            None => {
                configured_value(config, "core", "sshcommand")?.or(environment.git_ssh.as_deref())
            }
        };
        let (executable, arguments) = approve_command(
            selected,
            environment.approved_command,
            environment.default_executable,
        )?;
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
        if let LaunchPolicy::Restricted {
            agent_socket,
            askpass,
            ..
        } = &mut remote.policy
        {
            *agent_socket = environment.agent_socket;
            *askpass = environment.askpass;
        }
        Ok(remote)
    }

    /// Attaches exclusive foreground terminal ownership to an ordinary OpenSSH remote.
    ///
    /// The paired terminal event receiver must remain alive and be polled while the operation
    /// runs. See [`ForegroundTerminal`] for job-control and cleanup responsibilities. This opts in
    /// to local terminal access without changing OpenSSH authentication or host-key policy.
    ///
    /// # Errors
    ///
    /// Rejects restricted launch policy and replacing an existing terminal attachment.
    pub fn with_terminal(
        mut self,
        terminal: std::sync::Arc<ForegroundTerminal>,
    ) -> Result<Self, SshError> {
        if !matches!(self.policy, LaunchPolicy::OpenSsh(_)) || self.terminal.is_some() {
            return Err(SshError::Configuration(
                "terminal requires ordinary OpenSSH policy",
            ));
        }
        self.terminal = Some(terminal);
        Ok(self)
    }

    /// Selects the upload-pack protocol version requested through `GIT_PROTOCOL`.
    ///
    /// The default is v0, which sends nothing. For v1 or v2, upload-pack sessions set
    /// `GIT_PROTOCOL` for the local OpenSSH process and add `-o SendEnv=GIT_PROTOCOL`; the server
    /// must accept the variable (OpenSSH `AcceptEnv`). A server that does not receive it speaks
    /// v0, which is accepted. Receive-pack sessions never send it.
    #[must_use]
    pub fn with_protocol(mut self, protocol: super::RequestedProtocol) -> Self {
        self.protocol = protocol;
        self
    }

    /// The upload-pack protocol version this remote requests.
    pub fn protocol(&self) -> super::RequestedProtocol {
        self.protocol
    }

    /// Returns whether this remote uses an explicit foreground terminal attachment.
    pub fn has_terminal(&self) -> bool {
        self.terminal.is_some()
    }

    pub(crate) fn connect(
        &self,
        service: &str,
        control: TransportControl<'_>,
    ) -> Result<Session, SshError> {
        control.check()?;
        Session::spawn_with_terminal(&mut self.command(service), self.terminal.as_deref())
    }

    fn command(&self, service: &str) -> Command {
        let mut command = Command::new(&self.executable);
        command.env_clear();
        let version = self
            .protocol
            .parameter()
            .filter(|_| service == "git-upload-pack");
        match &self.policy {
            LaunchPolicy::Restricted {
                config,
                agent_socket,
                askpass,
                path_environment,
            } => {
                // OpenSSH config can explicitly invoke programs. Pass only the caller-selected
                // search path and authentication inputs; never inherit loader or
                // Git-protocol variables.
                if let Some(path) = &path_environment {
                    command.env("PATH", path);
                }
                if let Some(socket) = &agent_socket {
                    command.env("SSH_AUTH_SOCK", socket);
                }
                if let Some(askpass) = &askpass {
                    command.env("SSH_ASKPASS", askpass);
                    command.env("SSH_ASKPASS_REQUIRE", "force");
                    command.env("DISPLAY", "girt-askpass");
                } else {
                    command.env("SSH_ASKPASS_REQUIRE", "never");
                }
                command.args(["-T", "-F"]).arg(config);
                command.args([
                    "-o",
                    if askpass.is_some() {
                        "BatchMode=no"
                    } else {
                        "BatchMode=yes"
                    },
                ]);
                command.args([
                    "-o",
                    if agent_socket.is_some() {
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

                if let Some(version) = version {
                    command.env("GIT_PROTOCOL", version);
                    command.args(["-o", "SendEnv=GIT_PROTOCOL"]);
                }
                command.args(&self.arguments);
                command.args(["-p", &self.port.expect("restricted port").to_string(), "-l"]);
                command.arg(self.user.as_deref().expect("restricted user"));
                command.args(["--", &self.host]);
            }
            LaunchPolicy::OpenSsh(environment) => {
                command.envs(environment);
                // An approved command is `/bin/sh -c '<command> "$@"'`; options follow it.
                command.args(&self.arguments);
                if let Some(version) = version {
                    command.env("GIT_PROTOCOL", version);
                    command.args(["-o", "SendEnv=GIT_PROTOCOL"]);
                }
                if let Some(port) = self.port {
                    command.args(["-p", &port.to_string()]);
                }
                match &self.user {
                    Some(user) => {
                        command.arg(format!("{user}@{}", self.host));
                    }
                    None => {
                        command.arg(&self.host);
                    }
                }
            }
        }
        command.arg(format!("{service} '{}'", self.path.replace('\'', "'\\''")));
        command
    }
}
// A present key without a value is invalid, rather than permission to use a lower-priority input.
fn configured_value<'a>(
    config: &'a Config,
    section: &'a str,
    key: &'a str,
) -> Result<Option<&'a [u8]>, SshError> {
    match config.values(section, None, key).last() {
        Some(None) => Err(SshError::Configuration(
            "SSH configuration requires a value",
        )),
        value => Ok(value.flatten()),
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
    let (host, user, port, path) = parse_endpoint(raw)?;
    Ok((
        host,
        user.unwrap_or_else(|| default_user.to_owned()),
        port.unwrap_or(22),
        path,
    ))
}

// Keep omission distinct from explicit endpoint choices for ordinary OpenSSH configuration.
fn parse_endpoint(raw: &[u8]) -> Result<(String, Option<String>, Option<u16>, String), SshError> {
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
        .map_or((None, authority), |(user, host)| {
            (Some(user.to_owned()), host)
        });
    if authority.matches('@').count() > 1 || raw.contains(['?', '#', '%']) {
        return Err(SshError::Configuration("SSH endpoint syntax"));
    }
    let (host, port) = if url {
        if let Some(bracketed) = host_port.strip_prefix('[') {
            let (host, suffix) = bracketed
                .split_once(']')
                .ok_or(SshError::Configuration("SSH host"))?;
            let port = if suffix.is_empty() {
                None
            } else {
                Some(parse_port(
                    suffix
                        .strip_prefix(':')
                        .ok_or(SshError::Configuration("SSH host"))?,
                )?)
            };
            (host, port)
        } else if let Some((host, port)) = host_port.split_once(':') {
            (host, Some(parse_port(port)?))
        } else {
            (host_port, None)
        }
    } else {
        (host_port, None)
    };
    if host.contains(['[', ']', ':']) && host.parse::<std::net::Ipv6Addr>().is_err() {
        return Err(SshError::Configuration("SSH host"));
    }
    Ok((host.to_owned(), user, port, path))
}

fn parse_port(port: &str) -> Result<u16, SshError> {
    port.parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or(SshError::Configuration("SSH port"))
}

#[cfg(test)]
mod tests;

fn validate_endpoint(
    host: &str,
    user: Option<&str>,
    port: Option<u16>,
    path: &str,
) -> Result<(), SshError> {
    let host_name = safe_name(host) && !host.contains('_');
    if host.len() > 255 || !(host_name || host.parse::<std::net::Ipv6Addr>().is_ok()) {
        return Err(SshError::Configuration("host"));
    }
    if user.is_some_and(|user| user.len() > 255 || !safe_name(user)) {
        return Err(SshError::Configuration("user"));
    }
    if port == Some(0) {
        return Err(SshError::Configuration("port"));
    }
    if path.is_empty()
        || path.len() > 8192
        || path.starts_with(['-', '~'])
        || path.chars().any(char::is_control)
    {
        return Err(SshError::Configuration("repository path"));
    }
    Ok(())
}

fn approve_command(
    selected: Option<&[u8]>,
    approved_command: Option<ApprovedSshCommand>,
    default_executable: PathBuf,
) -> Result<(PathBuf, Vec<String>), SshError> {
    match selected {
        Some(value) => {
            if value.is_empty() {
                return Err(SshError::Configuration("empty SSH command"));
            }
            let approved = approved_command.ok_or(SshError::Configuration(
                "SSH command requires application approval",
            ))?;
            if approved.configured != value {
                return Err(SshError::Configuration("SSH command approval mismatch"));
            }
            if approved
                .arguments
                .iter()
                .any(|arg| arg.is_empty() || arg.len() > 8192 || arg.chars().any(char::is_control))
            {
                return Err(SshError::Configuration("SSH command argument"));
            }
            Ok((approved.executable, approved.arguments))
        }
        None => {
            if approved_command.is_some() {
                return Err(SshError::Configuration("unused SSH command approval"));
            }
            Ok((default_executable, Vec::new()))
        }
    }
}
