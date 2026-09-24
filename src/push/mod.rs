//! Conditional reference publication over receive-pack protocol v0.
//!
//! Build a [`PreparedPush`] from explicit commands and a local object reader, then call [`send`]
//! with caller-owned blocking streams or [`send_local`] with a trusted local Git repository.
//! Preparation validates the complete reachable graph and buffers a non-thin pack before any
//! commands can reach a server. Local refs, tracking refs, configuration and reflogs are untouched.
//!
//! Only SHA-1 branches and tags are supported. `report-status` is required; no atomic, sideband,
//! deletion, push-options, signed-push or report-status-v2 features are requested. Multiple
//! commands can partially succeed. The `http` feature adds async smart-HTTP sending with explicit
//! caller-supplied authorization headers; `ssh` adds system OpenSSH on macOS/Linux.
//! Both require a caller-owned Tokio runtime. Credential discovery, refspecs, pruning and
//! automatic force are deferred.

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
mod ssh;
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub use ssh::send_ssh;

#[cfg(feature = "http")]
mod http;
#[cfg(feature = "http")]
pub use http::send_http;

mod graph;
mod local;
mod prepared;
mod protocol;
mod types;

pub use local::{send_local, send_local_with_control};
pub use prepared::PreparedPush;
pub use protocol::send;
pub use types::{
    ForcePolicy, PushCommand, PushError, PushFailure, PushLimits, PushReport, RefStatus, Status,
};

#[cfg(test)]
mod tests;
