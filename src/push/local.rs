use std::path::Path;
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use std::process::Command;
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use std::sync::atomic::AtomicBool;

use super::{PreparedPush, PushError, PushReport};
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use super::{PushFailure, protocol};
use crate::Signature;
use crate::config::ConfigInputs;
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use crate::packet::Wire;
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use crate::transport::Server;
use crate::transport::TransportControl;

/// Prepared native push and receiver configuration/identity policy.
///
/// A native local push acts as the receiving repository's `git receive-pack` would. That
/// includes its configuration, such as `receive.denyCurrentBranch`, and reflog entries for the
/// references it updates, written with the identity supplied here.
///
/// # Examples
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::PackLimits;
/// use girt::push::{LocalPushRequest, PreparedPush, PushCommand, PushLimits, send_local};
/// use girt::refs::{RefName, Target};
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, main, person) = include!("../doctest/remote.rs");
/// # let commit = include!("../doctest/commit.rs");
/// # girt::workflow::fetch(&repo, "origin", &girt::workflow::FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]), &girt::workflow::Environment::default(), &mut girt::workflow::NoCallbacks)?;
/// let topic = RefName::new("refs/heads/topic")?;
/// let objects = repo.objects(PackLimits::default())?;
/// let cancel = AtomicBool::new(false);
/// let prepared = PreparedPush::new(
///     &objects,
///     vec![PushCommand::new(topic.clone(), None, main)],
///     &[main],
///     PushLimits::default(),
///     &cancel,
/// )?;
///
/// let request = LocalPushRequest::new(&prepared).with_identity(Some(&person));
/// let report = send_local(upstream.git_dir(), request, TransportControl::new(&cancel), |_| {})?;
/// assert!(report.all_succeeded());
/// assert_eq!(upstream.references()?.read(&topic)?, Some(Target::Direct(main)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct LocalPushRequest<'a> {
    prepared: &'a PreparedPush,
    config_inputs: Option<&'a ConfigInputs>,
    identity: Option<&'a Signature>,
}

impl std::fmt::Debug for LocalPushRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalPushRequest").finish_non_exhaustive()
    }
}

impl<'a> LocalPushRequest<'a> {
    /// Sends this artifact with no system, global or runtime configuration and no reflog identity.
    ///
    /// Only the destination's local and worktree configuration applies unless inherited inputs
    /// are supplied with [`Self::with_config_inputs`].
    pub fn new(prepared: &'a PreparedPush) -> Self {
        Self {
            prepared,
            config_inputs: None,
            identity: None,
        }
    }

    /// Selects inherited receiver configuration without changing reflog identity.
    ///
    /// Supply the same effective sources and runtime pairs that the caller's Git push policy
    /// would use. The destination adds its local and worktree configuration to these inputs.
    #[must_use]
    pub fn with_config_inputs(mut self, config_inputs: &'a ConfigInputs) -> Self {
        self.config_inputs = Some(config_inputs);
        self
    }

    /// Supplies or clears the identity used for reflog records actually written.
    ///
    /// Existing logs are appended even if automatic log creation is disabled; creation follows
    /// receiver configuration. An invalid identity refuses the push before installation.
    #[must_use]
    pub fn with_identity(mut self, identity: Option<&'a Signature>) -> Self {
        self.identity = identity;
        self
    }
}

