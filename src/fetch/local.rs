use std::ops::ControlFlow;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use super::{
    Advertisement, FetchError, FetchLimits, KnownHistory, ReceivedFetch, receive_with_known,
};
use crate::ObjectId;
use crate::transport::{Server, TransportControl};

/// Receives from a local repository through `git upload-pack`, without invoking a Git client.
///
/// Canonicalizes the explicit repository path, starts `git` from PATH with an empty environment
/// except PATH and system configuration disabled, forces v0, and uses binary stdin/stdout pipes.
/// The caller must trust the executable and source repository/configuration. This adapter does
/// not accept URLs or perform authentication. All client framing, pack decoding, indexing, and
/// validation are implemented in girt.
///
/// Uses interruptible owned pipes on macOS/Linux, with no deadline. See [`TransportControl`] for
/// cancellation, diagnostic disposal, process-group cleanup, and platform requirements. Use
/// [`receive_local_with_control`] to impose an absolute transport deadline.
///
/// # Errors
///
/// Returns [`super::receive`]'s failures, path/spawn/pipe errors, or an unsuccessful server exit
/// status. Waits for a successful exit before returning validated objects. No destination is
/// touched.
pub fn receive_local(
    source: impl AsRef<Path>,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    limits: FetchLimits,
    cancel: &AtomicBool,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, FetchError> {
    receive_local_with_control(
        source,
        select,
        limits,
        TransportControl::new(cancel),
        progress,
    )
}

/// Receives objects from a local server with caller-controlled interruption.
///
/// Same protocol and trust contract as [`receive_local`]. See [`TransportControl`] for deadline
/// scope, races, cleanup, and OS support. No destination is touched, even on interruption.
///
/// # Errors
///
/// Returns [`receive_local`]'s failures, [`FetchError::Cancelled`], or [`FetchError::Deadline`].
pub fn receive_local_with_control(
    source: impl AsRef<Path>,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    limits: FetchLimits,
    control: TransportControl<'_>,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, FetchError> {
    receive_local_with_known(
        source,
        select,
        &KnownHistory::default(),
        limits,
        control,
        progress,
    )
}

/// Negotiates a local fetch using verified local history and owned transport controls.
///
/// Combines [`super::receive_with_known`]'s knowledge and validation contract with
/// [`receive_local_with_control`]'s process, deadline and trust contract. Knowledge preparation
/// is separate and is not covered by the transport deadline.
///
/// # Errors
///
/// Returns protocol/knowledge validation, interruption, path, process and storage failures.
pub fn receive_local_with_known(
    source: impl AsRef<Path>,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: &KnownHistory,
    limits: FetchLimits,
    control: TransportControl<'_>,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, FetchError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "fetch.local",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
    );

    let operation = || {
        control.check()?;
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
            .args(["-c", "protocol.version=0", "upload-pack", "--strict"])
            .arg(source);
        receive_server_with_known(&mut command, select, known, limits, control, progress)
    };
    #[cfg(feature = "tracing")]
    let result = span.in_scope(operation);
    #[cfg(not(feature = "tracing"))]
    let result = { operation }();
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, crate::trace::fetch);

    result
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
fn receive_server(
    command: &mut Command,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    limits: FetchLimits,
    control: TransportControl<'_>,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, FetchError> {
    receive_server_with_known(
        command,
        select,
        &KnownHistory::default(),
        limits,
        control,
        progress,
    )
}

fn receive_server_with_known(
    command: &mut Command,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: &KnownHistory,
    limits: FetchLimits,
    control: TransportControl<'_>,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, FetchError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "fetch.local_server",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
    );

    let operation = || {
        let mut child = Server::spawn(command, control)?;
        let received = {
            let (mut reader, mut writer) = child.streams();
            receive_with_known(
                &mut reader,
                &mut writer,
                select,
                known,
                limits,
                control.cancel,
                progress,
            )?
        };
        let status = child.wait()?;
        if !status.success() {
            return Err(FetchError::Process(status));
        }
        Ok(received)
    };
    #[cfg(feature = "tracing")]
    let result = span.in_scope(operation);
    #[cfg(not(feature = "tracing"))]
    let result = { operation }();
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, crate::trace::fetch);

    result
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests;
