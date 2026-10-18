//! Serve fetches to Git clients, as `git upload-pack` does.
//!
//! [`UploadPack`] answers protocol v2 requests over any byte stream. A program that runs it over
//! stdin and stdout works as the `--upload-pack` command of `git clone` and `git fetch`, over SSH
//! and over `file://` URLs. Pack generation reuses the push object walk: it counts the objects to
//! send before the first pack byte, then streams the pack through sideband 1 as it is written.
//!
//! # Examples
//!
//! Advertise capabilities and list references for a client that sent `ls-refs`:
//!
//! ```
//! use std::sync::atomic::AtomicBool;
//!
//! use girt::serve::{Protocol, UploadPack};
//! use girt::{InitKind, ObjectFormat, Repository};
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("repo.git"),
//!     InitKind::Bare,
//! )?;
//! let upload = UploadPack::new(&repo)?;
//!
//! let request = b"0014command=ls-refs\n0001000bsymrefs000aunborn0000";
//! let mut response = Vec::new();
//! upload.serve(
//!     Protocol::V2,
//!     &mut &request[..],
//!     &mut response,
//!     &AtomicBool::new(false),
//! )?;
//! assert!(response.starts_with(b"000eversion 2\n"));
//! assert!(response.ends_with(b"002eunborn HEAD symref-target:refs/heads/main\n0000"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Status
//!
//! Protocol v2 `ls-refs` and `fetch` are served, with `include-tag`, thin packs, `wait-for-done`
//! and shallow requests (`deepen`, `deepen-relative`, `deepen-since` and `deepen-not`).
//! Negotiation sends the pack as soon as one common object is found. `filter`, `object-info` and
//! protocol v0 aren't supported yet, and a shallow repository can't be served; see
//! [#520](https://github.com/joshka/girt/issues/520).

#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted network input (#103)"
)]

mod shallow;
mod upload;

#[cfg(test)]
mod tests;

pub use upload::{UploadLimits, UploadPack, WantPolicy};

use crate::ObjectId;

/// The wire protocol version a client asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Protocol {
    /// The original protocol, used when the client requests no version.
    V0,
    /// Protocol v2, with separate commands such as `ls-refs` and `fetch`.
    V2,
}

impl Protocol {
    /// Reads the version a client requested in the `GIT_PROTOCOL` environment variable or the
    /// `Git-Protocol` HTTP header.
    ///
    /// The value holds colon-separated `key=value` parameters. The highest `version` that girt
    /// serves wins. Unknown keys and versions are ignored, so an absent or empty value is
    /// [`Protocol::V0`].
    ///
    /// ```
    /// use girt::serve::Protocol;
    ///
    /// assert_eq!(Protocol::from_parameters(b"version=2"), Protocol::V2);
    /// assert_eq!(
    ///     Protocol::from_parameters(b"object-format=sha256:version=2"),
    ///     Protocol::V2
    /// );
    /// assert_eq!(Protocol::from_parameters(b""), Protocol::V0);
    /// ```
    pub fn from_parameters(value: &[u8]) -> Self {
        let v2 = value
            .split(|&byte| byte == b':')
            .any(|parameter| parameter == b"version=2");
        if v2 { Self::V2 } else { Self::V0 }
    }
}

/// Why a serving session stopped.
///
/// Requests the client may correct are reported to it in an `ERR` packet before the session
/// stops, as Git does.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServeError {
    /// Reading the request or writing the response failed.
    #[error("serving I/O")]
    Io(#[from] std::io::Error),
    /// The client sent a malformed or out-of-order request.
    #[error("invalid client request: {0}")]
    Protocol(&'static str),
    /// The client asked for a feature girt doesn't serve.
    #[error("unsupported client request: {0}")]
    Unsupported(&'static str),
    /// The client wanted an object that the served references don't name.
    #[error("not our ref {0}")]
    NotOurRef(ObjectId),
    /// A request exceeded a configured bound.
    #[error("serving limit exceeded: {0}")]
    Limit(&'static str),
    /// The caller's cancellation flag was set.
    #[error("serving cancelled")]
    Cancelled,
    /// Repository configuration can't be served as written.
    #[error("unsupported serving configuration: {0}")]
    Config(&'static str),
    /// Reading the served references failed.
    #[error("reading references")]
    References(#[source] Box<crate::refs::ReferenceError>),
    /// Opening or reading the object store failed.
    #[error("reading objects")]
    Objects(#[source] Box<crate::ObjectReadError>),
    /// Walking history for [`WantPolicy::Reachable`] failed.
    #[error("walking history")]
    History(#[source] Box<crate::HistoryError>),
    /// Peeling an advertised tag failed.
    #[error("peeling an advertised tag")]
    Peel(#[source] Box<crate::PeelError>),
    /// Selecting or writing the objects to send failed.
    #[error("generating the pack")]
    Pack(#[source] Box<crate::push::PushFailure>),
    /// Reading a commit for a shallow request failed.
    #[error("reading a commit")]
    Commit(#[source] Box<crate::ReadCommitError>),
}

impl From<crate::packet::Error> for ServeError {
    fn from(error: crate::packet::Error) -> Self {
        match error {
            crate::packet::Error::Io(error) => Self::Io(error),
            crate::packet::Error::Protocol(reason) => Self::Protocol(reason),
            crate::packet::Error::Limit(reason) => Self::Limit(reason),
            crate::packet::Error::Cancelled => Self::Cancelled,
            crate::packet::Error::Remote(_) => Self::Protocol("ERR packet from client"),
        }
    }
}

impl From<crate::refs::ReferenceError> for ServeError {
    fn from(error: crate::refs::ReferenceError) -> Self {
        Self::References(Box::new(error))
    }
}

impl From<crate::ObjectReadError> for ServeError {
    fn from(error: crate::ObjectReadError) -> Self {
        Self::Objects(Box::new(error))
    }
}

impl From<crate::HistoryError> for ServeError {
    fn from(error: crate::HistoryError) -> Self {
        Self::History(Box::new(error))
    }
}

impl From<crate::PeelError> for ServeError {
    fn from(error: crate::PeelError) -> Self {
        Self::Peel(Box::new(error))
    }
}

impl From<crate::push::PushFailure> for ServeError {
    fn from(error: crate::push::PushFailure) -> Self {
        match error {
            crate::push::PushFailure::Cancelled => Self::Cancelled,
            error => Self::Pack(Box::new(error)),
        }
    }
}
