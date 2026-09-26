use std::path::Path;
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use std::process::Command;
use std::sync::atomic::AtomicBool;

use super::{PreparedPush, PushError, PushReport};
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use super::{PushFailure, protocol};
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use crate::packet::Wire;
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use crate::transport::Server;
use crate::transport::TransportControl;

/// Installs a prepared pack and conditionally updates a trusted local repository with girt.
///
/// Opens an explicit path through [`crate::Repository::open`], checks expected ref values and
/// receiver-history dependencies, installs the pack, then applies each command with a conditional
/// reference transaction. It invokes no Git executable or hook. Existing receive hooks or a
/// configured hooks path cause refusal before publication; checked-out branches are rejected.
/// Use [`crate::remote::Destination::local_path`] for local or `file://` destinations.
///
/// Supports either object format and files or reftable references when prepared by
/// [`PreparedPush::new_local`]. [`PreparedPush::new`] remains SHA-1 wire preparation. Ref and
/// worktree state may change between separate reads; conditional locks prevent stale overwrites,
/// and callers must coordinate worktree registration and GC. No reflogs are appended. Object
/// installation can leave an indexed pack when later refs reject or fail.
///
/// # Errors
///
/// Preflight and object installation failures are [`PushError::NotSent`] for ref effects. A
/// reference preparation failure is a per-ref rejection, allowing other refs to succeed. A
/// publication failure or interruption after installation is [`PushError::Uncertain`] with
/// completed status evidence; inspect the destination before retrying.
pub fn send_local(
    destination: impl AsRef<Path>,
    prepared: &PreparedPush,
    cancel: &AtomicBool,
) -> Result<PushReport, PushError> {
    send_local_with_control(destination, prepared, TransportControl::new(cancel))
}

/// Sends a prepared push with caller-controlled transport interruption.
///
/// Uses [`send_local`]'s storage contract. The deadline does not cover push preparation or
/// guarantee interruption during one synchronous filesystem or compression operation.
///
/// # Errors
///
/// Before installation interruption is [`PushError::NotSent`]. After installation it is
/// [`PushError::Uncertain`], retaining completed per-ref results; unknown refs may have changed.
/// The cause distinguishes [`super::PushFailure::Cancelled`] and
/// [`super::PushFailure::Deadline`] from rejection.
pub fn send_local_with_control(
    destination: impl AsRef<Path>,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
) -> Result<PushReport, PushError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "push.local",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
        accepted = tracing::field::Empty,
        rejected = tracing::field::Empty,
        pending = tracing::field::Empty,
        unpack = tracing::field::Empty,
    );

    let operation = || super::local_native::send(destination.as_ref(), prepared, control);
    #[cfg(feature = "tracing")]
    let result = span.in_scope(operation);
    #[cfg(not(feature = "tracing"))]
    let result = { operation }();
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, |error| crate::trace::push(error, &span));
    #[cfg(feature = "tracing")]
    if let Ok(report) = &result {
        crate::trace::push_report(&span, report);
    }

    result
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
fn send_server(
    command: &mut Command,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
) -> Result<PushReport, PushError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "push.local_server",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
        accepted = tracing::field::Empty,
        rejected = tracing::field::Empty,
        pending = tracing::field::Empty,
        unpack = tracing::field::Empty,
    );

    let operation = || {
        let mut child =
            Server::spawn(command, control).map_err(|e| PushError::NotSent(e.into()))?;
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
    };
    #[cfg(feature = "tracing")]
    let result = span.in_scope(operation);
    #[cfg(not(feature = "tracing"))]
    let result = { operation }();
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, |error| crate::trace::push(error, &span));
    #[cfg(feature = "tracing")]
    if let Ok(report) = &result {
        crate::trace::push_report(&span, report);
    }

    result
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests;
