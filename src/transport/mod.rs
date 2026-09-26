//! Caller control for owned transports; protocol streams remain caller-owned and cooperative.

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
