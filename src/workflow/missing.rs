//! Fetching objects a partial clone's promisor remotes promised, in explicit batches.

use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;

use super::{Endpoint, Environment, TransferCallbacks, TransferError, is_stream_failure};
use crate::fetch::{FetchLimits, FetchWorkflowError, KnownHistory, ObjectFilter};
use crate::remote::{Direction, Remote};
use crate::{IdSet, MissingObjectsError, ObjectId, Repository};

/// What [`fetch_missing`] or [`fetch_promised`] fetched.
///
/// # Examples
///
/// ```
/// use girt::workflow::PromisorFetch;
///
/// let report = PromisorFetch::default();
/// assert!(report.fetched.is_empty() && report.already_present.is_empty());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PromisorFetch {
    /// Requested objects the remote sent, in request order without duplicates.
    pub fetched: Vec<ObjectId>,
    /// Requested objects the repository already stored, so they were not requested.
    pub already_present: Vec<ObjectId>,
}

/// Lists the promisor remotes [`fetch_promised`] asks, in the order it asks them.
///
/// A remote is a promisor when `remote.<name>.promisor` is true or `extensions.partialClone`
/// names it, the settings `git clone --filter` writes. Like Git, remotes with `promisor = true`
/// come first in configuration order, and the remote `extensions.partialClone` names comes last.
/// Remote names that are not UTF-8 are left out, because the transfer functions take `&str` names.
///
/// # Examples
///
/// ```
/// use girt::{InitKind, ObjectFormat, Repository, workflow};
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r.git"),
///     InitKind::Bare,
/// )?;
/// assert!(workflow::promisor_remotes(&repo).is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn promisor_remotes(repository: &Repository) -> Vec<String> {
    let config = repository.config();
    let partial_clone = config
        .string("extensions", None, "partialClone")
        .filter(|name| !name.is_empty());
    let mut names: Vec<String> = Remote::names(config)
        .into_iter()
        .filter(|&name| Some(name) != partial_clone)
        .filter(|&name| config.boolean("remote", Some(name), "promisor") == Ok(Some(true)))
        .filter_map(|name| String::from_utf8(name.to_vec()).ok())
        .collect();
    if let Some(name) = partial_clone.and_then(|name| String::from_utf8(name.to_vec()).ok()) {
        names.push(name);
    }
    names
}

