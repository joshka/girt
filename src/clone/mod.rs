//! Clone into a new bare repository or an ordinary repository without checkout.
//!
//! [`CloneRequest::prepare_tracking`] explicitly chooses a remote-tracking reference layout,
//! selecting all branches and tags from the actual transfer advertisement.
//! [`CloneReady::finish`] exclusively creates the destination, installs the validated transfer,
//! saves `origin` configuration, and sets the selected branch/HEAD. Both layouts use
//! `refs/remotes/origin/*`; bare clone does not copy every branch into `refs/heads/*`.
//! No index or worktree files are created. An ordinary clone therefore has an absent index,
//! and Git status can show tracked files as deleted until the caller performs a checkout.
//!
//! Transfer and validation have no destination effects. Finish reports completed phases and
//! preserves residual state on failure; it never rolls back or deletes the destination. See
//! `examples/clone_repository.rs` for a disposable local workflow. HTTP/SSH return owned downloads
//! for validation on caller-managed bounded workers, exactly as the fetch adapters do.

mod config;
#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
mod network;
mod plan;
mod workflow;

#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
pub use network::CloneDownload;
pub use plan::{BranchSelection, CloneHead, ClonePlan, ClonePlanError};
pub use workflow::{
    CloneError, CloneFailure, CloneReady, CloneReport, CloneRequest, CloneTransferError,
};
