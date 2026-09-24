use std::ops::ControlFlow;

use super::{Advertisement, FetchError, FetchLimits, KnownHistory, ReceivedFetch, protocol};
use crate::ObjectId;
use crate::packet::Wire;
use crate::transport::TransportControl;
use crate::transport::http::{HttpRemote, RequestBody};

/// Downloads objects through explicit smart-HTTP discovery and one upload-pack RPC.
///
/// Uses protocol v0 and the same selection, capability, bounded have batch, pack and connectivity
/// contracts as [`super::receive_with_known`]. Pass [`KnownHistory::default`] for a full transfer.
/// Known-only/empty selections need discovery but send no RPC. Call [`HttpFetch::validate`] on a
/// synchronous worker before [`ReceivedFetch::install`]. Installation rechecks known dependencies.
///
/// Requires a Tokio runtime. [`HttpRemote`] owns HTTP, authentication, TLS and interruption policy.
/// The response is buffered up to the remaining `max_wire_bytes` before synchronous protocol/pack
/// validation. Selection and request encoding are synchronous preflight work bounded by ref/want
/// counts; caller callbacks must return promptly. Pack validation is explicitly separate.
/// This adds at most `max_wire_bytes` of retained HTTP data to the ordinary fetch budgets. Encoded
/// requests use at most 96 bytes per bounded want plus 2048 bytes for haves/framing. Header/parser
/// buffers are bounded separately by the HTTP client; see [`HttpRemote`].
///
/// # Errors
///
/// Returns the existing fetch validation errors or sanitized HTTP failures. Rejects wrong service
/// framing/media types, dumb HTTP, unsupported protocol versions, redirects and statuses other than
/// 200. A truncated HTTP body never produces installable objects. No automatic retry occurs.
pub async fn receive_http<'a>(
    remote: &HttpRemote,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: &'a KnownHistory,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<HttpFetch<'a>, FetchError> {
    control.check()?;
    protocol::validate_known(known, limits, control.cancel)?;
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
        known,
        limits,
        control.cancel,
    )?;
    control.check()?;
    if !negotiation.needs_pack {
        return Ok(HttpFetch {
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
    Ok(HttpFetch {
        advertisement,
        negotiation,
        known,
        limits,
        remaining,
        body: response.body,
    })
}

/// A bounded HTTP response awaiting synchronous pack and connectivity validation.
///
/// Holds a borrow of immutable verified history, plus downloaded protocol bytes; it has no local
/// filesystem side effects. This separation lets the caller choose its CPU worker/concurrency
/// policy. It is `Send + Sync`, as is [`HttpRemote`]; the history must outlive validation. Dropping
/// it discards the download. A successful download is not evidence of a valid pack.
pub struct HttpFetch<'a> {
    advertisement: Advertisement,
    negotiation: protocol::Negotiation,
    known: &'a KnownHistory,
    limits: FetchLimits,
    remaining: usize,
    body: Vec<u8>,
}

impl std::fmt::Debug for HttpFetch<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpFetch")
            .field("wire_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl HttpFetch<'_> {
    /// Validates packet framing, pack objects and selected-tip connectivity synchronously.
    ///
    /// Run on a caller-managed bounded CPU worker for large inputs. This work may process up to
    /// `max_decode_bytes`, `max_resolution_steps` and `max_connectivity_edges`; bounded input does
    /// not imply a short execution time. Cancellation is cooperative between packets/objects/graph
    /// steps, not during a single hash, inflate or parse. The HTTP deadline has ended. Progress
    /// callbacks run here, after network completion. Installation is a separate synchronous call.
    ///
    /// # Errors
    ///
    /// Returns [`super::receive_with_known`]'s validation failures. No repository is changed.
    pub fn validate(
        self,
        cancel: &std::sync::atomic::AtomicBool,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> Result<ReceivedFetch, FetchError> {
        if !self.negotiation.needs_pack {
            return ReceivedFetch::without_pack(
                self.advertisement,
                self.negotiation.wants,
                self.known,
                self.limits,
                cancel,
            );
        }
        let mut reader = self.body.as_slice();
        let mut wire = Wire {
            reader: &mut reader,
            remaining: self.remaining,
            cancel,
        };
        protocol::response(
            &mut wire,
            self.advertisement,
            self.negotiation,
            self.known,
            self.limits,
            cancel,
            progress,
        )
    }
}