/// Installs a prepared pack and conditionally updates a trusted local repository with girt.
///
/// Opens an explicit path through [`crate::Repository::open`], checks receiver-history
/// dependencies, installs the pack, then applies each command with a conditional reference
/// transaction against its exact expected old value. A stale command rejects independently.
/// Girt starts hooks directly, without a Git transport executable. It runs the destination's
/// `pre-receive`, `update`, `post-receive` and `post-update` hooks (honoring `core.hooksPath`) and
/// returns their output in [`PushReport::progress`]; `proc-receive`, `reference-transaction` and
/// `push-to-checkout` hooks cause refusal before publication. Current-branch, hidden-ref and
/// reflog behavior follows supported
/// receive configuration; use [`LocalPushRequest::with_identity`] when append logging needs an
/// identity; without one, required append logging refuses the push before installation.
/// Use [`crate::remote::Destination::local_path`] for local or `file://` destinations.
///
/// Supports either object format and files or reftable references when prepared by
/// [`PreparedPush::new`]. Ref and
/// worktree state may change between separate reads; conditional locks prevent stale ref
/// overwrites. Callers must exclude external GC/pruning, checkout, symbolic-HEAD/branch changes and
/// worktree registration changes from preparation through publication. Ref locks do not enforce
/// this exclusion. Deletion and non-fast-forward receive restrictions apply. `updateInstead`,
/// namespace-specific policy and unsupported receive settings refuse before mutation.
/// Object installation can leave an indexed pack when later refs reject or fail.
///
/// On macOS/Linux, incoming objects are private during `pre-receive`, with primary objects
/// exposed as alternates. Rejection or cancellation there discards incoming objects without
/// installing them in primary storage. After acceptance, the original validated pack is installed
/// and quarantine is removed before `update`. This writes the pack twice when pre-receive is
/// configured. Update rejection can leave published objects.
/// Hooks that add or change quarantined object storage are explicitly unsupported and refuse
/// before primary publication. The private inventory is bounded to 16 files/directories; the
/// supported single-pack layout is pinned by tests. Inherited object-directory/alternate
/// environment variables are cleared; only primary alternates are exposed to pre-receive.
/// A replacement quarantine directory is preserved, with structured
/// cleanup failure and its path. The caller must exclude concurrent object-directory/layout
/// replacement; path checks do not provide protection against hostile directory races.
///
/// Hook stdin/stdout/stderr and exit waits observe cancellation/deadlines. Combined retained hook
/// output shares [`super::PushLimits::max_status_bytes`]; excess output is discarded while pipes
/// continue draining. Successful hooks preserve background work and wait for pipe EOF. On error,
/// cancellation or deadline, one owner kills the hook's process group and reaps its leader.
/// Descendants that escape that group are outside this containment. Hooks are trusted programs, can
/// mutate repository data themselves, and are not an execution sandbox. Hook execution on other
/// platforms refuses before mutation; native transfers without hooks retain their existing platform
/// support.
///
/// The deadline excludes preparation and cannot interrupt one synchronous filesystem or
/// compression operation. Receiver configuration adds its local/worktree sources to the inputs
/// supplied by [`LocalPushRequest::with_config_inputs`]; no ambient environment is read.
///
/// Validation observers run synchronously before hooks, installation and publication. A pack-free
/// push has no snapshots; completion marks validation only and later steps may still fail.
/// Observers must return promptly and can set `control.cancel` for cooperative cancellation.
/// Validation failure is [`PushError::NotSent`] without a completion notification.
///
/// # Errors
///
/// Preflight, inherited/local configuration resolution, invalid identity, and installation failures
/// are [`PushError::NotSent`] for reference effects. Reference preparation failures reject one
/// reference while others may succeed. Publication failure or interruption after installation is
/// [`PushError::Uncertain`] with completed status evidence; inspect the destination before
/// retrying. The cause distinguishes [`super::PushFailure::Cancelled`] and
/// [`super::PushFailure::Deadline`] from receiver rejection.
///
/// # Panics
///
/// Observer panics unwind before incoming objects or references are published.
pub fn send_local(
    destination: impl AsRef<Path>,
    request: LocalPushRequest<'_>,
    control: TransportControl<'_>,
    validation: impl FnMut(crate::fetch::ValidationProgress),
) -> Result<PushReport, PushError> {
    let inputs = ConfigInputs::default();
    let policy = ReceiverPolicy {
        config_inputs: request.config_inputs.unwrap_or(&inputs),
        identity: request.identity,
    };
    send_local_preparing_observed(
        destination.as_ref(),
        request.prepared.format,
        control,
        policy,
        |_| Ok(request.prepared),
        validation,
    )
}

/// Explicit inherited configuration and optional reflog identity for one native local push.
///
/// The destination and its linked worktrees add their own local/worktree configuration to these
/// inputs. No ambient environment is read. The caller must supply the same effective sources and
/// runtime pairs that its Git push policy would use.
#[non_exhaustive]
pub(crate) struct ReceiverPolicy<'a> {
    /// System, global, and runtime configuration sources to inherit.
    pub config_inputs: &'a ConfigInputs,
    /// Identity for reflogs that the receive policy actually appends.
    pub identity: Option<&'a Signature>,
}

impl std::fmt::Debug for ReceiverPolicy<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceiverPolicy").finish_non_exhaustive()
    }
}

