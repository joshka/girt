use std::ops::ControlFlow;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;

use super::{Advertisement, FetchError, FetchLimits, ReceivedFetch, check_cancelled, receive};
use crate::ObjectId;

/// Receives from a local repository through `git upload-pack`, without invoking a Git client.
///
/// Canonicalizes the explicit repository path, starts `git` from PATH with an empty environment
/// except PATH and system configuration disabled, forces v0, and uses binary stdin/stdout pipes.
/// The server has a 30-second idle timeout. Server stderr is inherited; protocol progress is sent
/// to the callback. The caller must trust the executable and source repository/configuration.
/// This adapter does not accept URLs or perform authentication. All client framing, pack decoding,
/// indexing, and validation are implemented in girt.
///
/// Cancellation has the same between-I/O granularity as [`receive`]; a blocked local pipe read
/// cannot be interrupted by setting the flag alone. On ordinary failure or unwinding the direct
/// upload-pack child is killed and reaped. This does not promise process-group cancellation or a
/// wall-clock deadline for a server performing computation.
///
/// # Errors
///
/// Returns [`receive`]'s failures, path/spawn/pipe errors, or an unsuccessful server exit status.
/// Waits for a successful exit before returning validated objects. No destination is touched.
pub fn receive_local(
    source: impl AsRef<Path>,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    limits: FetchLimits,
    cancel: &AtomicBool,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, FetchError> {
    check_cancelled(cancel)?;
    let source = std::fs::canonicalize(source)?;
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
        .args([
            "-c",
            "protocol.version=0",
            "upload-pack",
            "--strict",
            "--timeout=30",
        ])
        .arg(source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = Server(command.spawn()?);
    let mut reader = child.0.stdout.take().expect("piped stdout");
    let mut writer = child.0.stdin.take().expect("piped stdin");
    let received = receive(&mut reader, &mut writer, select, limits, cancel, progress)?;
    drop(writer);
    drop(reader);
    let status = child.0.wait()?;
    if !status.success() {
        return Err(FetchError::Process(status));
    }
    check_cancelled(cancel)?;
    Ok(received)
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
