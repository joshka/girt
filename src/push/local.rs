use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use super::{PreparedPush, PushError, PushFailure, PushReport, protocol};
use crate::packet::Wire;
use crate::transport::{Server, TransportControl};

/// Sends to a trusted explicit local repository using Git receive-pack as the server.
///
/// Canonicalizes the destination path and starts `git` from PATH with an empty environment except
/// PATH and system/global configuration disabled. Uses binary stdin/stdout pipes, drains and
/// discards stderr, and forces v0. All client framing, policy, graph selection and pack writing are
/// girt code. URLs, SSH, HTTP and credentials are not accepted. Trust both the executable and the
/// destination configuration/hooks: the server may run hooks and update its refs/reflogs as usual.
///
/// Uses interruptible owned pipes on macOS/Linux, with no deadline. See [`TransportControl`] for
/// cancellation, diagnostic disposal, process-group cleanup, and platform requirements. Use
/// [`send_local_with_control`] to impose an absolute deadline. Killing the server cannot roll
/// back committed updates. Upload drains status concurrently, retaining at most
/// [`super::PushLimits::max_status_bytes`] for parsing after I/O completes or fails. A valid
/// acknowledgement prefix survives interruption; a complete report survives child-wait failure.
///
/// # Errors
///
/// Path/spawn/pipe failures are [`PushError::NotSent`]. After transmission, returns
/// [`super::send`]'s classification and status evidence. Unsuccessful child exit or wait failure is
/// uncertain, even with a complete report. Complete server rejections normally return `Ok` with
/// rejected statuses.
pub fn send_local(
    destination: impl AsRef<Path>,
    prepared: &PreparedPush,
    cancel: &AtomicBool,
) -> Result<PushReport, PushError> {
    send_local_with_control(destination, prepared, TransportControl::new(cancel))
}

/// Sends a prepared push with caller-controlled transport interruption.
///
/// Same protocol and trust contract as [`send_local`]. See [`TransportControl`] for deadline
/// scope, races, cleanup, and OS support. The deadline does not cover push preparation.
///
/// # Errors
///
/// Before transmission interruption is [`PushError::NotSent`]. Once commands are attempted it is
/// [`PushError::Uncertain`], retaining valid acknowledgements; unknown refs may have changed.
/// The cause distinguishes [`PushFailure::Cancelled`] and [`PushFailure::Deadline`] from rejection.
pub fn send_local_with_control(
    destination: impl AsRef<Path>,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
) -> Result<PushReport, PushError> {
    control.check().map_err(|e| PushError::NotSent(e.into()))?;
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
        .arg(destination);
    send_server(&mut command, prepared, control)
}

fn send_server(
    command: &mut Command,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
) -> Result<PushReport, PushError> {
    let mut child = Server::spawn(command, control).map_err(|e| PushError::NotSent(e.into()))?;
    let (body, exchange, attempted) = {
        let (mut reader, writer) = child.streams();
        let mut wire = Wire {
            reader: &mut reader,
            remaining: prepared.limits.max_advertisement_bytes,
            cancel: control.cancel,
        };
        protocol::advertise(&mut wire, prepared).map_err(PushError::NotSent)?;
        control.check().map_err(|e| PushError::NotSent(e.into()))?;
        Server::exchange(
            reader,
            writer,
            &prepared.request,
            &prepared.pack,
            prepared.limits.max_status_bytes,
        )
    };
    if !attempted && let Err(cause) = exchange {
        return Err(PushError::NotSent(cause.into()));
    }
    let mut report = PushReport::pending(&prepared.commands);
    // Parse retained bytes even after cancellation; no further I/O or mutation occurs here.
    let retain = AtomicBool::new(false);
    let mut reader = body.as_slice();
    let mut wire = Wire {
        reader: &mut reader,
        remaining: prepared.limits.max_status_bytes,
        cancel: &retain,
    };
    let parsed = if prepared.commands.is_empty() {
        wire.end().map_err(PushFailure::from)
    } else {
        protocol::read_status(&mut wire, &mut report)
            .and_then(|()| wire.end().map_err(PushFailure::from))
    };
    let transfer = exchange.map_err(PushFailure::from).and(parsed);
    if let Err(cause) = transfer {
        return if prepared.commands.is_empty() {
            Err(PushError::NotSent(cause))
        } else {
            Err(PushError::Uncertain {
                cause,
                report: Box::new(report),
            })
        };
    }
    let result = child.wait().map_err(PushFailure::from).and_then(|status| {
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

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests;
