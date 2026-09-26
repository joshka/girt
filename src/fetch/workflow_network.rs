use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::workflow::{select, selected};
use super::{
    DownloadedFetch, FetchLimits, FetchReady, FetchRequest, FetchUpdate, FetchWorkflowError,
    KnownHistory,
};
use crate::transport::TransportControl;

/// An owned network download paired with its advertisement-derived fetch plan.
///
/// Move this `Send + Sync + 'static` value to a caller-controlled bounded worker and call
/// [`Self::validate`] before installation/publication. No filesystem or object-validation work runs
/// inside the network future. Dropping this value releases bytes/history without storage effects.
#[derive(Debug)]
pub struct FetchDownload {
    request: FetchRequest,
    updates: Vec<FetchUpdate>,
    downloaded: DownloadedFetch,
}

impl FetchDownload {
    /// Validates packet, pack and selected-tip connectivity synchronously without storage changes.
    ///
    /// Retains the exact history owned by the download; callers cannot replace validation evidence.
    /// Cancellation is cooperative and the transport deadline has ended. See
    /// [`DownloadedFetch::validate`] for budgets and worker-lifetime obligations.
    ///
    /// # Errors
    ///
    /// Returns protocol, object, connectivity, limit or cancellation failures before installation.
    pub fn validate(
        self,
        cancel: &AtomicBool,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> Result<FetchReady, FetchWorkflowError> {
        Ok(FetchReady {
            request: self.request,
            updates: self.updates,
            received: self.downloaded.validate(cancel, progress)?,
        })
    }
}

impl FetchRequest {
    /// Downloads with explicit HTTP endpoint/credentials and caller-owned history.
    ///
    /// Plans from this discovery response, not an earlier preview. Only in-memory mapping runs in
    /// the selection callback. Move the returned owned download to a synchronous worker for
    /// validation, installation and publication. Uses [`super::receive_http`]'s runtime, buffering,
    /// deadline and authentication contracts; does not retry a changed server advertisement.
    ///
    /// # Errors
    ///
    /// Returns mapping/policy errors or sanitized HTTP/protocol failures, without storage effects.
    #[cfg(feature = "http")]
    pub async fn receive_http(
        self,
        remote: &crate::transport::http::HttpRemote,
        known: Option<Arc<KnownHistory>>,
        limits: FetchLimits,
        control: TransportControl<'_>,
    ) -> Result<FetchDownload, FetchWorkflowError> {
        self.check_known(known.as_deref().unwrap_or(&KnownHistory::default()))?;
        let mut plan = None;
        let downloaded = super::receive_http_with_depth(
            remote,
            |advertisement| select(self.plan(advertisement), &mut plan),
            known,
            self.depth,
            limits,
            control,
        )
        .await;
        let updates = selected(plan, &downloaded)?;
        Ok(FetchDownload {
            request: self,
            updates,
            downloaded: downloaded?,
        })
    }

    /// Downloads with explicit SSH endpoint/configuration and caller-owned history.
    ///
    /// Plans from the advertisement in this SSH service session. Only in-memory mapping runs in
    /// the selection callback. Validation and storage are separate synchronous calls on a
    /// caller-controlled worker. Uses [`super::receive_ssh`]'s runtime and transport contracts.
    ///
    /// # Errors
    ///
    /// Returns mapping/policy errors or sanitized SSH/protocol failures, without storage effects.
    ///
    /// # Panics
    ///
    /// Panics without the Tokio I/O/time runtime required by [`super::receive_ssh`].
    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    pub async fn receive_ssh(
        self,
        remote: &crate::transport::ssh::SshRemote,
        known: Option<Arc<KnownHistory>>,
        limits: FetchLimits,
        control: TransportControl<'_>,
    ) -> Result<FetchDownload, FetchWorkflowError> {
        self.check_known(known.as_deref().unwrap_or(&KnownHistory::default()))?;
        let mut plan = None;
        let downloaded = super::receive_ssh_with_depth(
            remote,
            |advertisement| select(self.plan(advertisement), &mut plan),
            known,
            self.depth,
            limits,
            control,
        )
        .await;
        let updates = selected(plan, &downloaded)?;
        Ok(FetchDownload {
            request: self,
            updates,
            downloaded: downloaded?,
        })
    }
}
