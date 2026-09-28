use std::num::NonZeroU32;
use std::sync::Arc;

use super::{Advertisement, DownloadedFetch, FetchError, FetchLimits, KnownHistory, protocol};
use crate::ObjectId;
use crate::packet::Wire;
use crate::transport::TransportControl;
use crate::transport::http::{HttpRemote, RequestBody};

/// Downloads objects through explicit smart-HTTP discovery and one upload-pack RPC.
///
/// Uses protocol v0 and the same selection, capability, bounded have batch, pack and connectivity
/// contracts as [`super::receive_with_known`]. Pass `None` for a full transfer.
/// Known-only/empty selections need discovery but send no RPC. Call [`DownloadedFetch::validate`]
/// on a synchronous worker before [`super::ReceivedFetch::install`]. Installation rechecks known
/// dependencies.
///
/// Requires a Tokio runtime. [`HttpRemote`] owns HTTP, authentication, TLS and interruption policy.
/// The response is buffered up to the remaining `max_wire_bytes` before synchronous protocol/pack
/// validation. Selection and request encoding are synchronous preflight work bounded by ref/want
/// counts; caller callbacks must return promptly. Pack validation is explicitly separate.
/// This adds at most `max_wire_bytes` of retained HTTP data to the ordinary fetch budgets. Encoded
/// requests use at most 96 bytes per bounded want plus 2048 bytes for haves/framing. Header/parser
/// buffers are bounded separately by the HTTP client; see [`HttpRemote`].
///
/// Takes shared ownership of `known` for negotiation and later validation; clone the Arc first
/// if the caller also needs it. `None` requires no history preparation or allocation.
///
/// # Errors
///
/// Returns the existing fetch validation errors or sanitized HTTP failures. Rejects wrong service
/// framing/media types, dumb HTTP, unsupported protocol versions, redirects and statuses other than
/// 200. A truncated HTTP body never produces installable objects. No automatic retry occurs.
pub async fn receive_http(
    remote: &HttpRemote,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: Option<Arc<KnownHistory>>,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<DownloadedFetch, FetchError> {
    receive_http_with_depth(remote, select, known, None, limits, control).await
}

/// Downloads a depth-limited upload-pack response. Validation reports resulting boundaries;
/// installation requires coordinated shallow metadata publication by the caller's workflow.
///
/// # Errors
///
/// Returns [`receive_http`]'s errors and rejects peers without shallow support.
pub async fn receive_http_with_depth(
    remote: &HttpRemote,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: Option<Arc<KnownHistory>>,
    depth: Option<NonZeroU32>,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<DownloadedFetch, FetchError> {
    receive_http_with_depth_and_progress(remote, select, known, depth, limits, control, |_| {})
        .await
}

/// Downloads an HTTP fetch while delivering live receiver sideband messages.
///
/// `progress` receives complete channel-2 payloads once, in wire order, after the negotiation
/// prefix is validated and before the response completes. Bytes remain untrusted and may not be
/// UTF-8. The callback runs synchronously on the async task and should return promptly.
/// Cancellation uses [`TransportControl`]. Empty and known-only selections produce no notices.
/// These are receiver messages; download byte counts and local object-resolution progress are
/// not reported.
///
/// Output is advisory: later HTTP, protocol or pack validation can still fail. No objects or refs
/// are installed by this call. [`DownloadedFetch::validate`] remains authoritative and replays
/// sideband messages through its own callback; pass a no-op callback there to avoid displaying
/// notices twice. Buffering and ownership follow [`receive_http`].
///
/// # Errors
///
/// Returns [`receive_http`]'s errors. Notices may have been delivered before a failure. Malformed
/// framing, invalid negotiation, terminal flush or channel 3 stop live notification; protocol and
/// pack errors are reported by final validation.
pub async fn receive_http_with_progress(
    remote: &HttpRemote,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: Option<Arc<KnownHistory>>,
    limits: FetchLimits,
    control: TransportControl<'_>,
    progress: impl FnMut(&[u8]) + Send,
) -> Result<DownloadedFetch, FetchError> {
    receive_http_with_depth_and_progress(remote, select, known, None, limits, control, progress)
        .await
}

/// Downloads a depth-limited HTTP fetch with [`receive_http_with_progress`]'s live notices.
///
/// The shallow response and acknowledgement prefix are checked before delivering sideband
/// messages. Final validation still determines the resulting shallow boundaries.
///
/// # Errors
///
/// Returns [`receive_http_with_depth`]'s errors, with the advisory notification and validation
/// replay contracts of [`receive_http_with_progress`].
pub async fn receive_http_with_depth_and_progress(
    remote: &HttpRemote,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: Option<Arc<KnownHistory>>,
    depth: Option<NonZeroU32>,
    limits: FetchLimits,
    control: TransportControl<'_>,
    mut progress: impl FnMut(&[u8]) + Send,
) -> Result<DownloadedFetch, FetchError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "fetch.http",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
        wire_bytes = tracing::field::Empty,
    );

    let operation = async {
        control.check()?;
        let empty = KnownHistory::default();
        let history = known.as_deref().unwrap_or(&empty);
        protocol::validate_known(history, limits, control.cancel)?;
        let (bytes, advertisement_bytes) = remote
            .discover(
                "git-upload-pack",
                limits.max_advertisement_bytes.min(limits.max_wire_bytes),
                control,
            )
            .await?;
        let mut reader = bytes.as_slice();
        let mut wire = Wire {
            reader: &mut reader,
            remaining: limits.max_wire_bytes,
            cancel: control.cancel,
        };
        let advertisement = protocol::advertise(&mut wire, limits)?;
        wire.end()?;
        // Include the service prelude in the aggregate HTTP payload budget.
        let remaining = limits.max_wire_bytes - advertisement_bytes;
        control.check()?;
        let mut request = Vec::new();
        let negotiation = protocol::request(
            &mut request,
            &advertisement,
            select(&advertisement),
            history,
            depth,
            limits,
            control.cancel,
        )?;
        control.check()?;
        if !negotiation.needs_pack {
            return Ok(DownloadedFetch {
                #[cfg(feature = "tracing")]
                trace: crate::trace::DownloadContext::capture(),
                advertisement,
                negotiation,
                known,
                limits,
                remaining,
                body: Vec::new(),
            });
        }
        let mut live = LiveProgress::new(depth.is_some());
        let mut validate_prefix = |bytes: &[u8]| {
            let mut reader = bytes;
            let mut wire = Wire {
                reader: &mut reader,
                remaining,
                cancel: control.cancel,
            };
            protocol::read_response_prefix(&mut wire, &negotiation, limits).is_ok()
                && wire.end().is_ok()
        };
        let response = remote
            .exchange_observed(
                "git-upload-pack",
                Some(RequestBody::new(request, Vec::new())),
                remaining,
                control,
                |bytes| live.observe(bytes, &mut validate_prefix, &mut progress),
            )
            .await;
        response.result?;
        Ok(DownloadedFetch {
            #[cfg(feature = "tracing")]
            trace: crate::trace::DownloadContext::capture(),
            advertisement,
            negotiation,
            known,
            limits,
            remaining,
            body: response.body,
        })
    };
    #[cfg(feature = "tracing")]
    let result = tracing::Instrument::instrument(operation, span.clone()).await;
    #[cfg(not(feature = "tracing"))]
    let result = operation.await;
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, crate::trace::fetch);
    #[cfg(feature = "tracing")]
    if let Ok(value) = &result {
        span.record("wire_bytes", value.body.len());
    }

    result
}

