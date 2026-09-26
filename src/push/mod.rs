//! Conditional reference publication over receive-pack protocol v0.
//!
//! Build a [`PreparedPush`] from explicit commands and a local object reader, then call [`send`]
//! with caller-owned blocking streams or [`send_local`] with a trusted local repository.
//! Preparation validates the complete reachable graph and buffers a non-thin pack before any
//! commands can reach a server. Preparation leaves local refs and reflogs untouched.
//!
//! Wire and native local push support SHA-1 and SHA-256. `report-status` is required by wire push;
//! deletion requires the advertised `delete-refs` capability, and supplied push options require
//! `push-options`. Report-status-v2 is preferred when advertised; optional sideband progress is
//! retained as bounded, untrusted bytes. Atomic and signed pushes are unsupported. Multiple
//! commands can partially succeed. The `http` feature adds async smart-HTTP sending with
//! explicit caller-supplied authorization headers; `ssh` adds system OpenSSH on macOS/Linux.
//! Both require a caller-owned Tokio runtime. [`crate::remote`] maps configured refspecs
//! separately; callers must authorize force and supply exact expected values. Credential discovery,
//! pruning and automatic force are deferred.

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
mod local_native;
mod prepared;
mod protocol;
mod types;

pub use local::{send_local, send_local_with_control, send_local_with_identity};
pub use prepared::PreparedPush;
pub use protocol::send;
pub use types::{
    ForcePolicy, PushCommand, PushError, PushFailure, PushLimits, PushReport, RefRewrite,
    RefStatus, Status,
};

#[cfg(test)]
mod tests;
