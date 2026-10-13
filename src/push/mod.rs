//! Conditional reference publication over receive-pack protocol v0.
//!
//! # Push lifecycle
//!
//! Build [`PushCommand`] values with exact expected old targets, then use [`PreparedPush::new`]
//! or [`PreparedPush::new_local`] to select objects and spool the pack. Choose [`send_local`]
//! for a native local destination or [`send`] for caller-owned protocol streams. A successful
//! exchange can contain rejected refs: inspect [`PushReport`] per command. [`PushError`] retains
//! known acknowledgements when the final outcome is uncertain; read destination refs before
//! retrying.
//!
//! Preparation validates structured objects, locates blobs and spools a pack (possibly thin against
//! advertised history) before any commands can reach a server. Reused compressed entries are
//! CRC-checked; the receiver derives their identities and checks connectivity before publishing
//! refs. With receiver roots, it reads only the history they do not cover plus a boundary; see
//! [`PreparedPush::new_excluding`]. Preparation leaves local refs and reflogs untouched. These
//! functions take an already prepared push, so its roots come from the caller's knowledge.
//! [`crate::Repository::push`] instead prepares after reading the advertisement and excludes
//! [`PushAdvertisement::receiver_roots`], as `git send-pack` does.
//!
//! Wire and native local push support SHA-1 and SHA-256. `report-status` is required by wire push;
//! deletion requires the advertised `delete-refs` capability, and supplied push options require
//! `push-options`. Report-status-v2 is preferred when advertised; optional sideband progress is
//! retained as bounded, untrusted bytes. Atomic and signed pushes are unsupported. Multiple
//! commands can partially succeed. The `http` feature adds async smart-HTTP sending with
//! explicit caller-supplied authorization headers; `ssh` adds system OpenSSH on macOS/Linux.
//! Both require a caller-owned Tokio runtime. [`crate::remote`] maps configured refspecs
//! separately; callers must authorize force and supply exact expected values. Pruning and
//! automatic force are deferred. [`crate::remote::CredentialSession`] is a separate
//! application-approved helper lifecycle; callers attach resulting credentials to their chosen
//! transport and keep authentication approval or rejection under application control.
//! `send_http_checked` lets an HTTP caller inspect validated receive-pack tips and decline a
//! whole batch before POST. A decision does not reserve those tips; the receiver still checks each
//! command's expected old value. `send_ssh_checked_with_progress` provides the same inspection
//! boundary in one SSH session, closing a declined session with only a protocol flush.

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
mod ssh;
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub(crate) use ssh::send_ssh_preparing;
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub use ssh::{
    SshPushOutcome, send_ssh, send_ssh_checked_with_progress, send_ssh_selected_with_progress,
};

#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
mod live_progress;

#[cfg(feature = "http")]
mod http;
#[cfg(feature = "http")]
pub(crate) use http::send_http_preparing;
#[cfg(feature = "http")]
pub use http::{HttpPushOutcome, send_http, send_http_checked, send_http_checked_with_progress};

mod graph;
mod hooks;
mod local;
mod local_native;
mod prepared;
mod progress;
mod protocol;
mod quarantine;
mod types;

pub(crate) use local::send_local_preparing_observed;
pub use local::{
    LocalPushContext, send_local, send_local_with_context, send_local_with_control,
    send_local_with_identity, send_local_with_progress,
};
pub use prepared::PreparedPush;
pub use progress::PreparationProgress;
pub use protocol::{PushAdvertisement, send};
pub use types::{
    ForcePolicy, PushCommand, PushError, PushFailure, PushLimits, PushReport, RefRewrite,
    RefStatus, RejectionOrigin, Status,
};

#[cfg(test)]
mod tests;