/// Fetches objects a partial clone is missing from one promisor remote.
///
/// Object reads never contact a remote: a promised object that isn't stored reads as absent. Call
/// this to fetch a batch of such objects explicitly, as Git's lazy fetch does when a command reads
/// one. Like that fetch, this sends one want per object that isn't already stored, no haves, and
/// the `blob:none` filter, so asking for a tree or commit brings its trees but not their files.
/// The received pack is validated, then installed with a `.promisor` marker. No reference
/// changes.
///
/// The remote must be a promisor (see [`promisor_remotes`]) and must allow wants for objects that
/// no reference names. Git's `upload-pack` allows them in protocol v2, which this module requests
/// by default, but not in v0 unless `uploadpack.allowAnySHA1InWant` or a related setting is on.
/// Local paths and `file://` URLs read the other repository directly and always allow them, but
/// send whole objects: a commit brings its full history.
///
/// Open a new [`Objects`](crate::Objects) reader, or reopen the repository, to read the fetched
/// objects.
///
/// # Examples
///
/// Asking a remote that isn't a promisor fails before anything is sent:
///
/// ```
/// use girt::workflow::{self, Environment, NoCallbacks, TransferError};
/// use girt::{ObjectFormat, ObjectKind};
/// # let (_directory, _, repo, _, _) = include!("../doctest/remote.rs");
///
/// let blob = ObjectFormat::Sha1.hash_object(ObjectKind::Blob, b"some file\n");
/// let result = workflow::fetch_missing(
///     &repo,
///     "origin",
///     &[blob],
///     &Environment::default(),
///     &mut NoCallbacks,
/// );
/// assert!(matches!(result, Err(TransferError::NotPromisor(name)) if name == "origin"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// Returns [`TransferError::NotPromisor`] when the remote isn't a promisor, and
/// [`TransferError::NotFetched`] when the remote's pack lacks some requested objects; that pack is
/// still installed, so the objects it did send are readable. Otherwise returns the errors of
/// [`fetch`](super::fetch) other than reference publication failures.
/// [`TransferError::FetchFinish`] means installation failed after the pack, and possibly its
/// `.promisor` marker, may have been published without an index; readers ignore such a pack and
/// fetching again is safe. Every other error except `NotFetched` means no local change was made.
pub fn fetch_missing(
    repository: &Repository,
    remote_name: &str,
    ids: &[ObjectId],
    environment: &Environment,
    callbacks: &mut dyn TransferCallbacks,
) -> Result<PromisorFetch, TransferError> {
    if !is_promisor(repository, remote_name) {
        return Err(TransferError::NotPromisor(remote_name.to_owned()));
    }
    let (wants, already_present) = partition(repository, ids)?;
    if wants.is_empty() {
        return Ok(PromisorFetch {
            fetched: Vec::new(),
            already_present,
        });
    }
    let sent = receive(repository, remote_name, &wants, environment, callbacks)?;
    let (fetched, missing): (Vec<_>, Vec<_>) = wants.into_iter().partition(|id| sent.contains(id));
    if !missing.is_empty() {
        return Err(TransferError::NotFetched(missing));
    }
    Ok(PromisorFetch {
        fetched,
        already_present,
    })
}

