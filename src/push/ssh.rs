use std::sync::atomic::AtomicBool;

use super::{PreparedPush, PushError, PushFailure, PushReport, protocol};
use crate::packet::Wire;
use crate::transport::TransportControl;
use crate::transport::ssh::SshRemote;

/// Sends a synchronously prepared push through one async SSH receive-pack session.
///
/// Requires a caller-owned Tokio runtime with I/O/time enabled. [`SshRemote`] defines endpoint,
/// authentication, host trust and noninteractive process policy. Uses [`super::send`]'s live
/// expectation checks, history exclusion, bounded report-status and non-atomic update semantics.
/// Borrows existing prepared buffers without copying packs or doing compression on the executor.
/// Preparation and remote inspection before retry remain caller-owned synchronous work.
///
/// Advertisement/status retention is bounded by the prepared limits. Valid acknowledgement
/// prefixes survive cancellation, truncation and unsuccessful SSH exit, including complete reports
/// awaiting EOF/exit. Dropping the future cleans up local processes but loses the classified
/// result; prefer setting cancellation and awaiting completion. Remote updates cannot be recalled.
///
/// # Errors
///
/// Failures before attempting update bytes are [`PushError::NotSent`]. After transmission starts,
/// failures are [`PushError::Uncertain`] with retained status evidence. Complete Git rejection
/// reports return `Ok`; inspect every ref. No retry is automatic, even after an SSH exit of 255.
///
/// # Panics
///
/// Panics when polled without a Tokio runtime with I/O and time enabled.
pub async fn send_ssh(
    remote: &SshRemote,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
) -> Result<PushReport, PushError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "push.ssh",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
        accepted = tracing::field::Empty,
        rejected = tracing::field::Empty,
        pending = tracing::field::Empty,
        unpack = tracing::field::Empty,
    );

    let operation = async {
        let mut session = remote
            .connect("git-receive-pack", control)
            .map_err(|e| PushError::NotSent(e.into()))?;
        let preflight = async {
            let bytes = session
                .advertise(prepared.limits.max_advertisement_bytes, control)
                .await?;
            let mut reader = bytes.as_slice();
            let mut wire = Wire {
                reader: &mut reader,
                remaining: prepared.limits.max_advertisement_bytes,
                cancel: control.cancel,
            };
            protocol::advertise(&mut wire, prepared)?;
            wire.end()?;
            control.check()?;
            Ok::<_, PushFailure>(())
        };
        preflight.await.map_err(PushError::NotSent)?;
        let mut report = PushReport::pending(&prepared.commands);
        let request = if prepared.commands.is_empty() {
            b"0000"
        } else {
            prepared.request.as_slice()
        };
        let (body, result, attempted) = session
            .exchange(
                request,
                &prepared.pack,
                prepared.limits.max_status_bytes,
                control,
            )
            .await;
        if prepared.commands.is_empty() {
            result.map_err(|e| PushError::NotSent(e.into()))?;
            if !body.is_empty() {
                return Err(PushError::NotSent(PushFailure::Protocol(
                    "trailing response bytes",
                )));
            }
            return Ok(report);
        }
        if !attempted && let Err(cause) = result {
            return Err(PushError::NotSent(cause.into()));
        }
        let retain = AtomicBool::new(false);
        let mut reader = body.as_slice();
        let mut wire = Wire {
            reader: &mut reader,
            remaining: prepared.limits.max_status_bytes,
            cancel: &retain,
        };
        let parsed = protocol::read_status(&mut wire, &mut report)
            .and_then(|()| wire.end().map_err(PushFailure::from));
        match result.map_err(PushFailure::from).and(parsed) {
            Ok(()) => Ok(report),
            Err(cause) => Err(PushError::Uncertain {
                cause,
                report: Box::new(report),
            }),
        }
    };
    #[cfg(feature = "tracing")]
    let result = tracing::Instrument::instrument(operation, span.clone()).await;
    #[cfg(not(feature = "tracing"))]
    let result = operation.await;
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, |error| crate::trace::push(error, &span));
    #[cfg(feature = "tracing")]
    if let Ok(report) = &result {
        crate::trace::push_report(&span, report);
    }

    result
}
