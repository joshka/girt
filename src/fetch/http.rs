use std::num::NonZeroU32;
use std::sync::Arc;

use super::live_progress::LiveProgress;
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
