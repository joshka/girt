//! Fetch over an application transport's connection.
//!
//! The same exchange as SSH: read the advertisement (asking `ls-refs` first under v2), send one
//! request, close the connection's input, and spool the response until the service ends it.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::live_progress::LiveProgress;
use super::v2::Capabilities;
use super::{Advertisement, DownloadedFetch, FetchError, FetchOptions, KnownHistory, protocol};
use crate::ObjectId;
use crate::packet::Wire;
use crate::transport::{Connection, RequestedProtocol};

// Counts the bytes read, so the response budget excludes the advertisement.
struct Counted<R> {
    inner: R,
    read: usize,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.read += read;
        Ok(read)
    }
}

/// Downloads an upload-pack response over `connection` without changing local storage.
#[allow(clippy::too_many_arguments)]
pub(crate) fn download(
    connection: &mut dyn Connection,
    requested: RequestedProtocol,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    mut known: Option<Arc<KnownHistory>>,
    options: FetchOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&[u8]),
) -> Result<DownloadedFetch, FetchError> {
    let limits = options.limits;
    let depth = options.depth;
    let empty = KnownHistory::default();
    protocol::validate_known(known.as_deref().unwrap_or(&empty), limits, cancel)?;
    let budget = limits.max_advertisement_bytes.min(limits.max_wire_bytes);
    let (advertisement, dialect, used) = open(connection, requested, limits, budget, cancel)?;
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
    let mut request = Vec::new();
    let negotiation = protocol::request(
        &mut request,
        &advertisement,
        &dialect,
        wants,
        history,
        &options,
        cancel,
    )?;
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(FetchError::Cancelled);
    }
    connection.write_all(&request).map_err(FetchError::Io)?;
    connection.flush().map_err(FetchError::Io)?;
    connection.close_write().map_err(FetchError::Io)?;

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
            cancel,
        };
        protocol::read_response_prefix(&mut wire, &negotiation, limits).is_ok()
            && wire.end().is_ok()
    };
    let mut body = super::bytes::Bytes::temporary()?;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(FetchError::Cancelled);
        }
        let read = match connection.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(FetchError::Io(error)),
        };
        if body.len().saturating_add(read) > remaining {
            return Err(FetchError::Limit("wire bytes"));
        }
        let bytes = &buffer[..read];
        body.append(bytes)?;
        if negotiation.needs_pack {
            live.feed(bytes, &mut validate_prefix, &mut progress);
        }
    }
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
}

// Reads the advertisement; under v2, asks `ls-refs` for it first.
fn open(
    connection: &mut dyn Connection,
    requested: RequestedProtocol,
    limits: super::FetchLimits,
    budget: usize,
    cancel: &AtomicBool,
) -> Result<(Advertisement, protocol::Dialect, usize), FetchError> {
    let mut reader = Counted {
        inner: &mut *connection,
        read: 0,
    };
    let mut wire = Wire {
        reader: &mut reader,
        remaining: budget,
        cancel,
    };
    let first = wire
        .packet()?
        .ok_or(FetchError::Protocol("empty protocol advertisement"))?;
    if first != b"version 2\n" {
        let mut prefix = Vec::new();
        crate::packet::packet(&mut prefix, &first, cancel)?;
        // The replayed packet was already charged; give it back, but no more.
        let remaining = wire.remaining + prefix.len();
        let mut stream = std::io::Cursor::new(prefix).chain(wire.reader);
        let mut legacy = Wire {
            reader: &mut stream,
            remaining,
            cancel,
        };
        let (advertisement, _) = protocol::advertise_with_version(&mut legacy, limits)?;
        let used = reader.read;
        return Ok((advertisement, protocol::Dialect::V0, used));
    }
    // Only a v2 request may be answered with v2; nothing is sent after an unsolicited answer.
    if requested != RequestedProtocol::V2 {
        return Err(FetchError::Unsupported("protocol version"));
    }
    let capabilities = Capabilities::read(&mut wire, limits)?;
    let mut request = Vec::new();
    capabilities.write_ls_refs(&mut request, &[], limits, cancel)?;
    let used = reader.read;
    connection.write_all(&request).map_err(FetchError::Io)?;
    connection.flush().map_err(FetchError::Io)?;
    let mut reader = Counted {
        inner: &mut *connection,
        read: 0,
    };
    let mut wire = Wire {
        reader: &mut reader,
        remaining: budget.saturating_sub(used),
        cancel,
    };
    let advertisement = capabilities.read_ls_refs(&mut wire, limits)?;
    advertisement.object_format()?;
    Ok((
        advertisement,
        protocol::Dialect::V2(capabilities),
        used + reader.read,
    ))
}