/// A cursor over retained response bytes. Prefix scanning finds a boundary; the existing parser
/// validates its semantics once before any notices are delivered.
struct LiveProgress {
    offset: usize,
    phase: Phase,
}

enum Phase {
    Shallow,
    Acknowledgements,
    Sideband,
    Stopped,
}

impl LiveProgress {
    fn new(depth: bool) -> Self {
        Self {
            offset: 0,
            phase: if depth {
                Phase::Shallow
            } else {
                Phase::Acknowledgements
            },
        }
    }

    fn observe(
        &mut self,
        bytes: &[u8],
        validate_prefix: &mut impl FnMut(&[u8]) -> bool,
        progress: &mut impl FnMut(&[u8]),
    ) {
        while !matches!(self.phase, Phase::Stopped) {
            let remaining = &bytes[self.offset..];
            let Some(header) = remaining.first_chunk::<4>() else {
                return;
            };
            let Ok(length) = crate::packet::packet_length(header) else {
                self.phase = Phase::Stopped;
                return;
            };
            if length == 0 {
                self.offset += 4;
                self.phase = if matches!(self.phase, Phase::Shallow) {
                    Phase::Acknowledgements
                } else {
                    Phase::Stopped
                };
                continue;
            }
            let Some(packet) = remaining.get(4..length) else {
                return;
            };
            self.offset += length;
            match self.phase {
                Phase::Shallow => {}
                Phase::Acknowledgements => {
                    let line = packet.strip_suffix(b"\n").unwrap_or(packet);
                    if line.starts_with(b"ACK ") && line.ends_with(b" continue") {
                        continue;
                    }
                    self.phase = if validate_prefix(&bytes[..self.offset]) {
                        Phase::Sideband
                    } else {
                        Phase::Stopped
                    };
                }
                Phase::Sideband => match packet.split_first() {
                    Some((1, _)) => {}
                    Some((2, payload)) => progress(payload),
                    _ => self.phase = Phase::Stopped,
                },
                Phase::Stopped => unreachable!(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::LiveProgress;

    #[rstest]
    #[case::first_header(2)]
    #[case::nak(6)]
    #[case::sideband_header(10)]
    #[case::sideband_payload(13)]
    #[case::complete(25)]
    fn live_notices_wait_for_complete_prefix_and_packets(#[case] split: usize) {
        let bytes = b"0008NAK\n0008\x02a\xff\r0006\x01x0006\x02b0000";
        let mut live = LiveProgress::new(false);
        let mut messages = Vec::new();
        let mut callback = |payload: &[u8]| messages.push(payload.to_vec());
        let mut validated = 0;
        let mut prefix = |bytes: &[u8]| {
            validated += 1;
            bytes == b"0008NAK\n"
        };
        live.observe(&bytes[..split], &mut prefix, &mut callback);
        live.observe(bytes, &mut prefix, &mut callback);
        live.observe(bytes, &mut prefix, &mut callback);
        assert_eq!(validated, 1);
        assert_eq!(messages, [b"a\xff\r".to_vec(), b"b".to_vec()]);
    }

    #[test]
    fn live_notices_wait_for_shallow_flush_and_final_ack() {
        let prefix = b"000dshallow x00000013ACK x continue\n000aACK x\n";
        let bytes = [prefix.as_slice(), b"0006\x02a0000"].concat();
        let mut live = LiveProgress::new(true);
        let mut messages = Vec::new();
        let mut validated = 0;
        live.observe(
            &bytes[..18],
            &mut |_| panic!("incomplete negotiation"),
            &mut |_| panic!("early progress"),
        );
        live.observe(
            &bytes,
            &mut |bytes| {
                validated += 1;
                bytes == prefix
            },
            &mut |bytes| messages.push(bytes.to_vec()),
        );
        assert_eq!(validated, 1);
        assert_eq!(messages, [b"a".to_vec()]);
    }

    #[rstest]
    #[case::invalid_header(b"zzzz")]
    #[case::reserved_header(b"0001")]
    #[case::flush(b"0000")]
    #[case::remote_error(b"0006\x03x")]
    #[case::invalid_channel(b"0006\x04x")]
    #[case::empty_packet(b"0004")]
    fn live_notices_stop_on_terminal_framing(#[case] terminal: &[u8]) {
        let bytes = [b"0008NAK\n0006\x02a".as_slice(), terminal, b"0006\x02b"].concat();
        let mut messages = Vec::new();
        LiveProgress::new(false).observe(&bytes, &mut |_| true, &mut |bytes| {
            messages.push(bytes.to_vec())
        });
        assert_eq!(messages, [b"a".to_vec()]);
    }

    #[test]
    fn live_notices_stop_when_authoritative_prefix_parser_rejects() {
        let mut messages = Vec::new();
        LiveProgress::new(false).observe(b"0008NAK\n0006\x02a0000", &mut |_| false, &mut |bytes| {
            messages.push(bytes.to_vec())
        });
        assert!(messages.is_empty());
    }
}
