use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;

use super::{PreparedPush, PushError, PushFailure, PushReport, send};
use crate::packet::check_cancelled;

/// Sends to a trusted explicit local repository using Git receive-pack as the server.
///
/// Canonicalizes the destination path and starts `git` from PATH with an empty environment except
/// PATH and system/global configuration disabled. Uses binary stdin/stdout pipes, inherits stderr,
/// and forces v0. All client framing, policy, graph selection and pack writing are girt code.
/// URLs, SSH, HTTP and credentials are not accepted. Trust both the executable and the destination
/// configuration/hooks: the server may run hooks and update its refs/reflogs as usual.
///
/// Cancellation has [`send`]'s between-I/O granularity. There is no idle or wall-clock timeout;
/// receive-pack and hooks can block. On error or unwinding the direct child is killed and reaped;
/// descendant process-group termination is not promised. Killing the process cannot roll back
/// already committed ref updates. A complete report is retained if waiting for the child fails.
///
/// # Errors
///
/// Path/spawn/pipe failures are [`PushError::NotSent`]. After transmission, returns [`send`]'s
/// classification and status evidence. Unsuccessful child exit or wait failure is uncertain, even
/// with a complete report. Complete server rejections normally return `Ok` with rejected statuses.
pub fn send_local(
    destination: impl AsRef<Path>,
    prepared: &PreparedPush,
    cancel: &AtomicBool,
) -> Result<PushReport, PushError> {
    check_cancelled(cancel).map_err(|e| PushError::NotSent(e.into()))?;
    let destination =
        std::fs::canonicalize(destination).map_err(|e| PushError::NotSent(e.into()))?;
    let mut command = Command::new("git");
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .args(["-c", "protocol.version=0", "receive-pack"])
        .arg(destination)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = Server(command.spawn().map_err(|e| PushError::NotSent(e.into()))?);
    let mut reader = child.0.stdout.take().expect("piped stdout");
    let mut writer = child.0.stdin.take().expect("piped stdin");
    let report = send(&mut reader, &mut writer, prepared, cancel)?;
    drop(writer);
    drop(reader);
    let result = child
        .0
        .wait()
        .map_err(PushFailure::from)
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(PushFailure::Process(status))
            }
        });
    match result {
        Ok(()) => Ok(report),
        Err(cause) if prepared.commands.is_empty() => Err(PushError::NotSent(cause)),
        Err(cause) => Err(PushError::Uncertain {
            cause,
            report: Box::new(report),
        }),
    }
}
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