/// Prepares a native local push from the destination's advertisement, then sends it.
///
/// `prepare` runs after the destination's receive policy is checked and before installation; its
/// preparation should exclude [`super::local_native::LocalAdvertisement::receiver_roots`].
/// Uses [`send_local`]'s storage and partial-outcome contract. The destination's
/// references are read again after preparation: a relied-on root that is no longer a reference
/// target is [`super::PushFailure::KnowledgeChanged`], before installation.
pub(crate) fn send_local_preparing_observed<P: std::borrow::Borrow<PreparedPush>>(
    destination: &Path,
    format: crate::ObjectFormat,
    control: TransportControl<'_>,
    context: ReceiverPolicy<'_>,
    prepare: impl FnOnce(&super::local_native::LocalAdvertisement<'_>) -> Result<P, super::PushFailure>,
    validation: impl FnMut(crate::fetch::ValidationProgress),
) -> Result<PushReport, PushError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "push.local",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
        accepted = tracing::field::Empty,
        rejected = tracing::field::Empty,
        pending = tracing::field::Empty,
        unpack = tracing::field::Empty,
    );

    let operation = || {
        super::local_native::send_preparing(
            destination,
            format,
            control,
            context.identity,
            context.config_inputs,
            prepare,
            validation,
        )
    };
    #[cfg(feature = "tracing")]
    let result = span.in_scope(operation);
    #[cfg(not(feature = "tracing"))]
    let result = { operation }();
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, |error| crate::trace::push(error, &span));
    #[cfg(feature = "tracing")]
    if let Ok(report) = &result {
        crate::trace::push_report(&span, report);
    }

    result
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
fn send_server(
    command: &mut Command,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
) -> Result<PushReport, PushError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(
        target: "girt",
        "push.local_server",
        outcome = "incomplete",
        failure_class = tracing::field::Empty,
        effects = tracing::field::Empty,
        accepted = tracing::field::Empty,
        rejected = tracing::field::Empty,
        pending = tracing::field::Empty,
        unpack = tracing::field::Empty,
    );

    let operation = || {
        let mut child =
            Server::spawn(command, control).map_err(|e| PushError::NotSent(e.into()))?;
        let (body, exchange, written, caps, request) = {
            let (mut reader, writer) = child.streams();
            let mut wire = Wire {
                reader: &mut reader,
                remaining: prepared.limits.max_advertisement_bytes,
                cancel: control.cancel,
            };
            let caps = protocol::advertise(&mut wire, prepared).map_err(PushError::NotSent)?;
            control.check().map_err(|e| PushError::NotSent(e.into()))?;
            let request = prepared
                .request_for(caps.report_v2, caps.sideband)
                .map_err(PushError::NotSent)?;
            let (body, exchange, written) = Server::exchange(
                reader,
                writer,
                &request,
                &prepared.pack.to_vec(),
                prepared.limits.max_status_bytes,
            );
            (body, exchange, written, caps, request)
        };
        if written == 0
            && let Err(cause) = exchange
        {
            return Err(PushError::NotSent(cause.into()));
        }
        let mut report = PushReport::pending(&prepared.commands);
        report.mark_attempted(&request, written);
        // Parse retained bytes even after cancellation; no further I/O or mutation occurs here.
        let retain = AtomicBool::new(false);
        let mut reader = body.as_slice();
        let mut wire = Wire {
            reader: &mut reader,
            remaining: prepared.limits.max_status_bytes,
            cancel: &retain,
        };
        let parsed = if prepared.commands.is_empty() {
            wire.end().map_err(PushFailure::from)
        } else {
            protocol::read_response(
                &mut wire,
                &mut report,
                caps,
                prepared.limits.max_status_bytes,
            )
            .and_then(|()| wire.end().map_err(PushFailure::from))
        };
        let transfer = exchange.map_err(PushFailure::from).and(parsed);
        if let Err(cause) = transfer {
            return if prepared.commands.is_empty() {
                Err(PushError::NotSent(cause))
            } else {
                Err(PushError::Uncertain {
                    cause,
                    report: Box::new(report),
                })
            };
        }
        let result = child.wait().map_err(PushFailure::from).and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(PushFailure::Process(status))
            }
        });
        match result {
            Ok(()) => Ok(report),
            Err(cause) if prepared.commands.is_empty() => Err(PushError::NotSent(cause)),
            Err(cause) => Err(PushError::Uncertain {
                cause,
                report: Box::new(report),
            }),
        }
    };
    #[cfg(feature = "tracing")]
    let result = span.in_scope(operation);
    #[cfg(not(feature = "tracing"))]
    let result = { operation }();
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, |error| crate::trace::push(error, &span));
    #[cfg(feature = "tracing")]
    if let Ok(report) = &result {
        crate::trace::push_report(&span, report);
    }

    result
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests;
