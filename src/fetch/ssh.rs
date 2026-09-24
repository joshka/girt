use std::ops::ControlFlow;
use std::sync::Arc;

use super::{Advertisement, FetchError, FetchLimits, KnownHistory, ReceivedFetch, protocol};
use crate::ObjectId;
use crate::packet::Wire;
use crate::transport::TransportControl;
use crate::transport::ssh::SshRemote;

/// Downloads a bounded protocol v0 response over system OpenSSH without changing local storage.
///
/// Uses the selection, negotiation and history contracts of [`super::receive_with_known`].
/// Requires a caller-owned Tokio runtime with I/O/time enabled. [`SshRemote`] defines executable,
/// authentication, trust, endpoint and process cleanup policy. A single SSH service process spans
/// advertisement, negotiation and response; empty/known-only fetches send a flush and await exit.
///
/// Returns unvalidated bytes: call [`SshFetch::validate`] synchronously on a caller-owned CPU
/// worker, then install explicitly. No hidden workers or filesystem redesign are involved.
/// Network retention adds at most `max_wire_bytes` to ordinary fetch budgets. Selection and
/// bounded advertisement/request parsing run synchronously; callbacks must return promptly.
///
/// Takes shared ownership of `known` for negotiation and later validation; clone the Arc first
/// if the caller also needs it. `None` requires no history preparation or allocation.
///
/// # Errors
///
/// Returns fetch validation/preflight errors or sanitized SSH failures. Interruption covers
/// handshake, blocked pipes, diagnostic drain and exit. No automatic retries or partial results.
///
/// # Panics
///
/// Panics when polled without a Tokio runtime with I/O and time enabled.
pub async fn receive_ssh(
    remote: &SshRemote,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: Option<Arc<KnownHistory>>,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<SshFetch, FetchError> {
    control.check()?;
    let empty = KnownHistory::default();
    let history = known.as_deref().unwrap_or(&empty);
    protocol::validate_known(history, limits, control.cancel)?;
    let mut session = remote.connect("git-upload-pack", control)?;
    let bytes = session
        .advertise(
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
    let remaining = limits.max_wire_bytes - bytes.len();
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
    let (body, result, _) = session.exchange(&request, &[], remaining, control).await;
    result?;
    if !negotiation.needs_pack && !body.is_empty() {
        return Err(FetchError::Protocol("trailing response bytes"));
    }
    Ok(SshFetch {
        advertisement,
        negotiation,
        known,
        limits,
        remaining,
        body,
    })
}

/// A bounded SSH response awaiting synchronous pack and connectivity validation.
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
pub struct SshFetch {
    advertisement: Advertisement,
    negotiation: protocol::Negotiation,
    known: Option<Arc<KnownHistory>>,
    limits: FetchLimits,
    remaining: usize,
    body: Vec<u8>,
}

impl std::fmt::Debug for SshFetch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshFetch")
            .field("wire_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl SshFetch {
    /// Validates packet framing, pack objects and selected-tip connectivity synchronously.
    ///
    /// Run on a caller-managed bounded CPU worker for large inputs. This work may process up to
    /// `max_decode_bytes`, `max_resolution_steps` and `max_connectivity_edges`; bounded input does
    /// not imply a short execution time. Cancellation is cooperative between packets/objects/graph
    /// steps, not during a single hash, inflate or parse. The SSH deadline has ended. Progress
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
