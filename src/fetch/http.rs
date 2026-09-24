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
        limits,
        control.cancel,
    )?;
    control.check()?;
    if !negotiation.needs_pack {
        return Ok(DownloadedFetch {
            advertisement,
            negotiation,
            known,
            limits,
            remaining,
            body: Vec::new(),
        });
    }
    let response = remote
        .exchange(
            "git-upload-pack",
            Some(RequestBody::new(request, Vec::new())),
            remaining,
            control,
        )
        .await;
    response.result?;
    Ok(DownloadedFetch {
        advertisement,
        negotiation,
        known,
        limits,
        remaining,
        body: response.body,
    })
}
