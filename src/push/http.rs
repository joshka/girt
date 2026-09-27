use std::sync::atomic::AtomicBool;

use super::{PreparedPush, PushAdvertisement, PushError, PushFailure, PushReport, protocol};
use crate::packet::Wire;
use crate::transport::TransportControl;
use crate::transport::http::{HttpRemote, RequestBody};

/// Outcome of an HTTP push whose caller inspected the receive-pack advertisement.
#[derive(Debug)]
pub enum HttpPushOutcome {
    /// The caller declined before the receive-pack POST. No update command was sent.
    Declined,
    /// The accepted operation completed with a report. Empty batches send no POST.
    /// Inspect each reference for rejection or partial success.
    Sent(PushReport),
}

/// Sends a prepared push through smart-HTTP discovery and one receive-pack RPC.
///
/// Uses [`super::send`]'s expectation, receiver-root, report-status and non-atomic update
/// contracts. Pack compression and exclusion are selected during [`PreparedPush`] construction.
/// Discovery checks required capabilities before any POST; exact old values travel in each
/// command for the receiver to enforce. Empty commands perform discovery only.
/// Requires a Tokio runtime; [`HttpRemote`] defines TLS, explicit authentication and network
/// controls.
///
/// Consumes the prepared push, moving its command and pack buffers into the POST without copying
/// the pack. Prepare a new push for another attempt after inspecting uncertain outcomes. Response
/// retention is bounded by `max_status_bytes`; advertisement retention by
/// `max_advertisement_bytes`. Complete valid status packets received before a body interruption
/// remain report evidence. No automatic retries occur, including HTTP authentication challenges or
/// redirects.
///
/// # Errors
///
/// Discovery/preflight failures are [`PushError::NotSent`]. After starting the RPC, every failure
/// (including HTTP status, cancellation, truncation or invalid media type) is conservatively
/// [`PushError::Uncertain`]. Inspect remote refs before retrying. An HTTP failure alone never
/// proves rejection. Complete Git rejection reports return `Ok`; inspect each reference status.
pub async fn send_http(
    remote: &HttpRemote,
    prepared: PreparedPush,
    control: TransportControl<'_>,
) -> Result<PushReport, PushError> {
    match send_http_checked(remote, prepared, control, |_| true).await? {
        HttpPushOutcome::Sent(report) => Ok(report),
        HttpPushOutcome::Declined => unreachable!("unconditional HTTP push cannot decline"),
    }
}

/// Sends a prepared push only when the caller accepts the live receive-pack advertisement.
///
/// After bounded discovery and capability validation, `should_send` receives the validated
/// advertised tips. Returning `false` yields [`HttpPushOutcome::Declined`] without a POST or
/// receiver mutation; the caller can choose another transport. Returning `true` retains
/// [`send_http`]'s exact old-value commands and structured report. The callback runs synchronously
/// on the caller's async task and should return promptly. It cannot change the prepared commands.
/// Advertised values can change after the callback; the receiver still enforces each command's
/// expected old value. An absent advertised tip may also represent a hidden reference.
///
/// Preparation has already consumed local CPU and memory before this call. Discovery and declined
/// decisions do not install a pack. Once the POST begins, failures are
/// [`PushError::Uncertain`] and must not trigger an automatic retry or fallback.
///
/// # Errors
///
/// Discovery, capability, cancellation and request-encoding failures before the POST are
/// [`PushError::NotSent`]. Post-send failures retain the valid response prefix in
/// [`PushError::Uncertain`]. Complete receiver rejections are returned in
/// [`HttpPushOutcome::Sent`] and must be inspected per reference.
pub async fn send_http_checked(
    remote: &HttpRemote,
    mut prepared: PreparedPush,
    control: TransportControl<'_>,
    should_send: impl FnOnce(&PushAdvertisement) -> bool,
) -> Result<HttpPushOutcome, PushError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "push.http",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
        accepted = tracing::field::Empty,
        rejected = tracing::field::Empty,
        pending = tracing::field::Empty,
        unpack = tracing::field::Empty,
    );

    let operation = async {
        let preflight = async {
            control.check()?;
            let (bytes, _) = remote
                .discover(
                    "git-receive-pack",
                    prepared.limits.max_advertisement_bytes,
                    control,
                )
                .await?;
            let mut reader = bytes.as_slice();
            let mut wire = Wire {
                reader: &mut reader,
                remaining: prepared.limits.max_advertisement_bytes,
                cancel: control.cancel,
            };
            let (caps, advertisement) = protocol::advertise_with_refs(&mut wire, &prepared)?;
            wire.end()?;
            control.check()?;
            Ok::<_, PushFailure>((caps, advertisement))
        };
        let (caps, advertisement) = preflight.await.map_err(PushError::NotSent)?;
        if !should_send(&advertisement) {
            return Ok(HttpPushOutcome::Declined);
        }
        if caps.report_v2 || caps.sideband {
            prepared.request = prepared
                .request_for(caps.report_v2, caps.sideband)
                .map_err(PushError::NotSent)?
                .into_owned();
        }
        let mut report = PushReport::pending(&prepared.commands);
        if prepared.commands.is_empty() {
            return Ok(HttpPushOutcome::Sent(report));
        }
        report.mark_attempted(&prepared.request, prepared.request.len());
        let body = RequestBody::new(prepared.request, prepared.pack);
        control.check().map_err(|e| PushError::NotSent(e.into()))?;
        let response = remote
            .exchange(
                "git-receive-pack",
                Some(body),
                prepared.limits.max_status_bytes,
                control,
            )
            .await;
        // Parse already received evidence even when cancellation is set. This work is bounded by
        // the status byte budget; the network failure still determines the uncertain
        // outcome.
        let retain = AtomicBool::new(false);
        let mut reader = response.body.as_slice();
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
        let result = response.result.map_err(PushFailure::from).and(parsed);
        match result {
            Ok(()) => Ok(HttpPushOutcome::Sent(report)),
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
    if let Ok(HttpPushOutcome::Sent(report)) = &result {
        crate::trace::push_report(&span, report);
    }

    result
}
