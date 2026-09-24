//! Object-only fetch over upload-pack protocol v0.
//!
//! [`receive`] consumes a caller-owned pair of blocking streams; [`receive_local`] starts a local
//! Git upload-pack server. Both return a validated [`ReceivedFetch`] without touching a repository.
//! Install it explicitly, then use [`crate::refs`] for caller-selected conditional reference
//! updates. Fetch never writes references, reflogs, remote configuration, or `FETCH_HEAD`.
//!
//! Wants must be advertised IDs. Negotiation sends no haves and expects NAK, requesting a complete
//! pack on every transfer. Repeat/incremental fetches are correct but can retransmit history.
//! Only `side-band-64k` and, when advertised, `ofs-delta` are requested. Thin packs, shallow/filter
//! requests, automatic tags, refspecs, pruning, protocol v1/v2, HTTP, SSH, and credentials are
//! outside this slice. Peeling hints are exposed separately from selectable reference tips.

mod connectivity;
mod install;
mod local;
mod protocol;

use std::sync::atomic::{AtomicBool, Ordering};

pub use install::{FetchInstalled, ReceivedFetch};
pub use local::receive_local;
pub use protocol::{AdvertisedRef, Advertisement, receive};

/// Bounds for one advertisement, transfer, import, and connectivity check.
///
/// Retained buffers are O(`max_wire_bytes` + `max_decode_bytes` + `max_objects` +
/// `max_connectivity_edges`). Index generation needs at most 36 bytes per object plus 1072 bytes.
/// Graph parsing can temporarily copy a structured payload and its fields. These input/work bounds
/// exclude allocator overhead, stream-owned buffers, fixed zlib scratch space, and server memory;
/// they are not a hard process heap or wall-clock limit. Zero bounds allow only the corresponding
/// empty operation. Counters include duplicate input occurrences where applicable.
#[derive(Debug, Clone, Copy)]
pub struct FetchLimits {
    /// Total received pkt-line bytes, framing included (default 512 MiB).
    pub max_wire_bytes: usize,
    /// Advertisement bytes including framing (default 4 MiB).
    pub max_advertisement_bytes: usize,
    /// Advertised entries, including peeled hints (default 100,000).
    pub max_refs: usize,
    /// Want occurrences accepted from the selection callback (default 100,000).
    pub max_wants: usize,
    /// Received pack bytes, including header and trailer (default 256 MiB).
    pub max_pack_bytes: usize,
    /// Objects in the pack (default one million).
    pub max_objects: usize,
    /// Bytes in each decoded object (default 64 MiB).
    pub max_object_bytes: usize,
    /// Bytes in each inflated delta program (default 64 MiB).
    pub max_delta_bytes: usize,
    /// Aggregate inflated programs, ordinary payloads, and delta results (default 512 MiB).
    pub max_decode_bytes: usize,
    /// Delta edges from any object to an ordinary base (default 64).
    pub max_delta_depth: usize,
    /// Entry visits during iterative base resolution (default 10 million).
    pub max_resolution_steps: usize,
    /// Reachable edge occurrences, including duplicates (default 4 million).
    pub max_connectivity_edges: usize,
}

impl Default for FetchLimits {
    fn default() -> Self {
        Self {
            max_wire_bytes: 512 * 1024 * 1024,
            max_advertisement_bytes: 4 * 1024 * 1024,
            max_refs: 100_000,
            max_wants: 100_000,
            max_pack_bytes: 256 * 1024 * 1024,
            max_objects: 1_000_000,
            max_object_bytes: 64 * 1024 * 1024,
            max_delta_bytes: 64 * 1024 * 1024,
            max_decode_bytes: 512 * 1024 * 1024,
            max_delta_depth: 64,
            max_resolution_steps: 10_000_000,
            max_connectivity_edges: 4_000_000,
        }
    }
}

/// Transfer, validation, or installation failed. No references have been changed.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// Stream or filesystem failure; protocol I/O propagates interruption without retrying.
    #[error("fetch I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Invalid or unexpected protocol framing or state.
    #[error("invalid upload-pack response: {0}")]
    Protocol(&'static str),
    /// A protocol feature needed by the peer is outside this slice.
    #[error("unsupported upload-pack feature: {0}")]
    Unsupported(&'static str),
    /// Peer ERR or fatal sideband, retained as bytes without assuming UTF-8.
    #[error("upload-pack reported an error: {0:?}")]
    Remote(Vec<u8>),
    /// An explicit want was not a non-peeled advertised reference tip.
    #[error("unadvertised want {0}")]
    Unadvertised(ObjectId),
    /// A resource budget was exhausted.
    #[error("fetch limit exceeded: {0}")]
    Limit(&'static str),
    /// The cancellation flag was set or the progress callback requested cancellation.
    #[error("fetch cancelled")]
    Cancelled,
    /// Pack framing, checksum, delta reconstruction, or storage validation failed.
    #[error("received pack: {0}")]
    Pack(#[from] crate::ObjectReadError),
    /// Index encoding failed.
    #[error("received index: {0}")]
    Index(#[from] crate::PackWriteError),
    /// A selected tip or an object it references is absent from the received pack.
    #[error("missing reachable object {0}")]
    Missing(ObjectId),
    /// A reachable reference has the wrong logical object type.
    #[error("reachable object {0} has the wrong kind")]
    Kind(ObjectId),
    /// Reachable commit syntax is unsupported or invalid.
    #[error("reachable commit: {0}")]
    Commit(#[from] crate::CommitError),
    /// Reachable tree syntax or entries are invalid.
    #[error("reachable tree: {0}")]
    Tree(#[from] crate::TreeError),
    /// Reachable tag syntax is unsupported or invalid.
    #[error("reachable tag: {0}")]
    Tag(#[from] crate::TagError),
    /// A local upload-pack exited unsuccessfully after its protocol response.
    #[error("local upload-pack exited unsuccessfully: {0}")]
    Process(std::process::ExitStatus),
    /// A live object artifact with the same name contains different bytes.
    #[error("existing pack artifact differs: {0}")]
    Existing(std::path::PathBuf),
}

use crate::ObjectId;

pub(crate) fn check_cancelled(cancel: &AtomicBool) -> Result<(), FetchError> {
    if cancel.load(Ordering::Relaxed) {
        Err(FetchError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
