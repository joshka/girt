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
use crate::transport::ssh::SshRemote;

/// Reads an SSH upload-pack advertisement and, for a v2 server, lists every reference with one
/// `ls-refs` command on the same session. Returns the advertisement, its dialect and version, and
/// the response bytes consumed, all within `max_advertisement_bytes`.
pub(super) async fn open(
    session: &mut crate::transport::ssh::Session,
    requested: crate::transport::RequestedProtocol,
    limits: FetchLimits,
    control: TransportControl<'_>,
    diagnostics: &mut impl FnMut(&[u8]),
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
    let bytes = session
        .advertise_with_diagnostics(budget, control, diagnostics)
        .await?;
    let mut reader = bytes.as_slice();
    let mut wire = Wire {
        reader: &mut reader,
        remaining: budget,
        cancel: control.cancel,
    };
    if !bytes.starts_with(b"000eversion 2\n") {
        let (advertisement, version) = protocol::advertise_with_version(&mut wire, limits)?;
        wire.end()?;
        return Ok((advertisement, protocol::Dialect::V0, version, bytes.len()));
    }
    // Only a v2 request may be answered with v2; nothing is sent after an unsolicited answer.
    if requested != crate::transport::RequestedProtocol::V2 {
        return Err(FetchError::Unsupported("protocol version"));
    }
    wire.packet()?;
    let capabilities = Capabilities::read(&mut wire, limits)?;
    wire.end()?;
    let mut request = Vec::new();
    capabilities.write_ls_refs(&mut request, &[], limits, control.cancel)?;
    let left = budget.saturating_sub(bytes.len());
    let response = session
        .command_with_diagnostics(&request, left, control, diagnostics)
        .await?;
    let mut reader = response.as_slice();
    let mut wire = Wire {
        reader: &mut reader,
        remaining: left,
        cancel: control.cancel,
    };
    let advertisement = capabilities.read_ls_refs(&mut wire, limits)?;
    wire.end()?;
    advertisement.object_format()?;
    Ok((
        advertisement,
        protocol::Dialect::V2(capabilities),
        super::ProtocolVersion::V2,
        bytes.len() + response.len(),
    ))
}

/// Downloads a bounded upload-pack response over system OpenSSH without changing local storage.
///
/// Requests the remote's [`SshRemote::protocol`] (v0 unless changed); a v2 server adds one
/// `ls-refs` command to the same session before `fetch`.
///
/// Uses the selection, negotiation and history contracts of [`super::receive_with_known`].
/// Requires a caller-owned Tokio runtime with I/O/time enabled. [`SshRemote`] defines executable,
/// authentication, trust, endpoint and process cleanup policy. A single SSH service process spans
/// advertisement, negotiation and response; empty/known-only fetches send a flush and await exit.
///
/// Returns unvalidated bytes: call [`DownloadedFetch::validate`] synchronously on a caller-owned
/// CPU worker, then install explicitly. The response is spooled to an owned temporary file;
/// ordinary errors and drop remove it. Network retention adds at most `max_wire_bytes` of disk use
/// to ordinary fetch budgets. Selection and
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
    receive_ssh_with_depth(remote, select, known, None, limits, control).await
}

/// Downloads a depth-limited upload-pack response from one SSH session. Validation reports
/// resulting boundaries; installation requires coordinated shallow metadata publication.
///
/// # Errors
///
/// Returns [`receive_ssh`]'s errors and rejects peers without shallow support.
pub async fn receive_ssh_with_depth(
    remote: &SshRemote,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: Option<Arc<KnownHistory>>,
    depth: Option<NonZeroU32>,
    limits: FetchLimits,
    control: TransportControl<'_>,
) -> Result<DownloadedFetch, FetchError> {
    receive_ssh_with_progress(
        remote,
        select,
        known,
        FetchOptions { limits, depth },
        control,
        |_| {},
        |_| {},
    )
    .await
}

/// Downloads an SSH fetch with separate local diagnostics and live remote notices.
///
/// Uses [`receive_ssh`]'s owned download and process contracts, with `options.depth` selecting
/// optional shallow history. `diagnostics` receives raw local stderr in chunks of at most 8192
/// bytes; it follows [`super::discover_ssh_with_diagnostics`]'s redaction and display obligations.
/// `progress` receives complete remote channel-2 payloads once, in order, after validation of the
/// acknowledgement/shallow prefix. Notifications borrow the retained response and add no buffer.
/// Malformed or terminal sideband framing stops notification; final validation is authoritative.
///
/// Both callbacks must return promptly. They cannot select retry or publication outcomes. Local
/// diagnostics may contain secrets; remote notices are untrusted advisory bytes. The download's
/// validation replays remote notices: use a no-op validation callback after displaying them live.
/// Cancellation uses [`TransportControl`]; no partial fetch or automatic retry is returned.
///
/// # Errors
///
/// Returns [`receive_ssh_with_depth`]'s errors. Notifications may precede a later failure and never
/// establish valid objects or published references. Bytes outside the wire budget are not
/// delivered.
pub async fn receive_ssh_with_progress(
    remote: &SshRemote,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    mut known: Option<Arc<KnownHistory>>,
    options: FetchOptions,
    control: TransportControl<'_>,
    mut diagnostics: impl FnMut(&[u8]),
    mut progress: impl FnMut(&[u8]),
) -> Result<DownloadedFetch, FetchError> {
    let FetchOptions { limits, depth } = options;
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "fetch.ssh",
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
        let mut session = remote.connect("git-upload-pack", control)?;
        let (advertisement, dialect, _, used) =
            open(&mut session, remote.protocol(), limits, control, &mut diagnostics).await?;
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
        let remaining = limits.max_wire_bytes - used;
        control.check()?;
        let mut request = Vec::new();
        let negotiation = protocol::request(
            &mut request,
            &advertisement,
            &dialect,
            wants,
            history,
            depth,
            limits,
            control.cancel,
        )?;
        control.check()?;
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
        let (result, _) = session
            .exchange_streaming(
                &request,
                &[],
                remaining,
                control,
                &mut diagnostics,
                &mut |bytes| {
                    if let Err(error) = body.append(bytes) {
                        io_error = Some(error);
                        return Err(crate::transport::ssh::SshError::Io(
                            std::io::ErrorKind::Other,
                        ));
                    }
                    if negotiation.needs_pack {
                        live.feed(bytes, &mut validate_prefix, &mut progress);
                    }
                    Ok(())
                },
            )
            .await;
        if let Some(error) = io_error {
            return Err(error);
        }
        result?;
        if !negotiation.needs_pack && !body.is_empty() {
            return Err(FetchError::Protocol("trailing response bytes"));
        }
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
