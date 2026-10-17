use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::*;
use crate::InitKind;
use crate::clone::{BranchSelection, CloneHead, CloneReport, CloneRequest};
use crate::config::{ConfigInputs, Document};
use crate::remote::RemoteConfig;

/// Policy for [`clone_with`]. Defaults create a working tree and check out the remote's HEAD.
///
/// Resource limits use bounded defaults. Bare clones retain the core tracking layout (only the
/// selected branch is local), and skip checkout. See [`CloneRequest::prepare_tracking`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CloneOptions {
    /// Repository layout to create.
    pub kind: InitKind,

    /// Materialize the selected commit when creating a working tree.
    pub checkout: bool,

    /// Default branch or an explicitly selected full branch name.
    pub selection: BranchSelection,

    /// Download and validation bounds.
    pub fetch_limits: FetchLimits,

    /// Installation and reference publication bounds.
    pub update_limits: FetchUpdateLimits,

    /// Working tree and index bounds.
    pub checkout_limits: crate::checkout::Limits,
}

impl Default for CloneOptions {
    fn default() -> Self {
        Self {
            kind: InitKind::Worktree,
            checkout: true,
            selection: BranchSelection::Default,
            fetch_limits: FetchLimits::default(),
            update_limits: FetchUpdateLimits::default(),
            checkout_limits: crate::checkout::Limits::default(),
        }
    }
}

impl CloneOptions {
    /// Sets repository layout.
    #[must_use]
    pub fn with_kind(mut self, kind: InitKind) -> Self {
        self.kind = kind;
        self
    }

    /// Sets whether to materialize the selected commit.
    #[must_use]
    pub fn with_checkout(mut self, checkout: bool) -> Self {
        self.checkout = checkout;
        self
    }

    /// Sets branch selection.
    #[must_use]
    pub fn with_selection(mut self, selection: BranchSelection) -> Self {
        self.selection = selection;
        self
    }

    /// Sets download and validation bounds.
    #[must_use]
    pub fn with_fetch_limits(mut self, fetch_limits: FetchLimits) -> Self {
        self.fetch_limits = fetch_limits;
        self
    }

    /// Sets publication bounds.
    #[must_use]
    pub fn with_update_limits(mut self, update_limits: FetchUpdateLimits) -> Self {
        self.update_limits = update_limits;
        self
    }

    /// Sets checkout bounds.
    #[must_use]
    pub fn with_checkout_limits(mut self, checkout_limits: crate::checkout::Limits) -> Self {
        self.checkout_limits = checkout_limits;
        self
    }
}

/// A completed clone and its optional checkout. HEAD is published before checkout starts.
#[derive(Debug)]
#[non_exhaustive]
pub struct CloneOutcome {
    /// Core installation, configuration and HEAD publication report, including the repository.
    pub clone: CloneReport,

    /// Completed checkout; absent for bare, no-checkout or unborn remote HEAD.
    pub checkout: Option<crate::checkout::Report>,
}

