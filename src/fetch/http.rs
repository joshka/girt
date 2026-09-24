use std::ops::ControlFlow;
use std::sync::Arc;

use super::{Advertisement, FetchError, FetchLimits, KnownHistory, ReceivedFetch, protocol};
use crate::ObjectId;
use crate::packet::Wire;
use crate::transport::TransportControl;
use crate::transport::http::{HttpRemote, RequestBody};

/// Downloads objects through explicit smart-HTTP discovery and one upload-pack RPC.
///
/// Uses protocol v0 and the same selection, capability, bounded have batch, pack and connectivity
/// contracts as [`super::receive_with_known`]. Pass `None` for a full transfer.
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
) -> Result<HttpFetch, FetchError> {
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
/// Owns downloaded protocol bytes and, when supplied, an [`Arc`] retaining the exact verified
/// history used for negotiation. History cannot be substituted during validation. It is
/// `Send + Sync + 'static` and can move into a caller-managed blocking worker after the initiating
/// scope ends. No history allocation is retained for a `None` input. Validation consumes this
/// result and releases its history ownership on success or failure; dropping it does the same
/// while discarding the bytes. Other Arc owners can keep history alive independently.
///
/// Downloading has no local filesystem side effects. A successful download is not evidence of a
/// valid pack. Callers bound queued downloads, retained bytes and active workers, and observe
/// worker completion even after requesting cancellation; dropping a worker handle does not stop its
/// work.
pub struct HttpFetch {
    advertisement: Advertisement,
    negotiation: protocol::Negotiation,
    known: Option<Arc<KnownHistory>>,
    limits: FetchLimits,
    remaining: usize,
    body: Vec<u8>,
}

impl std::fmt::Debug for HttpFetch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpFetch")
            .field("wire_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl HttpFetch {
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
        let empty = KnownHistory::default();
        let history = self.known.as_deref().unwrap_or(&empty);
        if !self.negotiation.needs_pack {
            return ReceivedFetch::without_pack(
                self.advertisement,
                self.negotiation.wants,
                history,
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
            history,
            self.limits,
            cancel,
            progress,
        )
    }
}
