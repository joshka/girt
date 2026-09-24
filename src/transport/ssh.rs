//! Async protocol v0 over a caller-selected system OpenSSH executable and configuration.
//!
//! Available with `ssh` on macOS/Linux. No runtime, credential discovery, keychain or cryptography
//! is implemented here. Callers own a Tokio runtime with I/O/time enabled and trust the executable,
//! config (including includes and Match exec), remote account and service. See [`SshRemote`].

mod process;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) use process::Session;

use super::TransportControl;

/// Explicit repository components and trusted OpenSSH configuration, with redacted Debug output.
///
/// Use with [`crate::fetch::receive_ssh`] and [`crate::push::send_ssh`]. Components are literal:
/// host is a DNS name, IPv4 or bare IPv6 address; user is explicit; port is nonzero; path is a
/// UTF-8 absolute or account-relative path. URL/scp syntax, percent decoding and tilde expansion
/// are not supported. Spaces, quotes and shell metacharacters in paths are preserved literally.
/// The server must use a POSIX-compatible shell and provide `git-upload-pack`/`git-receive-pack`.
///
/// The executable and `-F` configuration file must be explicit absolute paths. No `GIT_SSH*`,
/// environment options or arbitrary command arguments are consulted. Configuration may choose
/// public-key identity files and known-host files. Default identity filenames are disabled;
/// agent, password, keyboard-interactive and PKCS#11 discovery are disabled.
/// Security-key identities can still require hardware interaction; cancellation/deadlines bound
/// their waits. Encrypted keys requiring a prompt fail in batch mode. Caller config is trusted
/// executable policy: includes and Match exec can run local commands. Do not pass untrusted config
/// files.
///
/// Enforced command-line policy overrides config: strict host-key verification, batch mode,
/// no TTY, forwarding, proxies, local commands, multiplexing or backgrounding. Unknown/changed
/// keys fail; pre-provision trust in the selected known-host file. No host-key updates are made.
/// OpenSSH config may choose algorithms and trust authorities, but cannot disable strict checking.
/// The service command is fixed. Config environment directives are caller-owned and must not
/// request another Git protocol version. Server-side forced commands may impose
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
        })
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
        // OpenSSH config can explicitly invoke programs; retain only executable search, never
        // agent/askpass/loader/Git-protocol environment variables.
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        command.env("SSH_ASKPASS_REQUIRE", "never");
        command.args(["-T", "-F"]).arg(&self.config);
        for option in [
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "UpdateHostKeys=no",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "PreferredAuthentications=publickey",
            "IdentityFile=none",
            "IdentityAgent=none",
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

#[cfg(test)]
mod tests;
