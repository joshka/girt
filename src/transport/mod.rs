//! Caller control for owned transports; protocol streams remain caller-owned and cooperative.

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub mod ssh;

mod control;
#[cfg(feature = "http")]
pub mod http;
pub use control::TransportControl;
pub(crate) use control::{Interruption, interruption};

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod process;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use process::Server;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod unsupported;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) use unsupported::Server;
