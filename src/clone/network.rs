use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;

use super::{CloneHead, CloneReady, CloneRequest, CloneTransferError, plan, workflow};
use crate::fetch::{DownloadedFetch, FetchLimits};
use crate::transport::TransportControl;

/// Owned network bytes and exact advertisement-derived clone selection, without destination I/O.
///
/// This value is `Send + Sync + 'static`. Move it to a caller-managed bounded worker; dropping it
/// only releases bytes. The caller owns runtime, queue/worker concurrency and cancellation
/// lifetime.
///
/// # Examples
///
/// Clone over HTTPS: download on the async runtime, then validate and finish on a blocking
/// thread. This needs a server and the `http` feature, so it is not run:
///
/// ```no_run
/// # #[cfg(feature = "http")]
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::ops::ControlFlow;
/// use std::sync::atomic::AtomicBool;
///
/// use girt::InitKind;
/// use girt::clone::{BranchSelection, CloneRequest};
/// use girt::fetch::{FetchLimits, FetchUpdateLimits};
/// use girt::refs::Reflog;
/// use girt::transport::TransportControl;
/// use girt::transport::http::HttpRemote;
///
/// let url = "https://example.com/project.git";
/// let request = CloneRequest::prepare_tracking(
///     "project",
///     InitKind::Worktree,
///     url.as_bytes(),
///     BranchSelection::Default,
///     Reflog::Preserve,
/// )?;
/// let runtime = tokio::runtime::Builder::new_current_thread()
///     .enable_all()
///     .build()?;
/// runtime
///     .block_on(async {
///         let remote = HttpRemote::new(url, &[], &[])?;
///         let cancel = AtomicBool::new(false);
///         let download = request
///             .receive_http(
///                 &remote,
///                 FetchLimits::default(),
///                 TransportControl::new(&cancel),
///             )
///             .await?;
///         tokio::task::spawn_blocking(
///             move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
///                 let cancel = AtomicBool::new(false);
///                 let ready = download.validate(&cancel, |_| ControlFlow::Continue(()))?;
///                 ready.finish(FetchUpdateLimits::default(), &cancel)?;
///                 Ok(())
///             },
///         )
///         .await??;
///         Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
///     })
///     .map_err(|error| error as Box<dyn std::error::Error>)?;
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "http"))]
/// # fn main() {}
/// ```
#[derive(Debug)]
pub struct CloneDownload {
    request: CloneRequest,
    head: CloneHead,
    downloaded: DownloadedFetch,
}
impl CloneDownload {
    /// Validates the owned full transfer synchronously without creating the destination.
    ///
    /// Transport deadlines have ended. Cancellation is cooperative; join the worker before
    /// releasing its resources. See [`DownloadedFetch::validate`] for validation budgets.
    ///
    /// # Errors
    ///
    /// Protocol, object, connectivity, limit and cancellation failures leave the destination
    /// absent.
    pub fn validate(
        self,
        cancel: &AtomicBool,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> Result<CloneReady, CloneTransferError> {
        self.validate_with_progress(cancel, progress, |_| {})
    }

    /// Validates with cumulative object and delta counts before destination publication.
    ///
    /// Uses [`DownloadedFetch::validate_with_progress`]'s notification and replay contracts.
    /// Pass a no-op message callback when network notices have already been displayed.
    ///
    /// # Errors
    ///
    /// Returns [`Self::validate`]'s errors, leaving the destination absent.
    pub fn validate_with_progress(
        self,
        cancel: &AtomicBool,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
        observe: impl FnMut(crate::fetch::ValidationProgress),
    ) -> Result<CloneReady, CloneTransferError> {
        Ok(CloneReady {
            request: self.request,
            head: self.head,
            received: self
                .downloaded
                .validate_with_progress(cancel, progress, observe)?,
        })
    }
}
impl CloneRequest {
    /// Downloads from an explicit HTTP endpoint with its explicit credentials/trust policy.
    ///
    /// Uses this discovery response to select IDs, then requests exactly those IDs. HTTP discovery
    /// and RPC are separate server observations: changed/unavailable IDs may cause transfer
    /// failure, but never change the plan silently. No synchronous storage/validation runs in
    /// the future. The stored URL is separate caller metadata; it is not used to choose this
    /// endpoint.
    ///
    /// # Errors
    ///
    /// Returns selection or sanitized HTTP/protocol errors without creating the destination.
    #[cfg(feature = "http")]
    pub async fn receive_http(
        self,
        remote: &crate::transport::http::HttpRemote,
        limits: FetchLimits,
        control: TransportControl<'_>,
    ) -> Result<CloneDownload, CloneTransferError> {
        self.receive_http_with_progress(remote, limits, control, |_| {})
            .await
    }

    /// Downloads a clone while forwarding live receiver sideband messages.
    ///
    /// Uses [`crate::fetch::receive_http_with_progress`]'s advisory message and replay contracts.
    /// No validation or destination I/O runs while polling this future.
    ///
    /// # Errors
    ///
    /// Returns [`Self::receive_http`]'s errors; messages do not establish a valid clone.
    #[cfg(feature = "http")]
    pub async fn receive_http_with_progress(
        self,
        remote: &crate::transport::http::HttpRemote,
        limits: FetchLimits,
        control: TransportControl<'_>,
        progress: impl FnMut(&[u8]),
    ) -> Result<CloneDownload, CloneTransferError> {
        let mut saved = None;
        let download = crate::fetch::receive_http_with_progress(
            remote,
            |ad| plan::select(self.plan(ad), &mut saved),
            None,
            limits,
            control,
            progress,
        )
        .await;
        let (plan, downloaded) = workflow::selected(saved, download)?;
        Ok(CloneDownload {
            request: self,
            head: plan.head,
            downloaded,
        })
    }

    /// Downloads from an explicit SSH endpoint/config using this service session's advertisement.
    ///
    /// No known history is offered, and no storage/validation runs inside network polling. The
    /// stored URL is independent metadata, not endpoint discovery. Uses fetch SSH's process
    /// cleanup, host trust, deadline and credential contracts.
    ///
    /// # Errors
    ///
    /// Returns planning or sanitized SSH/protocol errors with no destination effects.
    ///
    /// # Panics
    ///
    /// Panics without the Tokio I/O/time runtime required by [`crate::fetch::receive_ssh`].
    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    pub async fn receive_ssh(
        self,
        remote: &crate::transport::ssh::SshRemote,
        limits: FetchLimits,
        control: TransportControl<'_>,
    ) -> Result<CloneDownload, CloneTransferError> {
        let mut saved = None;
        let download = crate::fetch::receive_ssh(
            remote,
            |ad| plan::select(self.plan(ad), &mut saved),
            None,
            limits,
            control,
        )
        .await;
        let (plan, downloaded) = workflow::selected(saved, download)?;
        Ok(CloneDownload {
            request: self,
            head: plan.head,
            downloaded,
        })
    }
}
