use std::ops::ControlFlow;
use std::path::Path;
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use std::process::Command;
use std::sync::atomic::AtomicBool;

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use super::receive_with_known;
use super::{Advertisement, FetchError, FetchLimits, KnownHistory, ReceivedFetch};
use crate::ObjectId;
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use crate::transport::Server;
use crate::transport::TransportControl;

/// Reads a local repository with girt and constructs a validated reachable pack.
///
/// Opens the explicit path through [`crate::Repository::open`], lists its references, then reads
/// selected reachable objects through girt's storage. No Git executable or helper is started.
/// The path must identify a trusted local repository; use
/// [`crate::remote::Destination::local_path`] to convert a resolved local or `file://` destination. The result owns a pack/index pair but
/// touches no destination until [`ReceivedFetch::install`] is called.
///
/// Supports SHA-1 and SHA-256 with files or reftable references on native local filesystems.
/// Cancellation and deadlines are checked between synchronous operations; one filesystem read,
/// graph parse, hash or compression call cannot be interrupted. Concurrent source updates may
/// produce a mixed advertisement; missing or corrupt selected history fails before publication.
///
/// # Errors
///
/// Returns path, reference, object, graph, limit and interruption failures. No destination is
/// touched.
pub fn receive_local(
    source: impl AsRef<Path>,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    limits: FetchLimits,
    cancel: &AtomicBool,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, FetchError> {
    receive_local_with_control(
        source,
        select,
        limits,
        TransportControl::new(cancel),
        progress,
    )
}

/// Receives objects from native local storage with caller-controlled interruption.
///
/// Uses [`receive_local`]'s path and storage contract. No destination is touched on interruption.
///
/// # Errors
///
/// Returns [`receive_local`]'s failures, [`FetchError::Cancelled`], or [`FetchError::Deadline`].
pub fn receive_local_with_control(
    source: impl AsRef<Path>,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    limits: FetchLimits,
    control: TransportControl<'_>,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, FetchError> {
    receive_local_with_known(
        source,
        select,
        &KnownHistory::default(),
        limits,
        control,
        progress,
    )
}

/// Reads a local source while excluding explicitly verified destination history.
///
/// Source history is fully checked even for objects present in `known`. Excluded objects become
/// dependencies rechecked during installation. Knowledge preparation remains caller-owned.
///
/// # Errors
///
/// Returns path, reference, object, graph, limit and interruption failures.
pub fn receive_local_with_known(
    source: impl AsRef<Path>,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: &KnownHistory,
    limits: FetchLimits,
    control: TransportControl<'_>,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, FetchError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "fetch.local",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
    );

    let operation = || {
        control.check()?;
        let source = crate::Repository::open(source)
            .map_err(|_| FetchError::Protocol("local repository"))?;
        super::local_native::receive(&source, select, known, limits, control, progress)
    };
    #[cfg(feature = "tracing")]
    let result = span.in_scope(operation);
    #[cfg(not(feature = "tracing"))]
    let result = { operation }();
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, crate::trace::fetch);

    result
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
fn receive_server(
    command: &mut Command,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    limits: FetchLimits,
    control: TransportControl<'_>,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, FetchError> {
    receive_server_with_known(
        command,
        select,
        &KnownHistory::default(),
        limits,
        control,
        progress,
    )
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
fn receive_server_with_known(
    command: &mut Command,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: &KnownHistory,
    limits: FetchLimits,
    control: TransportControl<'_>,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<ReceivedFetch, FetchError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "fetch.local_server",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
    );

    let operation = || {
        let mut child = Server::spawn(command, control)?;
        let received = {
            let (mut reader, mut writer) = child.streams();
            receive_with_known(
                &mut reader,
                &mut writer,
                select,
                known,
                limits,
                control.cancel,
                progress,
            )?
        };
        let status = child.wait()?;
        if !status.success() {
            return Err(FetchError::Process(status));
        }
        Ok(received)
    };
    #[cfg(feature = "tracing")]
    let result = span.in_scope(operation);
    #[cfg(not(feature = "tracing"))]
    let result = { operation }();
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, crate::trace::fetch);

    result
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests;
