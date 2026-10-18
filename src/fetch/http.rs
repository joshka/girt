use std::num::NonZeroU32;
use std::sync::Arc;

use super::live_progress::LiveProgress;
use super::v2::Capabilities;
use super::{
    Advertisement, DownloadedFetch, FetchError, FetchLimits, FetchOptions, KnownHistory, protocol,
};
use crate::ObjectId;
use crate::packet::Wire;
use crate::transport::TransportControl;
use crate::transport::http::{HttpRemote, RequestBody};

/// Reads an HTTP upload-pack advertisement and, for a v2 server, lists every reference with one
/// `ls-refs` request. Returns the advertisement, its dialect and version, and the response bytes
/// consumed, all
/// within `max_advertisement_bytes`.
pub(super) async fn open(
    remote: &HttpRemote,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<
    (
        Advertisement,
        protocol::Dialect,
        super::ProtocolVersion,
        usize,
    ),
    FetchError,
> {
    let budget = limits.max_advertisement_bytes.min(limits.max_wire_bytes);
    let (bytes, advertisement_bytes) = remote.discover("git-upload-pack", budget, control).await?;
    let mut reader = bytes.as_slice();
    let mut wire = Wire {
        reader: &mut reader,
        remaining: budget,
        cancel: control.cancel,
    };
    if !bytes.starts_with(b"000eversion 2\n") {
        let (advertisement, version) = protocol::advertise_with_version(&mut wire, limits)?;
        wire.end()?;
        return Ok((
            advertisement,
            protocol::Dialect::V0,
            version,
            advertisement_bytes,
        ));
    }
    // Only a v2 request may be answered with v2; nothing is sent after an unsolicited answer.
    if remote.protocol() != crate::transport::RequestedProtocol::V2 {
        return Err(FetchError::Unsupported("protocol version"));
    }
    wire.packet()?;
    let capabilities = Capabilities::read(&mut wire, limits)?;
    wire.end()?;
    let mut request = Vec::new();
    capabilities.write_ls_refs(&mut request, &[], limits, control.cancel)?;
    control.check()?;
    let response = remote
        .exchange(
            "git-upload-pack",
            Some(RequestBody::new(request, Vec::new())?),
            budget.saturating_sub(advertisement_bytes),
            control,
        )
        .await;
    response.result?;
    let mut reader = response.body.as_slice();
    let mut wire = Wire {
        reader: &mut reader,
        remaining: budget.saturating_sub(advertisement_bytes),
        cancel: control.cancel,
    };
    let advertisement = capabilities.read_ls_refs(&mut wire, limits)?;
    wire.end()?;
    advertisement.object_format()?;
    Ok((
        advertisement,
        protocol::Dialect::V2(capabilities),
        super::ProtocolVersion::V2,
        advertisement_bytes + response.body.len(),
    ))
}

/// Downloads objects through explicit smart-HTTP discovery and one upload-pack RPC.
///
/// Requests the endpoint's [`HttpRemote::protocol`] (v0 unless changed). A v2 server adds one
/// `ls-refs` request before the fetch RPC; v0/v1 responses are accepted either way. Uses the same
/// selection, capability, bounded have batch, pack and connectivity contracts as
/// [`super::receive_with_known`]. Pass `None` for a full transfer.
/// Known-only/empty selections need discovery but send no RPC. Call [`DownloadedFetch::validate`]
/// on a synchronous worker before [`super::ReceivedFetch::install`]. Installation rechecks known
/// dependencies.
///
/// Requires a Tokio runtime. [`HttpRemote`] owns HTTP, authentication, TLS and interruption policy.
/// The response is spooled to a temporary file up to the remaining `max_wire_bytes` before
/// synchronous protocol/pack validation. Selection and request encoding are synchronous preflight
/// work bounded by ref/want counts; caller callbacks must return promptly. Pack validation is
/// explicitly separate. This adds at most `max_wire_bytes` of temporary disk use to the ordinary
/// fetch budgets. Encoded requests use at most 96 bytes per bounded want plus 2048 bytes for
/// haves/framing. Header/parser buffers are bounded separately by the HTTP client; see
/// [`HttpRemote`].
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
    progress: impl FnMut(&[u8]),
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
    progress: impl FnMut(&[u8]),
) -> Result<DownloadedFetch, FetchError> {
    receive_http_with_options(
        remote,
        select,
        known,
        FetchOptions::default()
            .with_limits(limits)
            .with_depth(depth),
        control,
        progress,
    )
    .await
}

/// Downloads an HTTP fetch shaped by [`FetchOptions`], with [`receive_http_with_progress`]'s live
/// notices.
///
/// `options.depth` requests shallow history as [`receive_http_with_depth`] does.
/// `options.filter` requests a partial pack when the server advertises filtering; otherwise the
/// pack is complete. The validated result's [`ReceivedFetch::filter_applied`] reports which.
///
/// # Errors
///
/// Returns [`receive_http_with_depth_and_progress`]'s errors.
///
/// [`ReceivedFetch::filter_applied`]: super::ReceivedFetch::filter_applied
pub async fn receive_http_with_options(
    remote: &HttpRemote,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    mut known: Option<Arc<KnownHistory>>,
    options: FetchOptions,
    control: TransportControl<'_>,
    mut progress: impl FnMut(&[u8]),
) -> Result<DownloadedFetch, FetchError> {
    let limits = options.limits;
    let depth = options.depth;
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
        let (advertisement, dialect, _, advertisement_bytes) =
            open(remote, limits, control).await?;
        let wants = select(&advertisement);
        if depth.is_none()
            && known
                .as_ref()
                .is_some_and(|history| !history.applies_to(&wants))
        {
            known = known
                .as_ref()
                .map(|history| Arc::new(history.shallow_only()));
        }
        let history = known.as_deref().unwrap_or(&empty);
        // Include the service prelude in the aggregate HTTP payload budget.
        let remaining = limits.max_wire_bytes - advertisement_bytes;
        control.check()?;
        let mut request = Vec::new();
        let negotiation = protocol::request(
            &mut request,
            &advertisement,
            &dialect,
            wants,
            history,
            &options,
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
                body: Vec::new().into(),
            });
        }
        let mut live = if negotiation.is_v2() {
            LiveProgress::new_v2(limits.max_shallow_roots)
        } else {
            LiveProgress::new(depth.is_some(), limits.max_shallow_roots)
        };
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
        let mut body = super::bytes::Bytes::temporary()?;
        let mut io_error = None;
        let result = remote
            .transfer(
                "git-upload-pack",
                Some(RequestBody::new(request, Vec::new())?),
                control,
                |chunk| {
                    if chunk.len() > remaining.saturating_sub(body.len()) {
                        return Err(crate::transport::http::HttpError::Limit("response body"));
                    }
                    if let Err(error) = body.append(chunk) {
                        io_error = Some(error);
                        return Err(crate::transport::http::HttpError::Network);
                    }
                    live.feed(chunk, &mut validate_prefix, &mut progress);
                    Ok(())
                },
            )
            .await;
        if let Some(error) = io_error {
            return Err(error);
        }
        result?;
        Ok(DownloadedFetch {
            #[cfg(feature = "tracing")]
            trace: crate::trace::DownloadContext::capture(),
            advertisement,
            negotiation,
            known,
            limits,
            remaining,
            body,
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
