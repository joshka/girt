use std::sync::Arc;

use super::{Advertisement, DownloadedFetch, FetchError, FetchLimits, KnownHistory, protocol};
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
/// Returns unvalidated bytes: call [`DownloadedFetch::validate`] synchronously on a caller-owned
/// CPU worker, then install explicitly. No hidden workers or filesystem redesign are involved.
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
) -> Result<DownloadedFetch, FetchError> {
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
    Ok(DownloadedFetch {
        advertisement,
        negotiation,
        known,
        limits,
        remaining,
        body,
    })
}