/// Clone failure, preserving core publication and checkout effects for recovery.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CloneError {
    /// Checkout was requested on a platform outside Linux/macOS; nothing was created.
    #[error("clone checkout requires Linux/macOS; select no-checkout or bare mode")]
    UnsupportedCheckout,
    /// Git-default configuration inputs could not be selected; nothing was created.
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    /// Configuration could not be resolved; nothing was created.
    #[error(transparent)]
    Resolve(#[from] Box<crate::config::ResolveError>),
    /// Configuration, path or endpoint resolution failed before destination creation.
    #[error(transparent)]
    Transfer(#[from] TransferError),
    /// Invalid destination or core clone policy; nothing was created.
    #[error(transparent)]
    Prepare(#[from] crate::clone::CloneFailure),
    /// Download or validation failed; the destination is absent.
    #[error(transparent)]
    Receive(#[from] crate::clone::CloneTransferError),
    /// Core installation failed; inspect its report before removing residual files.
    #[error(transparent)]
    Finish(#[from] Box<crate::clone::CloneError>),
    /// The clone completed, but opening it or reading its selected commit failed.
    #[error("clone completed but reopening or reading the selected commit failed")]
    Read {
        /// Completed clone, with HEAD and tracking configuration already published.
        report: Box<CloneReport>,

        /// Read failure.
        #[source]
        source: crate::Error,
    },
    /// The clone completed; checkout may have changed files or published the index.
    #[error("clone completed but checkout failed; inspect the checkout report before recovery")]
    Checkout {
        /// Completed clone. HEAD remains on the selected branch/commit.
        report: Box<CloneReport>,

        /// Checkout effects and failure stage. No automatic rollback or deletion occurs.
        #[source]
        source: Box<crate::checkout::Failure>,
    },
}

/// Clones a URL into a new working tree and checks out its default branch.
///
/// Uses [`clone_with`] with default options, Git-default configuration, the process environment
/// and [`NoCallbacks`]. Local relative sources and destinations resolve against the current
/// directory. Local path arguments become absolute without resolving symlinks; file URLs and
/// rewritten aliases retain their spelling for subsequent fetches. URL rewrites, protocol policy,
/// credentials and proxies follow [`fetch`].
///
/// # Errors
///
/// See [`clone_with`] for exclusive ownership, partial effects and recovery requirements.
///
/// # Examples
///
/// ```
/// # if !cfg!(any(target_os = "linux", target_os = "macos")) { return Ok(()); }
/// # let (directory, upstream, _, _, _) = include!("../doctest/remote.rs");
/// let destination = directory.path().join("copy");
/// let outcome = girt::workflow::clone(upstream.git_dir().to_str().unwrap(), &destination)?;
/// assert!(outcome.checkout.unwrap().index_published);
/// assert!(outcome.clone.repository.is_some());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn clone(url: &str, destination: impl AsRef<Path>) -> Result<CloneOutcome, CloneError> {
    let environment = Environment::from_process();
    let inputs = ConfigInputs::git_default(&environment)?;
    clone_with(
        url,
        destination,
        &CloneOptions::default(),
        &inputs,
        &environment,
        &mut NoCallbacks,
    )
}

/// Clones with explicit configuration inputs, environment, callbacks and resource policy.
///
/// Composes [`CloneRequest`], [`RemoteConfig`] and [`Repository::checkout_tree_with`].
/// Configuration is resolved before transfer; no repository is discovered in the current directory.
/// The supplied URL is used even when inherited configuration defines `origin`. Non-local URLs are
/// stored as supplied (use a credential-free URL). Unrewritten relative local path arguments
/// become absolute with [`crate::remote::ParsedUrl::canonicalize_local`], without resolving
/// symlinks. Absolute paths retain their spelling, including native Windows verbatim prefixes.
///
/// The destination must be absent, including empty directories and dangling symlinks, and its
/// parent must exist. The caller must exclusively own the destination path, ancestors, metadata
/// and working tree throughout the call, excluding replacement, other writers and GC. This is
/// not an untrusted-path security boundary. Finish reserves the root exclusively. Objects, refs,
/// origin and branch tracking config, then HEAD are published by the core clone; checkout then
/// writes files and publishes the index. These phases are not one atomic transaction.
///
/// Checkout inherits [`crate::checkout`]'s Linux/macOS scope, literal bytes, native modes and
/// refusals. Other platforms refuse before transfer when checkout is requested; use
/// [`CloneOptions::with_checkout`] with `false`, or bare mode. Unborn or unavailable HEAD skips
/// checkout; a detached remote HEAD stays detached. A symbolic HEAD naming a missing branch in a
/// nonempty remote is refused by the core planner. No origin/HEAD alias, shallow clone, filters,
/// submodules, hardlinks or automatic rollback is provided. HTTP/SSH runtime constraints match
/// [`fetch`]. Like fetch, this synchronous workflow has no cancellation or deadline; use the core
/// APIs for caller-controlled cancellation.
///
/// # Errors
///
/// Resolution/download errors leave no destination. Finish errors carry the core partial report.
/// Checkout errors preserve the completed clone and checkout failure report: HEAD may already be
/// published and files may be partially written. Inspect those reports and disk state under
/// exclusive ownership before retrying checkout or deleting a disposable destination. Never delete
/// a path merely because preparation reported a collision.
pub fn clone_with(
    url: &str,
    destination: impl AsRef<Path>,
    options: &CloneOptions,
    inputs: &ConfigInputs,
    environment: &Environment,
    callbacks: &mut dyn TransferCallbacks,
) -> Result<CloneOutcome, CloneError> {
    if options.checkout
        && matches!(options.kind, InitKind::Worktree)
        && !cfg!(any(target_os = "linux", target_os = "macos"))
    {
        return Err(CloneError::UnsupportedCheckout);
    }
    let base = match environment.current_dir() {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(TransferError::local)?,
    };
    let destination = base.join(destination);
    let config = crate::Config::resolve(inputs).map_err(Box::new)?;
    let mut document = Document::parse(b"").map_err(TransferError::local)?;
    RemoteConfig::new(&mut document)
        .add(b"origin", url.as_bytes(), &[])
        .map_err(TransferError::local)?;
    let remote = Remote::find(document.config(), b"origin")
        .map_err(TransferError::local)?
        .ok_or_else(|| TransferError::NoSuchRemote("origin".to_owned()))?;
    let (endpoint, resolved) = configured_endpoint(
        &config,
        &remote,
        "origin",
        Direction::Fetch,
        &base,
        environment,
    )?;
    let stored_url = if resolved.bytes() != url.as_bytes() || Path::new(url).is_absolute() {
        url.as_bytes().to_vec()
    } else {
        crate::remote::ParsedUrl::parse(url.as_bytes())
            .and_then(|parsed| parsed.canonicalize_local(&base))
            .map_err(TransferError::local)?
    };
    let request = || {
        CloneRequest::prepare_tracking(
            &destination,
            options.kind,
            &stored_url,
            options.selection.clone(),
            Reflog::Preserve,
        )
    };
    // Check collisions before connecting, including in the HTTP retry path.
    let prepared = request()?;
    let cancel = AtomicBool::new(false);
    let control = TransportControl::new(&cancel);
    let ready = match endpoint {
        Endpoint::Local(path) => {
            let callbacks = std::cell::RefCell::new(&mut *callbacks);
            prepared.receive_local_with_progress(
                path,
                options.fetch_limits,
                control,
                crate::fetch::LocalFetchObserver::default()
                    .with_messages(&mut |message| {
                        callbacks.borrow_mut().remote_message(message);
                        ControlFlow::Continue(())
                    })
                    .with_construction(&mut |event| callbacks.borrow_mut().fetch_progress(event))
                    .with_validation(&mut |event| {
                        callbacks.borrow_mut().validation_progress(event)
                    }),
            )
        }
        #[cfg(feature = "http")]
        Endpoint::Http(http) => {
            let runtime = runtime()?;
            let mut download = runtime.block_on(prepared.receive_http_with_progress(
                &http.remote(None)?,
                options.fetch_limits,
                control,
                |message| callbacks.remote_message(message),
            ));
            if matches!(
                &download,
                Err(crate::clone::CloneTransferError::Transfer(
                    crate::fetch::FetchError::Http(crate::transport::http::HttpError::Status(401))
                ))
            ) {
                let session = http.credentials(callbacks)?;
                download = runtime.block_on(request()?.receive_http_with_progress(
                    &http.remote(Some(session))?,
                    options.fetch_limits,
                    control,
                    |message| callbacks.remote_message(message),
                ));
            }
            download.and_then(|download| {
                download.validate_with_progress(
                    &cancel,
                    |_| ControlFlow::Continue(()),
                    |event| callbacks.validation_progress(event),
                )
            })
        }
        #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
        Endpoint::Ssh(ssh) => {
            let callbacks = std::cell::RefCell::new(&mut *callbacks);
            runtime()?
                .block_on(prepared.receive_ssh_with_progress(
                    &ssh,
                    options.fetch_limits,
                    control,
                    |message| callbacks.borrow_mut().transport_message(message),
                    |message| callbacks.borrow_mut().remote_message(message),
                ))
                .and_then(|download| {
                    download.validate_with_progress(
                        &cancel,
                        |_| ControlFlow::Continue(()),
                        |event| callbacks.borrow_mut().validation_progress(event),
                    )
                })
        }
    }?;
    let mut report = ready
        .finish(options.update_limits, &cancel)
        .map_err(|error| CloneError::Finish(Box::new(error)))?;
    match Repository::open_with_config(&report.destination, inputs) {
        Ok(repository) => report.repository = Some(repository),
        Err(source) => {
            return Err(CloneError::Read {
                report: Box::new(report),
                source: source.into(),
            });
        }
    }
    let checkout = checkout(&report, options, &cancel, callbacks);
    match checkout {
        Ok(checkout) => Ok(CloneOutcome {
            clone: report,
            checkout,
        }),
        Err(CheckoutError::Read(source)) => Err(CloneError::Read {
            report: Box::new(report),
            source,
        }),
        Err(CheckoutError::Checkout(source)) => Err(CloneError::Checkout {
            report: Box::new(report),
            source: Box::new(source),
        }),
    }
}

enum CheckoutError {
    Read(crate::Error),
    Checkout(crate::checkout::Failure),
}

fn checkout(
    report: &CloneReport,
    options: &CloneOptions,
    cancel: &AtomicBool,
    callbacks: &mut dyn TransferCallbacks,
) -> Result<Option<crate::checkout::Report>, CheckoutError> {
    if !options.checkout || matches!(options.kind, InitKind::Bare) {
        return Ok(None);
    }
    let id = match report.head {
        CloneHead::Branch { id, .. } | CloneHead::Detached(id) => id,
        _ => return Ok(None),
    };
    let tree = (|| -> crate::Result<_> {
        let repository = report
            .repository
            .as_ref()
            .ok_or_else(|| std::io::Error::other("completed clone has no repository"))?;
        let tree = repository.objects()?.read_commit(id)?.tree();
        Ok((repository, tree))
    })()
    .map_err(CheckoutError::Read)?;
    tree.0
        .checkout_tree_with_progress(
            None,
            Some(tree.1),
            options.checkout_limits,
            cancel,
            |done, total| callbacks.checkout_progress(done, total),
        )
        .map(Some)
        .map_err(CheckoutError::Checkout)
}
