//! Caller control for owned transports; protocol streams remain caller-owned and cooperative.
//!
//! [`TransportControl`] carries a cancellation flag and optional deadline into native local,
//! HTTP, and SSH adapters. It checks between work steps; it cannot interrupt a synchronous hash,
//! filesystem call, or caller-owned blocking stream. The `http` feature adds owned HTTP transfers;
//! `ssh` adds an OpenSSH adapter on macOS/Linux. Those adapters document their runtime, process,
//! authentication, and cleanup contracts at the endpoint.
//!
//! For a full fetch lifecycle start with [`crate::fetch::FetchRequest`]; for push, prepare
//! [`crate::push::PreparedPush`] before choosing a transport. Applications own worker scheduling
//! and must inspect partial outcomes before retrying publication.

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub mod ssh;

mod control;
#[cfg(feature = "http")]
pub mod http;
pub use control::TransportControl;
pub(crate) use control::{Interruption, interruption};

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod process;
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[cfg(test)]
pub(crate) use process::Server;
