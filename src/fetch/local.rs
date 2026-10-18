use std::num::NonZeroU32;
use std::ops::ControlFlow;
use std::path::Path;
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use std::process::Command;

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
/// A missing canonical empty tree is materialized from its verified identity; other missing
/// reachable objects fail before installation.
/// The [`super::FetchRequest`] workflow can truncate one selected commit tip at depth one;
/// standalone local receives retain complete history.
/// Cancellation and deadlines are checked between synchronous operations; one filesystem read,
/// graph parse, hash or compression call cannot be interrupted. Concurrent source updates may
/// produce a mixed advertisement; missing or corrupt selected history fails before publication.
///
/// With verified history, complete known boundaries are excluded and become dependencies rechecked
/// during [`ReceivedFetch::install`]. Declared shallow roots are expanded. Selected blobs are
/// located by framing rather than retained as decoded payloads; the spooled pack is independently
/// imported to check identities, delta depth and connectivity. The caller owns history preparation
/// and protection against concurrent destination collection.
///
/// Validation snapshots report actual object and delta work; a pack-free fetch has no snapshots.
/// Completion describes validation only. A later deadline check or enclosing request's planning
/// can still fail before installation or reference publication.
///
/// # Panics
///
/// Observer panics unwind normally. No destination storage has been installed by this operation.
///
/// # Errors
///
/// Returns path, reference, object, graph, limit and interruption failures. No destination is
/// touched.
///
/// # Examples
///
/// Copy the objects of one advertised tip into a repository without creating any reference, for
/// example to make a commit available before deciding what to name it:
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::PackLimits;
/// use girt::fetch::{FetchLimits, LocalFetchObserver, LocalFetchRequest, receive_local};
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, main, _) = include!("../doctest/remote.rs");
///
/// let cancel = AtomicBool::new(false);
/// let received = receive_local(
///     upstream.git_dir(),
///     // Choose the wants from the advertisement; here, the tip of `main`.
///     |advertisement| {
///         advertisement
///             .refs
///             .iter()
///             .filter(|reference| reference.name.as_bytes() == b"refs/heads/main")
///             .map(|reference| reference.id)
///             .collect()
///     },
///     LocalFetchRequest::new(FetchLimits::default()),
///     TransportControl::new(&cancel),
///     LocalFetchObserver::default(),
/// )?;
///
/// assert_eq!(received.wants(), [main]);
///
/// let installed = received.install(&repo, PackLimits::default(), &cancel)?;
/// assert_eq!(installed.objects, 2);
/// // The objects are readable, but no reference was created.
/// let objects = repo.objects_with(PackLimits::default())?;
/// assert!(
///     objects
///         .read_with(main, girt::ReadLimits::default())?
///         .is_some()
/// );
/// assert!(repo.references()?.list()?.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn receive_local(
    source: impl AsRef<Path>,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    request: super::LocalFetchRequest<'_>,
    control: TransportControl<'_>,
    mut observer: super::LocalFetchObserver<'_>,
) -> Result<ReceivedFetch, FetchError> {
    let empty = KnownHistory::default();
    receive_local_observed(
        source,
        select,
        request.known.unwrap_or(&empty),
        None,
        request.limits,
        control,
        |message| {
            observer
                .messages
                .as_mut()
                .map_or(ControlFlow::Continue(()), |notify| notify(message))
        },
        |event| {
            if let Some(notify) = observer.construction.as_mut() {
                notify(event);
            }
        },
        |event| {
            if let Some(notify) = observer.validation.as_mut() {
                notify(event);
            }
        },
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "local transfer keeps explicit progress policy"
)]
pub(super) fn receive_local_observed(
    source: impl AsRef<Path>,
    select: impl FnOnce(&Advertisement) -> Vec<ObjectId>,
    known: &KnownHistory,
    depth: Option<NonZeroU32>,
    limits: FetchLimits,
    control: TransportControl<'_>,
    progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    observe: impl FnMut(super::LocalFetchProgress),
    validation: impl FnMut(super::ValidationProgress),
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
        let source =
            crate::Repository::open(source).map_err(|error| FetchError::Source(Box::new(error)))?;
        super::local_native::receive_depth_observed(
            &source, select, known, depth, limits, control, progress, observe, validation,
        )
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
