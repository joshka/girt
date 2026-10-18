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
/// [`Self::validate`] before installation/publication. The network future spools response bytes to
/// a temporary file; object validation runs on the worker. Dropping this value removes the
/// temporary file and releases history without changing repository files.
///
/// # Examples
///
/// Download on the async runtime, validate on a blocking thread, then publish. Network I/O is
/// async; validation hashes every object and belongs off the executor. This example needs a
/// server and the `http` feature, so it is not run:
///
/// ```no_run
/// # #[cfg(feature = "http")]
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::collections::BTreeSet;
/// use std::ops::ControlFlow;
/// use std::sync::atomic::AtomicBool;
///
/// use girt::Repository;
/// use girt::fetch::{FetchLimits, FetchRequest, FetchUpdateLimits};
/// use girt::refs::Reflog;
/// use girt::remote::Remote;
/// use girt::transport::TransportControl;
/// use girt::transport::http::HttpRemote;
///
/// let runtime = tokio::runtime::Builder::new_current_thread()
///     .enable_all()
///     .build()?;
/// runtime
///     .block_on(async {
///         let repo = Repository::open("project")?;
///         let origin = Remote::find(repo.config(), b"origin")?.ok_or("no origin")?;
///         let request = FetchRequest::prepare(
///             repo.clone(),
///             origin.fetch_refspecs().clone(),
///             BTreeSet::new(),
///             Reflog::Preserve,
///         )?;
///         let remote = HttpRemote::new("https://example.com/project.git", &[], &[])?;
///
///         let cancel = AtomicBool::new(false);
///         let download = request
///             .receive_http(
///                 &remote,
///                 None,
///                 FetchLimits::default(),
///                 TransportControl::new(&cancel),
///             )
///             .await?;
///
///         // The download owns everything it needs, so it can move to a blocking thread.
///         let report = tokio::task::spawn_blocking(
///             move || -> Result<_, Box<dyn std::error::Error + Send + Sync>> {
///                 let cancel = AtomicBool::new(false);
///                 let ready = download.validate(&cancel, |_| ControlFlow::Continue(()))?;
///                 Ok(ready.finish(FetchUpdateLimits::default(), &cancel)?)
///             },
///         )
///         .await??;
///
///         println!("updated {} references", report.references.len());
///         Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
///     })
///     .map_err(|error| error as Box<dyn std::error::Error>)?;
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "http"))]
/// # fn main() {}
/// ```
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
        self.validate_with_progress(cancel, progress, |_| {})
    }

    /// Validates with local object/delta snapshots before installation or reference publication.
    ///
    /// Uses [`DownloadedFetch::validate_with_progress`]'s phase-count, completion, cancellation
    /// and sideband-replay contracts. The observer runs on the calling validation worker.
    ///
    /// # Errors
    ///
    /// Returns [`Self::validate`]'s errors; earlier snapshots do not establish valid objects.
    pub fn validate_with_progress(
        self,
        cancel: &AtomicBool,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
        observe: impl FnMut(super::ValidationProgress),
    ) -> Result<FetchReady, FetchWorkflowError> {
        Ok(FetchReady {
            request: self.request,
            updates: self.updates,
            received: self
                .downloaded
                .validate_with_progress(cancel, progress, observe)?,
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
        self.receive_http_with_progress(remote, known, limits, control, |_| {})
            .await
    }

    /// Downloads with live receiver notices and a discovery-derived publication plan.
    ///
    /// Uses [`super::receive_http_with_progress`]'s borrowed byte callback, advisory output and
    /// validation replay contracts. Pass a no-op progress callback to [`FetchDownload::validate`]
    /// when notices have already been displayed. Validation, installation and publication remain
    /// explicit; this call makes no storage changes.
    ///
    /// # Errors
    ///
    /// Returns [`Self::receive_http`]'s mapping, policy and transport errors. Progress may precede
    /// a later failure and does not establish valid objects or published references.
    #[cfg(feature = "http")]
    pub async fn receive_http_with_progress(
        self,
        remote: &crate::transport::http::HttpRemote,
        known: Option<Arc<KnownHistory>>,
        limits: FetchLimits,
        control: TransportControl<'_>,
        progress: impl FnMut(&[u8]),
    ) -> Result<FetchDownload, FetchWorkflowError> {
        self.check_known(known.as_deref().unwrap_or(&KnownHistory::default()))?;
        let mut plan = None;
        let downloaded = super::receive_http_with_depth_and_progress(
            remote,
            |advertisement| select(self.plan(advertisement), &mut plan),
            known,
            self.depth,
            limits,
            control,
            progress,
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
        self.receive_ssh_with_progress(remote, known, limits, control, |_| {}, |_| {})
            .await
    }

    /// Downloads SSH history with separate local diagnostics and live remote notices.
    ///
    /// Uses [`super::receive_ssh_with_progress`]'s bounded observers and validation replay
    /// contract. Pass a no-op notice callback to [`FetchDownload::validate_with_progress`]
    /// after displaying notices live. Validation, installation and publication remain explicit
    /// and caller-owned.
    ///
    /// # Errors
    ///
    /// Returns [`Self::receive_ssh`]'s mapping, policy and transport errors. Observed output does
    /// not establish valid objects or published references; a launched failure is never retried.
    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    pub async fn receive_ssh_with_progress(
        self,
        remote: &crate::transport::ssh::SshRemote,
        known: Option<Arc<KnownHistory>>,
        limits: FetchLimits,
        control: TransportControl<'_>,
        diagnostics: impl FnMut(&[u8]),
        progress: impl FnMut(&[u8]),
    ) -> Result<FetchDownload, FetchWorkflowError> {
        self.check_known(known.as_deref().unwrap_or(&KnownHistory::default()))?;
        let mut plan = None;
        let downloaded = super::receive_ssh_with_progress(
            remote,
            |advertisement| select(self.plan(advertisement), &mut plan),
            known,
            super::FetchOptions {
                limits,
                depth: self.depth,
            },
            control,
            diagnostics,
            progress,
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
