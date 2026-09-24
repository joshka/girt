use std::ops::ControlFlow;

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
/// # Errors
///
/// Returns fetch validation/preflight errors or sanitized SSH failures. Interruption covers
/// handshake, blocked pipes, diagnostic drain and exit. No automatic retries or partial results.
///
/// # Panics
///
/// Panics when polled without a Tokio runtime with I/O and time enabled.
pub async fn receive_ssh<'a>(
    remote: &SshRemote,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: &'a KnownHistory,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<SshFetch<'a>, FetchError> {
    control.check()?;
    protocol::validate_known(known, limits, control.cancel)?;
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
        known,
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
/// Holds a borrow of immutable verified history, plus downloaded protocol bytes; it has no local
/// filesystem side effects. This separation lets the caller choose its CPU worker/concurrency
/// policy. It is `Send + Sync`, as is [`SshRemote`]; the history must outlive validation. Dropping
/// it discards the download. A successful download is not evidence of a valid pack.
pub struct SshFetch<'a> {
    advertisement: Advertisement,
    negotiation: protocol::Negotiation,
    known: &'a KnownHistory,
    limits: FetchLimits,
    remaining: usize,
    body: Vec<u8>,
}

impl std::fmt::Debug for SshFetch<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshFetch")
            .field("wire_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl SshFetch<'_> {
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
