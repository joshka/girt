use std::sync::atomic::AtomicBool;

use super::live_progress::LiveProgress;
use super::{PreparedPush, PushAdvertisement, PushError, PushFailure, PushReport, protocol};
use crate::packet::Wire;
use crate::transport::TransportControl;
use crate::transport::ssh::SshRemote;

/// Outcome of an SSH push whose caller inspected the live receive-pack advertisement.
#[derive(Debug)]
pub enum SshPushOutcome {
    /// The caller declined. Only a protocol flush was sent; the session exited successfully.
    /// Authentication, trust or proxy side effects may already have occurred. Do not automatically
    /// retry or start another transport solely because no update command was sent.
    Declined,
    /// The accepted operation completed with a report; inspect every reference status.
    Sent(PushReport),
}

/// Sends a synchronously prepared push through one async SSH receive-pack session.
///
/// Requires a caller-owned Tokio runtime with I/O/time enabled. [`SshRemote`] defines endpoint,
/// authentication, host trust and noninteractive process policy. Uses [`super::send`]'s exact
/// per-command old values, history exclusion, bounded report-status and non-atomic update
/// semantics. Borrows existing prepared buffers without copying packs or doing compression on the
/// executor. Preparation and remote inspection before retry remain caller-owned synchronous work.
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
    match send_ssh_checked_with_progress(remote, prepared, control, |_| true, |_| {}, |_| {})
        .await?
    {
        SshPushOutcome::Sent(report) => Ok(report),
        SshPushOutcome::Declined => unreachable!("unconditional SSH push cannot decline"),
    }
}

/// Sends an SSH push after checking the live advertisement, with separate local and remote output.
///
/// After bounded advertisement and capability validation, `should_send` inspects the live tips.
/// Returning false sends only an empty selection flush, closes stdin and awaits the same session's
/// exit. Returning true preserves [`send_ssh`]'s exact old-value commands; the receiver must still
/// enforce them because advertised refs can change. Neither outcome permits an automatic retry.
///
/// `diagnostics` receives raw local stderr according to
/// [`crate::fetch::discover_ssh_with_diagnostics`]'s display and redaction contract. Call
/// [`PreparedPush::with_progress`] to request sideband; when negotiated, `progress` receives each
/// complete channel-2 payload once, in order, before response completion. Malformed/terminal
/// framing stops notices and the final parser determines the result. No bytes beyond the status
/// budget reach the progress callback. Progress is retained in the bounded report even on
/// uncertainty. Both callbacks must return promptly; they do not select retry or transport
/// outcomes.
///
/// # Errors
///
/// Preflight, decline-cleanup and failures before attempting update bytes are
/// [`PushError::NotSent`]. Authentication or trust effects may still have occurred. After update
/// transmission starts, failures retain acknowledgement evidence in [`PushError::Uncertain`].
/// Complete receiver rejections are returned in [`SshPushOutcome::Sent`] and must be inspected per
/// reference.
pub async fn send_ssh_checked_with_progress(
    remote: &SshRemote,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
    should_send: impl FnOnce(&PushAdvertisement) -> bool,
    diagnostics: impl FnMut(&[u8]),
    progress: impl FnMut(&[u8]),
) -> Result<SshPushOutcome, PushError> {
    send_selected(
        remote,
        prepared,
        control,
        |advertisement, offered| {
            // Retain the checked sender's all-command validation before its boolean callback.
            offered.validate(prepared, &prepared.commands, control.cancel)?;
            Ok(should_send(advertisement).then(|| prepared.commands.clone()))
        },
        diagnostics,
        progress,
    )
    .await
}

/// Sends only selected prepared commands through one SSH receive-pack session.
///
/// After bounded advertisement parsing and object-format validation, `select` returns prepared
/// destination names to submit. Each name must occur once and belong to [`PreparedPush::commands`].
/// Selection preserves original command order and exact expected/new IDs, regardless of callback
/// order. Required report-status, delete-refs and push-options capabilities are checked for the
/// submitted subset after selection. The receiver still enforces each command's expected old ID.
///
/// A nondeletion selection borrows the original prepared pack without copying or recompression;
/// the pack may contain objects for omitted commands and retains its receiver-root requirements.
/// Empty and deletion-only selections send no pack. Empty selection sends only a flush, closes
/// stdin, awaits exit and returns [`SshPushOutcome::Sent`] with an empty report. This function
/// never returns [`SshPushOutcome::Declined`]. Normal and uncertain reports contain only submitted
/// refs.
///
/// `diagnostics` and `progress` follow [`send_ssh_checked_with_progress`]'s raw-output, retention
/// and prompt-callback contracts. Selection itself must also return promptly. Authentication,
/// trust and proxy effects may precede selection; no outcome authorizes an automatic retry.
///
/// # Errors
///
/// Invalid or duplicate selections, missing required capabilities and failures before attempted
/// update bytes return [`PushError::NotSent`] after local session cleanup. Once transmission
/// starts, failures return [`PushError::Uncertain`] with submitted-command acknowledgement
/// evidence. Empty-selection cleanup failures are `NotSent`; complete receiver rejection remains a
/// report.
pub async fn send_ssh_selected_with_progress(
    remote: &SshRemote,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
    select: impl FnOnce(&PushAdvertisement) -> Vec<crate::refs::RefName>,
    diagnostics: impl FnMut(&[u8]),
    progress: impl FnMut(&[u8]),
) -> Result<SshPushOutcome, PushError> {
    send_selected(
        remote,
        prepared,
        control,
        |advertisement, offered| {
            offered.validate_format(prepared.format)?;
            prepared.selected_commands(select(advertisement)).map(Some)
        },
        diagnostics,
        progress,
    )
    .await
}