/// Fetches objects a partial clone is missing from its promisor remotes, trying each in turn.
///
/// Asks the remotes [`promisor_remotes`] lists, in that order, for the objects still missing,
/// as Git's lazy fetch does. A remote that fails or sends only some objects is followed by the
/// next one. See [`fetch_missing`] for what each fetch sends and installs.
///
/// # Examples
///
/// A repository without promisor remotes has nowhere to fetch from:
///
/// ```
/// use girt::workflow::{self, Environment, NoCallbacks, TransferError};
/// use girt::{ObjectFormat, ObjectKind};
/// # let (_directory, _, repo, _, _) = include!("../doctest/remote.rs");
///
/// let blob = ObjectFormat::Sha1.hash_object(ObjectKind::Blob, b"some file\n");
/// let result =
///     workflow::fetch_promised(&repo, &[blob], &Environment::default(), &mut NoCallbacks);
/// assert!(matches!(result, Err(TransferError::NoPromisorRemote)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// Returns [`TransferError::NoPromisorRemote`] when no remote is a promisor. When objects are
/// still missing after every remote, returns the first remote's error if any remote failed, and
/// otherwise [`TransferError::NotFetched`] listing them. Packs installed before the failure stay
/// installed.
pub fn fetch_promised(
    repository: &Repository,
    ids: &[ObjectId],
    environment: &Environment,
    callbacks: &mut dyn TransferCallbacks,
) -> Result<PromisorFetch, TransferError> {
    let remotes = promisor_remotes(repository);
    if remotes.is_empty() {
        return Err(TransferError::NoPromisorRemote);
    }
    let (mut wants, already_present) = partition(repository, ids)?;
    // Every requested object is fetched on success, so the report keeps request order.
    let fetched = wants.clone();
    let mut first_error = None;
    for remote in &remotes {
        if wants.is_empty() {
            break;
        }
        match receive(repository, remote, &wants, environment, callbacks) {
            Ok(sent) => {
                wants.retain(|id| !sent.contains(id));
            }
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    if !wants.is_empty() {
        return Err(first_error.unwrap_or(TransferError::NotFetched(wants)));
    }
    Ok(PromisorFetch {
        fetched,
        already_present,
    })
}

/// Fetches, in one batch, the objects an operation reported missing, so it can run again.
///
/// Library operations never fetch. Checkout, patch rendering, tree merges and blame fail with
/// [`MissingObjectsError`] when a partial clone lacks objects they need; checkout and patches list
/// every missing blob at once. Pass that error here, then repeat the operation with a new
/// [`Objects`](crate::Objects) reader or a reopened repository. This is [`fetch_promised`] for
/// [`MissingObjectsError::ids`].
///
/// # Examples
///
/// ```
/// use girt::workflow::{self, Environment, NoCallbacks, TransferError};
/// use girt::{MissingObjectsError, ObjectFormat, ObjectKind};
/// # let (_directory, _, repo, _, _) = include!("../doctest/remote.rs");
///
/// let blob = ObjectFormat::Sha1.hash_object(ObjectKind::Blob, b"some file\n");
/// let missing = MissingObjectsError::new(vec![(blob, ObjectKind::Blob)]);
/// let result = workflow::fetch_needed(&repo, &missing, &Environment::default(), &mut NoCallbacks);
/// assert!(matches!(result, Err(TransferError::NoPromisorRemote)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// Returns the errors of [`fetch_promised`].
pub fn fetch_needed(
    repository: &Repository,
    missing: &MissingObjectsError,
    environment: &Environment,
    callbacks: &mut dyn TransferCallbacks,
) -> Result<PromisorFetch, TransferError> {
    let ids: Vec<ObjectId> = missing.ids().collect();
    fetch_promised(repository, &ids, environment, callbacks)
}

/// Whether `remote.<name>.promisor` is true or `extensions.partialClone` names the remote.
fn is_promisor(repository: &Repository, remote_name: &str) -> bool {
    let config = repository.config();
    let name = remote_name.as_bytes();
    config.string("extensions", None, "partialClone") == Some(name)
        || config.boolean("remote", Some(name), "promisor") == Ok(Some(true))
}

/// Splits `ids` into objects to request, deduplicated in order, and objects already stored.
fn partition(
    repository: &Repository,
    ids: &[ObjectId],
) -> Result<(Vec<ObjectId>, Vec<ObjectId>), TransferError> {
    let objects = repository
        .objects_with(crate::PackLimits::trusted())
        .map_err(TransferError::local)?;
    let format = repository.object_format();
    let mut seen = IdSet::default();
    let mut wants = Vec::new();
    let mut present = Vec::new();
    for &id in ids {
        if id.format() != format {
            return Err(TransferError::local(
                crate::fetch::FetchError::FormatMismatch {
                    remote: id.format(),
                    local: format,
                },
            ));
        }
        if !seen.insert(id) {
            continue;
        }
        if objects.contains(id).map_err(TransferError::local)? {
            present.push(id);
        } else {
            wants.push(id);
        }
    }
    Ok((wants, present))
}

/// Fetches `wants` from one remote and installs the pack, returning the wants it contained.
fn receive(
    repository: &Repository,
    remote_name: &str,
    wants: &[ObjectId],
    environment: &Environment,
    callbacks: &mut dyn TransferCallbacks,
) -> Result<IdSet, TransferError> {
    let (endpoint, _) =
        repository.endpoint(remote_name, Direction::Fetch, environment, &*callbacks)?;
    let cancel = AtomicBool::new(false);
    let limits = FetchLimits::trusted();
    // Git's lazy fetch asks for `blob:none` whatever the remote's `partialclonefilter` says.
    let mut options = crate::fetch::FetchOptions::default()
        .with_limits(limits)
        .with_filter(Some(ObjectFilter::BlobNone));
    options.explicit_wants = true;
    let select = |_: &crate::fetch::Advertisement| wants.to_vec();
    let transfer = |error| TransferError::Fetch(Box::new(FetchWorkflowError::Transfer(error)));
    let received = match endpoint {
        Endpoint::Local(path) => crate::fetch::receive_local_observed(
            path,
            select,
            &KnownHistory::default(),
            None,
            true,
            limits,
            crate::transport::TransportControl::new(&cancel),
            |message| {
                callbacks.remote_message(message);
                ControlFlow::Continue(())
            },
            |_| {},
            |_| {},
        )
        .map_err(transfer)?,
        Endpoint::Custom(custom) => {
            let mut connection = custom.connect(crate::transport::Service::UploadPack)?;
            let download = crate::fetch::stream::download(
                &mut **connection.get_mut(),
                custom.protocol,
                select,
                None,
                options,
                &cancel,
                |message| callbacks.remote_message(message),
            );
            // A service that fails after a good response says nothing about it, but its failure
            // explains a broken or invalid one.
            let finished = connection.into_inner().finish();
            let validated = download.and_then(|download| {
                download.validate_with_progress(
                    &cancel,
                    |_| ControlFlow::Continue(()),
                    |event| callbacks.validation_progress(event),
                )
            });
            if let (Err(cause), Err(error)) = (&validated, finished)
                && is_stream_failure(cause)
            {
                return Err(error.into());
            }
            validated.map_err(transfer)?
        }
        #[cfg(feature = "http")]
        Endpoint::Http(http) => {
            let runtime = super::runtime()?;
            let control = crate::transport::TransportControl::new(&cancel);
            let receive = |remote: crate::transport::http::HttpRemote,
                           callbacks: &mut dyn TransferCallbacks| {
                runtime.block_on(crate::fetch::receive_http_with_options(
                    &remote,
                    select,
                    None,
                    options.clone(),
                    control,
                    |message| callbacks.remote_message(message),
                ))
            };
            let mut download = receive(http.remote(None)?, callbacks);
            http.settle_proxy(super::fetch_status(&download));
            if matches!(
                &download,
                Err(crate::fetch::FetchError::Http(
                    crate::transport::http::HttpError::Status(401)
                ))
            ) {
                let session = http.credentials(callbacks)?;
                download = receive(http.remote(Some(session))?, callbacks);
            }
            download
                .and_then(|download| {
                    download.validate_with_progress(
                        &cancel,
                        |_| ControlFlow::Continue(()),
                        |event| callbacks.validation_progress(event),
                    )
                })
                .map_err(transfer)?
        }
        #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
        Endpoint::Ssh(ssh) => {
            let callbacks = std::cell::RefCell::new(&mut *callbacks);
            let download = super::runtime()?.block_on(crate::fetch::receive_ssh_with_progress(
                &ssh,
                select,
                None::<std::sync::Arc<KnownHistory>>,
                options,
                crate::transport::TransportControl::new(&cancel),
                |message| callbacks.borrow_mut().transport_message(message),
                |message| callbacks.borrow_mut().remote_message(message),
            ));
            download
                .and_then(|download| {
                    download.validate_with_progress(
                        &cancel,
                        |_| ControlFlow::Continue(()),
                        |event| callbacks.borrow_mut().validation_progress(event),
                    )
                })
                .map_err(transfer)?
        }
    };
    // Like `fetch`, an installation failure can leave the pack, and its `.promisor` marker,
    // published without an index, so it is reported as a finishing failure.
    received
        .install(repository, crate::PackLimits::trusted(), &cancel)
        .map_err(|error| {
            TransferError::FetchFinish(Box::new(crate::fetch::FetchFinishError {
                report: Box::new(crate::fetch::FetchReport {
                    updates: Vec::new(),
                    pack_bytes: received.pack_bytes(),
                    objects: received.object_count(),
                    installed: None,
                    integrity: None,
                    shallow_published: false,
                    references: Vec::new(),
                    missing: Vec::new(),
                }),
                source: Box::new(crate::fetch::FetchFinishFailure::Installation(error)),
            }))
        })?;
    Ok(received.wants().iter().copied().collect())
}
