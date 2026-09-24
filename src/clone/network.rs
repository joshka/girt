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
        Ok(CloneReady {
            request: self.request,
            head: self.head,
            received: self.downloaded.validate(cancel, progress)?,
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
        let mut saved = None;
        let download = crate::fetch::receive_http(
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
