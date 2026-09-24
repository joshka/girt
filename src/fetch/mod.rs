//! Object-only fetch over upload-pack protocol v0.
//!
//! [`receive`] consumes a caller-owned pair of blocking streams; [`receive_local`] starts a local
//! Git upload-pack server. Both return a validated [`ReceivedFetch`] without touching a repository.
//! Install it explicitly, then use [`crate::refs`] for caller-selected conditional reference
//! updates. Fetch never writes references, reflogs, remote configuration, or `FETCH_HEAD`.
//!
//! Wants must be advertised IDs. [`receive`] requests full histories; [`receive_with_known`] uses
//! bounded [`KnownHistory`] to negotiate incremental transfers. Received delta bases stay internal,
//! while selected-tip connectivity can depend on verified local objects. Installation rechecks
//! those dependencies before publication. `side-band-64k`, optional `ofs-delta` and optional
//! `multi_ack` are the only requested capabilities. Thin packs, shallow/filter
//! requests, automatic tags, refspecs, pruning and protocol v1/v2 are outside this slice.
//! The `http` and `ssh` features add async network downloads with separate synchronous validation.
//! SSH requires macOS/Linux and a caller-selected OpenSSH configuration. Peeling
//! hints are exposed separately from selectable reference tips.

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
mod ssh;
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub use ssh::{SshFetch, receive_ssh};

#[cfg(feature = "http")]
mod http;
#[cfg(feature = "http")]
pub use http::{HttpFetch, receive_http};

mod connectivity;
mod install;
mod known;
mod local;
mod protocol;

use std::sync::atomic::{AtomicBool, Ordering};

pub use install::{FetchInstalled, ReceivedFetch};
pub use known::KnownHistory;
pub use local::{receive_local, receive_local_with_control, receive_local_with_known};
pub use protocol::{AdvertisedRef, Advertisement, receive, receive_with_known};

/// Bounds for one advertisement, transfer, import, and connectivity check.
///
/// Retained buffers are O(`max_wire_bytes` + `max_decode_bytes` + `max_objects` +
/// `max_connectivity_edges`), plus separately retained [`KnownHistory`] payloads bounded by
/// `max_known_bytes` and metadata bounded by `max_known_objects`. Index generation needs at most 36
/// bytes per object plus 1072 bytes. Graph parsing can temporarily copy a structured payload and
/// its fields. These input/work bounds exclude allocator overhead, stream-owned buffers, fixed zlib
/// scratch space, and server memory; they are not a hard process heap or wall-clock limit. Zero
/// bounds allow only the corresponding empty operation. Counters include duplicate input
/// occurrences where applicable.
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
    /// Maximum have candidates retained (default 256). The single wire batch sends at most 32
    /// with multi-ACK, or one without it.
    /// Additional verified commits are retained for connectivity but not offered as haves.
    pub max_haves: usize,
    /// Objects retained while verifying local history (default one million).
    pub max_known_objects: usize,
    /// Aggregate local payload bytes retained (default 256 MiB).
    pub max_known_bytes: usize,
    /// Local history edge occurrences (default 4 million).
    pub max_known_edges: usize,
    /// Per-read decoding limits for local verification and installation rechecks.
    /// Total decoding work is bounded by this budget times `max_known_objects`.
    pub known_read: crate::ReadLimits,
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
            max_haves: 256,
            max_known_objects: 1_000_000,
            max_known_bytes: 256 * 1024 * 1024,
            max_known_edges: 4_000_000,
            known_read: crate::ReadLimits::default(),
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
    /// Sanitized OpenSSH transport or service failure.
    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    #[error("{0}")]
    Ssh(#[source] crate::transport::ssh::SshError),
    /// Sanitized smart HTTP exchange failure.
    #[cfg(feature = "http")]
    #[error("{0}")]
    Http(#[source] crate::transport::http::HttpError),
    /// Stream or filesystem failure; protocol I/O propagates interruption without retrying.
    #[error("fetch I/O: {0}")]
    Io(#[source] std::io::Error),
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
    /// The owned transport reached its caller-supplied deadline.
    #[error("transport deadline expired")]
    Deadline,
    /// Pack framing, checksum, delta reconstruction, or storage validation failed.
    #[error("received pack: {0}")]
    Pack(#[from] crate::ObjectReadError),
    /// Index encoding failed.
    #[error("received index: {0}")]
    Index(#[from] crate::PackWriteError),
    /// A selected tip or reachable object is absent from both received and verified local objects.
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

impl From<crate::packet::Error> for FetchError {
    fn from(error: crate::packet::Error) -> Self {
        match error {
            crate::packet::Error::Io(e) => Self::from(e),
            crate::packet::Error::Protocol(e) => Self::Protocol(e),
            crate::packet::Error::Limit(e) => Self::Limit(e),
            crate::packet::Error::Cancelled => Self::Cancelled,
            crate::packet::Error::Remote(e) => Self::Remote(e),
        }
    }
}

impl From<std::io::Error> for FetchError {
    fn from(error: std::io::Error) -> Self {
        match crate::transport::interruption(&error) {
            Some(crate::transport::Interruption::Cancelled) => Self::Cancelled,
            Some(crate::transport::Interruption::Deadline) => Self::Deadline,
            None => Self::Io(error),
        }
    }
}

#[cfg(feature = "http")]
impl From<crate::transport::http::HttpError> for FetchError {
    fn from(error: crate::transport::http::HttpError) -> Self {
        match error {
            crate::transport::http::HttpError::Cancelled => Self::Cancelled,
            crate::transport::http::HttpError::Deadline => Self::Deadline,
            error => Self::Http(error),
        }
    }
}

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
impl From<crate::transport::ssh::SshError> for FetchError {
    fn from(error: crate::transport::ssh::SshError) -> Self {
        match error {
            crate::transport::ssh::SshError::Cancelled => Self::Cancelled,
            crate::transport::ssh::SshError::Deadline => Self::Deadline,
            error => Self::Ssh(error),
        }
    }
}
