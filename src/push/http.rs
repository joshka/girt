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
    prepared: PreparedPush,
    control: TransportControl<'_>,
    should_send: impl FnOnce(&PushAdvertisement) -> bool,
) -> Result<HttpPushOutcome, PushError> {
    send_http_checked_with_progress(remote, prepared, control, should_send, |_| {}).await
}

/// Sends a checked HTTP push while delivering receiver progress before the response completes.
///
/// Call [`PreparedPush::with_progress`] before sending to request sideband progress. When the
/// receiver supports it, `progress` receives each complete channel-2 payload once, in wire order,
/// as borrowed bytes without UTF-8 conversion. No callback occurs when sideband is not negotiated.
/// These messages describe receiver work. [`super::PreparationProgress`] reports preparation
/// separately; upload has no progress callback.
///
/// The callback runs synchronously on the caller's async task and should return promptly. Its
/// messages are untrusted, advisory output: they do not establish acceptance or replace inspection
/// of the final report. Complete progress packets remain in [`PushReport::progress`], including
/// reports returned with uncertain failures. Cancellation still uses [`TransportControl`].
/// Malformed framing or a terminal sideband packet stops notification; the final parser determines
/// the structured result. Bytes beyond the response budget never reach the callback.
///
/// # Errors
///
/// Uses [`send_http_checked`]'s preflight, decline and uncertain-outcome contracts. A callback may
/// run before a later transport or protocol failure; inspect remote refs before retrying an
/// uncertain push.
pub async fn send_http_checked_with_progress(
    remote: &HttpRemote,
    mut prepared: PreparedPush,
    control: TransportControl<'_>,
    should_send: impl FnOnce(&PushAdvertisement) -> bool,
    mut progress: impl FnMut(&[u8]) + Send,
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
        let mut sideband = LiveProgress::default();
        let response = remote
            .exchange_observed(
                "git-receive-pack",
                Some(body),
                prepared.limits.max_status_bytes,
                control,
                |bytes| {
                    if caps.sideband {
                        sideband.observe(bytes, &mut progress);
                    }
                },
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

/// Walks the retained response without copying it or changing authoritative parser results.
#[derive(Default)]
struct LiveProgress {
    offset: usize,
    stopped: bool,
}

impl LiveProgress {
    fn observe(&mut self, bytes: &[u8], progress: &mut impl FnMut(&[u8])) {
        while !self.stopped {
            let remaining = &bytes[self.offset..];
            let Some(header) = remaining.first_chunk::<4>() else {
                return;
            };
            let Ok(length) = crate::packet::packet_length(header) else {
                self.stopped = true;
                return;
            };
            if length == 0 {
                self.stopped = true;
                return;
            }
            let Some(packet) = remaining.get(4..length) else {
                return;
            };
            self.offset += length;
            match packet.split_first() {
                Some((1, _)) => {}
                Some((2, payload)) => progress(payload),
                _ => self.stopped = true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::LiveProgress;

    #[rstest]
    #[case::before_header(0)]
    #[case::header_byte(1)]
    #[case::half_header(2)]
    #[case::almost_header(3)]
    #[case::header(4)]
    #[case::channel(5)]
    #[case::payload(6)]
    #[case::first_packet(8)]
    #[case::second_header(10)]
    #[case::all(16)]
    fn progress_preserves_bytes_across_response_chunks(#[case] split: usize) {
        let bytes = b"0008\x02a\xff\n0008\x02b\x00\r0000";
        let mut observer = LiveProgress::default();
        let mut received = Vec::new();
        let mut callback = |payload: &[u8]| received.push(payload.to_vec());
        observer.observe(&bytes[..split], &mut callback);
        observer.observe(bytes, &mut callback);
        observer.observe(bytes, &mut callback);
        assert_eq!(received, [b"a\xff\n".to_vec(), b"b\x00\r".to_vec()]);
    }

    #[rstest]
    #[case::flush(b"0000")]
    #[case::remote_error(b"0006\x03x")]
    #[case::unknown_channel(b"0006\x04x")]
    #[case::missing_channel(b"0004")]
    #[case::invalid_header(b"zzzz")]
    #[case::reserved_length(b"0001")]
    #[case::oversized(b"ffff")]
    #[case::git_error(b"0009ERR x")]
    fn progress_stops_at_terminal_or_invalid_packets(#[case] terminal: &[u8]) {
        let bytes = [b"0006\x02a".as_slice(), terminal, b"0006\x02b"].concat();
        let mut received = Vec::new();
        LiveProgress::default().observe(&bytes, &mut |payload| received.push(payload.to_vec()));
        assert_eq!(received, [b"a".to_vec()]);
    }

    #[test]
    fn progress_skips_status_and_incomplete_payloads() {
        let bytes = b"0006\x01x0006\x02a0009\x02b";
        let mut received = Vec::new();
        LiveProgress::default().observe(bytes, &mut |payload| received.push(payload.to_vec()));
        assert_eq!(received, [b"a".to_vec()]);
    }
}
