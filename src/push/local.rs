use std::path::Path;
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
use std::process::Command;
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
/// receive configuration; use [`send_local_with_identity`] when an append needs a caller identity.
/// Use [`crate::remote::Destination::local_path`] for local or `file://` destinations.
///
/// Supports either object format and files or reftable references when prepared by
/// [`PreparedPush::new_local`]. [`PreparedPush::new`] remains SHA-1 wire preparation. Ref and
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
/// # Errors
///
/// Preflight and object installation failures are [`PushError::NotSent`] for ref effects. A
/// reference preparation failure is a per-ref rejection, allowing other refs to succeed. A
/// publication failure or interruption after installation is [`PushError::Uncertain`] with
/// completed status evidence; inspect the destination before retrying.
pub fn send_local(
    destination: impl AsRef<Path>,
    prepared: &PreparedPush,
    cancel: &AtomicBool,
) -> Result<PushReport, PushError> {
    send_local_with_control(destination, prepared, TransportControl::new(cancel))
}

/// Sends a prepared push with caller-controlled transport interruption.
///
/// Uses [`send_local`]'s storage contract. The deadline does not cover push preparation or
/// guarantee interruption during one synchronous filesystem or compression operation.
///
/// # Errors
///
/// Before primary installation, including during pre-receive, interruption is
/// [`PushError::NotSent`]. After installation it is
/// [`PushError::Uncertain`], retaining completed per-ref results; unknown refs may have changed.
/// The cause distinguishes [`super::PushFailure::Cancelled`] and
/// [`super::PushFailure::Deadline`] from rejection.
pub fn send_local_with_control(
    destination: impl AsRef<Path>,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
) -> Result<PushReport, PushError> {
    let inputs = ConfigInputs::default();
    send_local_with_context(
        destination,
        prepared,
        control,
        LocalPushContext {
            config_inputs: &inputs,
            identity: None,
        },
    )
}

/// Explicit inherited configuration and optional reflog identity for one native local push.
///
/// The destination and its linked worktrees add their own local/worktree configuration to these
/// inputs. No ambient environment is read. The caller must supply the same effective sources and
/// runtime pairs that its Git push policy would use.
#[non_exhaustive]
pub struct LocalPushContext<'a> {
    /// System, global, and runtime configuration sources to inherit.
    pub config_inputs: &'a ConfigInputs,
    /// Identity for reflogs that the receive policy actually appends.
    pub identity: Option<&'a Signature>,
}

impl std::fmt::Debug for LocalPushContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalPushContext").finish_non_exhaustive()
    }
}

impl<'a> LocalPushContext<'a> {
    /// Creates a push context with inherited configuration and no reflog identity.
    pub fn new(config_inputs: &'a ConfigInputs) -> Self {
        Self {
            config_inputs,
            identity: None,
        }
    }

    /// Sets [`Self::config_inputs`].
    #[must_use]
    pub fn with_config_inputs(mut self, config_inputs: &'a ConfigInputs) -> Self {
        self.config_inputs = config_inputs;
        self
    }

    /// Sets [`Self::identity`].
    #[must_use]
    pub fn with_identity(mut self, identity: Option<&'a Signature>) -> Self {
        self.identity = identity;
        self
    }
}

/// Sends a native local push with explicit inherited configuration.
///
/// Uses [`send_local_with_control`]'s partial-outcome contract. The supplied configuration is
/// resolved before any object installation or reference publication. Unsupported receive policy
/// refuses before any hook or object installation.
///
/// # Errors
///
/// Returns [`send_local_with_control`]'s storage, validation, policy, hook, cancellation and
/// publication errors, plus errors resolving the supplied configuration. Inspect the retained
/// partial outcomes before retrying: objects or references may already have been published.
pub fn send_local_with_context(
    destination: impl AsRef<Path>,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
    context: LocalPushContext<'_>,
) -> Result<PushReport, PushError> {
    send_local_with_progress(destination, prepared, control, context, |_| {})
}

/// Sends a native local push with synchronous pack-validation progress.
///
/// Uses [`send_local_with_context`]'s storage and partial-outcome contract. Snapshots report actual
/// inflated entries and reconstructed deltas through [`crate::fetch::ValidationProgress`]. No pack
/// means no snapshots. Completion describes validation before hooks, installation and ref updates;
/// a subsequent push failure can follow validation completion. Observers must return promptly and
/// can set `control.cancel` to request cooperative cancellation.
///
/// # Errors
///
/// Validation failure is [`PushError::NotSent`] without completion or primary object publication.
/// Later errors retain [`send_local_with_context`]'s partial-outcome contract.
///
/// # Panics
///
/// Observer panics unwind before incoming objects or references are published.
pub fn send_local_with_progress(
    destination: impl AsRef<Path>,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
    context: LocalPushContext<'_>,
    validation: impl FnMut(crate::fetch::ValidationProgress),
) -> Result<PushReport, PushError> {
    send_local_preparing_observed(
        destination.as_ref(),
        prepared.format,
        control,
        context,
        |_| Ok(prepared),
        validation,
    )
}

/// Prepares a native local push from the destination's advertisement, then sends it.
///
/// `prepare` runs after the destination's receive policy is checked and before installation; its
/// preparation should exclude [`super::local_native::LocalAdvertisement::receiver_roots`].
/// Uses [`send_local_with_context`]'s storage and partial-outcome contract. The destination's
/// references are read again after preparation: a relied-on root that is no longer a reference
/// target is [`super::PushFailure::KnowledgeChanged`], before installation.
pub(crate) fn send_local_preparing_observed<P: std::borrow::Borrow<PreparedPush>>(
    destination: &Path,
    format: crate::ObjectFormat,
    control: TransportControl<'_>,
    context: LocalPushContext<'_>,
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

/// Sends a native local push with an explicit reflog identity.
///
/// Git receive policy controls whether each successful update creates, appends, or deletes a
/// reflog. Existing logs are appended even when automatic log creation is disabled. The identity
/// is used only for records actually written; no environment identity is consulted.
///
/// # Errors
///
/// Uses [`send_local_with_control`]'s partial-outcome contract. An invalid identity refuses the
/// push before object installation. A log publication failure retains separate reference and log
/// effects in the uncertain report.
pub fn send_local_with_identity(
    destination: impl AsRef<Path>,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
    identity: &Signature,
) -> Result<PushReport, PushError> {
    let inputs = ConfigInputs::default();
    send_local_with_context(
        destination,
        prepared,
        control,
        LocalPushContext {
            config_inputs: &inputs,
            identity: Some(identity),
        },
    )
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