async fn send_selected(
    remote: &SshRemote,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
    select: impl FnOnce(
        &PushAdvertisement,
        &protocol::AdvertisedCapabilities,
    ) -> Result<Option<Vec<super::PushCommand>>, PushFailure>,
    mut diagnostics: impl FnMut(&[u8]),
    mut progress: impl FnMut(&[u8]),
) -> Result<SshPushOutcome, PushError> {
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
                .advertise_with_diagnostics(
                    prepared.limits.max_advertisement_bytes,
                    control,
                    &mut diagnostics,
                )
                .await?;
            let mut reader = bytes.as_slice();
            let mut wire = Wire {
                reader: &mut reader,
                remaining: prepared.limits.max_advertisement_bytes,
                cancel: control.cancel,
            };
            let (caps, advertisement) = protocol::advertise_for_selection(&mut wire, prepared)?;
            wire.end()?;
            control.check()?;
            Ok::<_, PushFailure>((caps, advertisement))
        };
        let (offered, advertisement) = preflight.await.map_err(PushError::NotSent)?;
        let commands = select(&advertisement, &offered).map_err(PushError::NotSent)?;
        let Some(commands) = commands else {
            let (body, result, _) = session
                .exchange_with_diagnostics(
                    b"0000",
                    &[],
                    prepared.limits.max_status_bytes,
                    control,
                    &mut diagnostics,
                )
                .await;
            result.map_err(|error| PushError::NotSent(error.into()))?;
            if !body.is_empty() {
                return Err(PushError::NotSent(PushFailure::Protocol(
                    "trailing declined response bytes",
                )));
            }
            return Ok(SshPushOutcome::Declined);
        };
        let caps = offered
            .validate(prepared, &commands, control.cancel)
            .map_err(PushError::NotSent)?;
        let negotiated = prepared
            .selected_request(&commands, caps.report_v2, caps.sideband, control.cancel)
            .map_err(PushError::NotSent)?;
        let mut report = PushReport::pending(&commands);
        let request: &[u8] = if commands.is_empty() {
            b"0000"
        } else {
            negotiated.as_ref()
        };
        let mut sideband = LiveProgress::default();
        let (body, result, written) = session
            .exchange_observed(
                request,
                if commands.iter().all(super::PushCommand::deletes) {
                    &[]
                } else {
                    &prepared.pack
                },
                prepared.limits.max_status_bytes,
                control,
                &mut diagnostics,
                &mut |bytes| {
                    if caps.sideband && !commands.is_empty() {
                        sideband.observe(bytes, &mut progress);
                    }
                },
            )
            .await;
        if commands.is_empty() {
            result.map_err(|e| PushError::NotSent(e.into()))?;
            if !body.is_empty() {
                return Err(PushError::NotSent(PushFailure::Protocol(
                    "trailing response bytes",
                )));
            }
            return Ok(SshPushOutcome::Sent(report));
        }
        if written == 0
            && let Err(cause) = result
        {
            return Err(PushError::NotSent(cause.into()));
        }
        report.mark_attempted(request, written);
        let retain = AtomicBool::new(false);
        let mut reader = body.as_slice();
        let mut wire = Wire {
            reader: &mut reader,
            remaining: prepared.limits.max_status_bytes,
            cancel: &retain,
        };
        let parsed = protocol::read_response(
            &mut wire,
            &mut report,
            caps,
            prepared.limits.max_status_bytes,
        )
        .and_then(|()| wire.end().map_err(PushFailure::from));
        match result.map_err(PushFailure::from).and(parsed) {
            Ok(()) => Ok(SshPushOutcome::Sent(report)),
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
    if let Ok(SshPushOutcome::Sent(report)) = &result {
        crate::trace::push_report(&span, report);
    }

    result
}
