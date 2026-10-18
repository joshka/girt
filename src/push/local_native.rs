//! Native local pack installation followed by conditional reference publication.

use std::borrow::Borrow;
use std::fs;
use std::path::Path;

use super::{PreparedPush, PushError, PushFailure, PushReport, RejectionOrigin, Status};
use crate::config::ConfigInputs;
use crate::fetch::{Advertisement, FetchError, FetchLimits, KnownHistory, ReceivedFetch};
use crate::hide_refs::HideRules;
use crate::refs::{
    Expected, RefEdit, RefName, RefOutcome, ReferenceError, References, Reflog, Target,
    TransactionError,
};
use crate::transport::TransportControl;
use crate::{Repository, Signature};

/// The destination's references as `git receive-pack` would advertise them, read on request.
pub(crate) struct LocalAdvertisement<'a> {
    refs: &'a References<'a>,
    hide: &'a HideRules,
}

impl LocalAdvertisement<'_> {
    /// The direct target of `name` as receive-pack would advertise it; hidden and symbolic
    /// references read as absent.
    pub(crate) fn target(
        &self,
        name: &crate::refs::RefName,
    ) -> Result<Option<crate::ObjectId>, PushFailure> {
        if self.hide.matches(name) {
            return Ok(None);
        }
        Ok(
            match self.refs.read(name).map_err(PushFailure::reference)? {
                Some(Target::Direct(id)) => Some(id),
                Some(Target::Symbolic(_)) | None => None,
            },
        )
    }

    /// Targets of the visible direct references, sorted and deduplicated. Like receive-pack, refs
    /// hidden by `receive.hideRefs` or `transfer.hideRefs` are not offered.
    pub(crate) fn receiver_roots(&self) -> Result<Vec<crate::ObjectId>, PushFailure> {
        let mut roots: Vec<_> = self
            .refs
            .list()
            .map_err(PushFailure::reference)?
            .into_iter()
            .filter(|reference| !self.hide.matches(&reference.name))
            .filter_map(|reference| match reference.target {
                Target::Direct(id) => Some(id),
                Target::Symbolic(_) => None,
            })
            .collect();
        roots.sort_unstable();
        roots.dedup();
        Ok(roots)
    }
}

/// Opens and checks the destination's receive policy, prepares the push from its advertisement,
/// then installs and publishes it.
///
/// Policy refusals happen before `prepare` runs. Receiver roots the preparation relied on are
/// checked against the references read after preparation; a change is
/// [`PushFailure::KnowledgeChanged`].
pub(super) fn send_preparing<P: Borrow<PreparedPush>>(
    destination: &Path,
    format: crate::ObjectFormat,
    control: TransportControl<'_>,
    identity: Option<&Signature>,
    config_inputs: &ConfigInputs,
    prepare: impl FnOnce(&LocalAdvertisement<'_>) -> Result<P, PushFailure>,
    mut validation: impl FnMut(crate::fetch::ValidationProgress),
) -> Result<PushReport, PushError> {
    control
        .check()
        .map_err(|error| PushError::NotSent(error.into()))?;
    let repository = Repository::open_with_config(destination, config_inputs)
        .map_err(|error| PushError::NotSent(PushFailure::destination(error)))?;
    if repository.object_format() != format {
        return Err(PushError::NotSent(PushFailure::Unsupported(
            "destination object format",
        )));
    }
    let advertises_push_options =
        configured_boolean(&repository, "receive", "advertisepushoptions")?.unwrap_or(false);
    if identity.is_some_and(|person| person.validate().is_err() || person.seconds < 0) {
        return Err(PushError::NotSent(PushFailure::Identity));
    }
    let current_policy = current_policy(&repository)?;
    let deny_delete_current = delete_current_policy(&repository)?;
    let deny_deletes = configured_boolean(&repository, "receive", "denydeletes")?.unwrap_or(false);
    let deny_non_fast_forwards =
        configured_boolean(&repository, "receive", "denynonfastforwards")?.unwrap_or(false);
    let log_policy = log_policy(&repository)?;
    let hide = HideRules::parse(&repository, "receive")
        .map_err(|reason| PushError::NotSent(PushFailure::Unsupported(reason)))?;
    if repository
        .config()
        .value("receive", None, "procreceiverefs")
        .is_some()
    {
        return Err(PushError::NotSent(PushFailure::Unsupported(
            "configured receive policy",
        )));
    }
    if !repository.shallow_roots().is_empty() {
        return Err(PushError::NotSent(PushFailure::Unsupported(
            "shallow destination",
        )));
    }
    let refs = repository
        .references()
        .map_err(|error| PushError::NotSent(PushFailure::reference(error)))?;
    let prepared = prepare(&LocalAdvertisement {
        refs: &refs,
        hide: &hide,
    })
    .map_err(PushError::NotSent)?;
    let prepared: &PreparedPush = prepared.borrow();
    if prepared.format != format {
        return Err(PushError::NotSent(PushFailure::Unsupported(
            "destination object format",
        )));
    }
    if prepared.has_options && !advertises_push_options {
        return Err(PushError::NotSent(PushFailure::Unsupported(
            "the receiving end does not support push options",
        )));
    }
    control
        .check()
        .map_err(|error| PushError::NotSent(error.into()))?;
    let mut report = PushReport::pending(&prepared.commands);
    for reference in &mut report.refs {
        reference.rewrite_complete = true;
    }
    if prepared.commands.is_empty() {
        return Ok(report);
    }
    let hooks = super::hooks::ReceiveHooks::new(
        &repository,
        prepared.options.clone(),
        prepared.limits.max_status_bytes,
    )
    .map_err(PushError::NotSent)?;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    if ["pre-receive", "update", "post-receive", "post-update"]
        .iter()
        .any(|name| hooks.has(name))
    {
        return Err(PushError::NotSent(PushFailure::Unsupported(
            "receive hook execution on this platform",
        )));
    }
    for hook in ["proc-receive", "reference-transaction", "push-to-checkout"] {
        if hooks.has(hook) {
            return Err(PushError::NotSent(PushFailure::Unsupported(
                "proc-receive, reference-transaction or push-to-checkout hook",
            )));
        }
    }
    let current = refs
        .list()
        .map_err(|error| PushError::NotSent(PushFailure::reference(error)))?;
    for command in &prepared.commands {
        if matches!(
            refs.read(&command.name)
                .map_err(|error| PushError::NotSent(PushFailure::reference(error)))?,
            Some(Target::Symbolic(_))
        ) {
            return Err(PushError::NotSent(PushFailure::Unsupported(
                "symbolic push destination",
            )));
        }
        let checked_out = is_checked_out(&repository, &command.name, config_inputs)
            .map_err(PushError::NotSent)?;
        let rejected_current = checked_out
            && (command.deletes() && deny_delete_current
                || !command.deletes() && current_policy == CurrentPolicy::Refuse);
        if !rejected_current && !hide.matches(&command.name) {
            reflog_policy(&refs, command, log_policy, identity).map_err(PushError::NotSent)?;
        }
    }
    for root in &prepared.receiver_roots {
        if !current
            .iter()
            .any(|reference| reference.target == Target::Direct(*root))
        {
            return Err(PushError::NotSent(PushFailure::KnowledgeChanged(*root)));
        }
    }
    // A live reader lets each later open of an unchanged destination pack reuse its validated
    // index instead of parsing it again. Every open still takes its own snapshot.
    let mut index_pin = None;
    if !prepared.receiver_roots.is_empty() {
        // Like Git, the destination's existing objects are trusted to have complete history, so
        // the pack may omit anything reachable from these roots.
        let store = repository
            .objects_with(prepared.limits.destination_packs)
            .map_err(|error| {
                PushError::NotSent(PushFailure::install(FetchError::Destination(error)))
            })?;
        for &root in &prepared.receiver_roots {
            let present = store
                .read_with(root, prepared.limits.read)
                .map_err(|error| {
                    PushError::NotSent(PushFailure::install(FetchError::Destination(error)))
                })?
                .is_some();
            if !present {
                return Err(PushError::NotSent(PushFailure::KnowledgeChanged(root)));
            }
        }
        index_pin = Some(store);
    }
    let limits = FetchLimits {
        max_pack_bytes: prepared
            .limits
            .pack
            .max_pack_bytes
            .try_into()
            .unwrap_or(usize::MAX),
        max_objects: prepared.limits.pack.max_objects as usize,
        max_object_bytes: prepared
            .limits
            .pack
            .max_object_bytes
            .try_into()
            .unwrap_or(usize::MAX),
        max_decode_bytes: prepared
            .limits
            .pack
            .max_input_bytes
            .try_into()
            .unwrap_or(usize::MAX),
        max_delta_depth: prepared.limits.read.max_delta_depth,
        max_delta_bytes: prepared.limits.read.max_delta_bytes,
        ..FetchLimits::trusted()
    };
    let (received, unstored_empty_tree) = if !prepared.pack.is_empty() {
        let store = repository
            .objects_with(prepared.limits.destination_packs)
            .map_err(|error| {
                PushError::NotSent(PushFailure::install(FetchError::Destination(error)))
            })?;
        index_pin.get_or_insert_with(|| store.clone());
        let roots: Vec<_> = current
            .iter()
            .filter_map(|reference| match reference.target {
                Target::Direct(id) => Some(id),
                Target::Symbolic(_) => None,
            })
            .collect();
        let mut known = KnownHistory::from_store_roots(store, &roots, control.cancel)
            .map_err(|error| PushError::NotSent(PushFailure::install(error)))?;
        // Git treats the canonical empty tree as present in every repository, so the sender omits
        // it when the receiver's history reaches it, even if the receiver doesn't store it.
        let unstored_empty_tree = known.assume_empty_tree();
        let mut pack = crate::fetch::Bytes::temporary()
            .map_err(|error| PushError::NotSent(PushFailure::install(error)))?;
        // Keep the prepared artifact reusable; import owns this independent temporary copy.
        let mut reader = prepared
            .pack
            .reader()
            .map_err(|error| PushError::NotSent(PushFailure::install(error)))?;
        let mut buffer = [0; 64 * 1024];
        loop {
            control
                .check()
                .map_err(|error| PushError::NotSent(error.into()))?;
            let count = std::io::Read::read(&mut reader, &mut buffer)
                .map_err(|e| PushError::NotSent(e.into()))?;
            if count == 0 {
                break;
            }
            pack.append(&buffer[..count])
                .map_err(|error| PushError::NotSent(PushFailure::install(error)))?;
        }
        let received = ReceivedFetch::validate_local_push(
            Advertisement {
                refs: Vec::new(),
                capabilities: vec![format!("object-format={}", prepared.format).into_bytes()],
            },
            prepared
                .commands
                .iter()
                .filter(|command| !command.deletes())
                .map(|command| command.new)
                .collect(),
            pack,
            known,
            vec![],
            crate::fetch::FetchOptions {
                limits,
                depth: None,
                filter: None,
            },
            &mut crate::fetch::ValidationObserver::new(control.cancel, &mut validation),
        )
        .map_err(|error| PushError::NotSent(PushFailure::install(error)))?;
        let unstored_empty_tree =
            unstored_empty_tree.filter(|id| received.dependencies().contains(id));
        (Some(received), unstored_empty_tree)
    } else {
        (None, None)
    };
    if let Some(received) = &received {
        report.integrity = received
            .enforce_fsck(
                repository.config(),
                crate::fsck::Scope::Receive,
                repository.worktree().unwrap_or(repository.git_dir()),
                control.cancel,
            )
            .map_err(|error| PushError::NotSent(PushFailure::Integrity(Box::new(error))))?;
    }
    let mut quarantine = received
        .as_ref()
        .filter(|_| hooks.has("pre-receive"))
        .map(|_| super::quarantine::Quarantine::new(&repository))
        .transpose()
        .map_err(|error| PushError::NotSent(error.into()))?;
    let pending = report.clone();
    let result = (|| {
        if let (Some(received), Some(quarantine)) = (&received, &quarantine) {
            if unstored_empty_tree.is_some() {
                quarantine
                    .repository
                    .loose_objects()
                    .write_object(prepared.format, crate::ObjectKind::Tree, b"")
                    .map_err(|error| {
                        PushError::NotSent(PushFailure::Io(std::io::Error::other(error)))
                    })?;
            }
            received
                .install(
                    &quarantine.repository,
                    prepared.limits.destination_packs,
                    control.cancel,
                )
                .map_err(|error| PushError::NotSent(PushFailure::install(error)))?;
        }
        let private_inputs = quarantine
            .as_ref()
            .map(|quarantine| quarantine.observe())
            .transpose()
            .map_err(|error| PushError::NotSent(error.into()))?;
        report.unpack = Some(Status::Ok);
        // Like Git, a reference that already has the requested (non-deletion) value is up to date,
        // whatever the expected value, and isn't part of the update.
        let null = crate::ObjectId::null(prepared.format);
        let mut current_values = Vec::with_capacity(prepared.commands.len());
        for command in &prepared.commands {
            let current = refs
                .read(&command.name)
                .map_err(|error| PushError::Uncertain {
                    cause: PushFailure::reference(error),
                    report: Box::new(report.clone()),
                })?;
            current_values.push(match current {
                Some(Target::Direct(id)) => Some(id),
                _ => None,
            });
        }
        let up_to_date: Vec<bool> = prepared
            .commands
            .iter()
            .zip(&current_values)
            // Deletions are never up to date: deleting an absent reference still checks its
            // expected value, as with `git push --force-with-lease`.
            .map(|(command, current)| !command.deletes() && *current == Some(command.new))
            .collect();
        let hook_updates = |selected: &dyn Fn(usize) -> bool| {
            prepared
                .commands
                .iter()
                .enumerate()
                .filter(|&(index, _)| selected(index))
                .map(|(index, command)| super::hooks::HookUpdate {
                    old: current_values[index].unwrap_or(null),
                    new: command.new,
                    name: &command.name,
                })
                .collect::<Vec<_>>()
        };
        let hook_error = |error: PushFailure, report: &PushReport| PushError::Uncertain {
            cause: error,
            report: Box::new(report.clone()),
        };
        let (accepted, messages) = hooks
            .run(
                "pre-receive",
                &[],
                &hook_updates(&|index| !up_to_date[index]),
                control,
                quarantine.as_ref().map(|quarantine| quarantine.path()),
                repository.object_dir(),
            )
            .map_err(PushError::NotSent)?;
        report
            .progress
            .extend((!messages.is_empty()).then_some(messages));
        if !accepted {
            for (index, reference) in report.refs.iter_mut().enumerate() {
                if !up_to_date[index] {
                    reference.status =
                        Some(Status::Rejected(b"pre-receive hook declined".to_vec()));
                    reference.rejection_origin = Some(RejectionOrigin::Receiver);
                } else {
                    reference.status = Some(Status::Ok);
                }
            }
            return Ok(report);
        }
        if let (Some(quarantine), Some(expected)) = (&quarantine, &private_inputs)
            && quarantine
                .observe()
                .map_err(|error| PushError::NotSent(error.into()))?
                != *expected
        {
            return Err(PushError::NotSent(PushFailure::Unsupported(
                "pre-receive modified quarantined object storage",
            )));
        }
        control
            .check()
            .map_err(|error| PushError::NotSent(error.into()))?;
        if unstored_empty_tree.is_some() {
            repository
                .loose_objects()
                .write_object(prepared.format, crate::ObjectKind::Tree, b"")
                .map_err(|error| {
                    PushError::NotSent(PushFailure::Io(std::io::Error::other(error)))
                })?;
        }
        if let Some(received) = &received {
            received
                .install(
                    &repository,
                    prepared.limits.destination_packs,
                    control.cancel,
                )
                .map_err(|error| PushError::NotSent(PushFailure::install(error)))?;
        }
        if let Some(quarantine) = quarantine.take() {
            let path = quarantine.path().to_owned();
            quarantine.close().map_err(|source| PushError::Uncertain {
                cause: PushFailure::QuarantineCleanup {
                    path,
                    source,
                    failure: None,
                },
                report: Box::new(report.clone()),
            })?;
        }
        for (index, command) in prepared.commands.iter().enumerate() {
            if let Err(error) = control.check() {
                return Err(PushError::Uncertain {
                    cause: error.into(),
                    report: Box::new(report),
                });
            }
            report.refs[index].attempted = true;
            if up_to_date[index] {
                report.refs[index].status = Some(Status::Ok);
                continue;
            }
            let old = current_values[index].unwrap_or(null).to_string();
            let new = command.new.to_string();
            let (accepted, messages) = hooks
                .run(
                    "update",
                    &[command.name.as_bytes(), old.as_bytes(), new.as_bytes()],
                    &[],
                    control,
                    None,
                    repository.object_dir(),
                )
                .map_err(|error| hook_error(error, &report))?;
            report
                .progress
                .extend((!messages.is_empty()).then_some(messages));
            if !accepted {
                // As `git-receive-pack` reports it.
                let mut message = b"error: hook declined to update ".to_vec();
                message.extend_from_slice(command.name.as_bytes());
                message.push(b'\n');
                report.progress.push(message);
                report.refs[index].status = Some(Status::Rejected(b"hook declined".to_vec()));
                report.refs[index].rejection_origin = Some(RejectionOrigin::Receiver);
                continue;
            }
            if hide.matches(&command.name) {
                report.refs[index].status = Some(Status::Rejected(b"hidden reference".to_vec()));
                report.refs[index].rejection_origin = Some(RejectionOrigin::Receiver);
                continue;
            }
            let checked_out =
                is_checked_out(&repository, &command.name, config_inputs).map_err(|error| {
                    PushError::Uncertain {
                        cause: error,
                        report: Box::new(report.clone()),
                    }
                })?;
            if checked_out {
                if command.deletes() && deny_delete_current {
                    report.refs[index].status = Some(Status::Rejected(
                        b"deletion of current branch denied".to_vec(),
                    ));
                    report.refs[index].rejection_origin = Some(RejectionOrigin::Receiver);
                    continue;
                }
                if !command.deletes() && current_policy == CurrentPolicy::Refuse {
                    report.refs[index].status = Some(Status::Rejected(
                        b"branch is currently checked out".to_vec(),
                    ));
                    report.refs[index].rejection_origin = Some(RejectionOrigin::Receiver);
                    continue;
                }
                if !command.deletes() && current_policy == CurrentPolicy::Warn {
                    report
                        .warnings
                        .push(b"updating currently checked out branch".to_vec());
                }
            }
            if command.deletes() && deny_deletes {
                report.refs[index].status = Some(Status::Rejected(b"deletion denied".to_vec()));
                report.refs[index].rejection_origin = Some(RejectionOrigin::Receiver);
                continue;
            }
            if command.force == super::ForcePolicy::Allow
                && command.name.as_bytes().starts_with(b"refs/heads/")
                && let Some(old) = command.expected
                && !command.deletes()
                && deny_non_fast_forwards
            {
                let objects = repository
                    .objects_with(prepared.limits.destination_packs)
                    .map_err(|error| PushError::Uncertain {
                        cause: PushFailure::install(FetchError::Destination(error)),
                        report: Box::new(report.clone()),
                    })?;
                let fast_forward = objects
                    .is_ancestor_with(old, command.new, crate::HistoryLimits::default())
                    .unwrap_or(false);
                if !fast_forward {
                    report.refs[index].status =
                        Some(Status::Rejected(b"non-fast-forward".to_vec()));
                    report.refs[index].rejection_origin = Some(RejectionOrigin::Receiver);
                    continue;
                }
            }
            let edit = RefEdit {
                name: command.name.clone(),
                dereference: crate::refs::Dereference::Stored,
                target: (!command.deletes()).then_some(Target::Direct(command.new)),
                expected: command
                    .expected
                    .map_or(Expected::Absent, |id| Expected::Value(Target::Direct(id))),
                reflog: reflog_policy(&refs, command, log_policy, identity).map_err(|cause| {
                    PushError::Uncertain {
                        cause,
                        report: Box::new(report.clone()),
                    }
                })?,
            };
            match refs.transaction(&[edit]) {
                Ok(outcomes) => {
                    report.refs[index].status = Some(Status::Ok);
                    report.refs[index].effects = outcomes.into_iter().next();
                }
                Err(TransactionError::Prepare { source, .. }) => {
                    let (reason, origin) = if matches!(source, ReferenceError::Mismatch { .. }) {
                        (b"stale old value".to_vec(), RejectionOrigin::ExpectedValue)
                    } else {
                        (
                            b"reference storage rejected".to_vec(),
                            RejectionOrigin::Receiver,
                        )
                    };
                    report.refs[index].status = Some(Status::Rejected(reason));
                    report.refs[index].rejection_origin = Some(origin);
                }
                Err(TransactionError::Publish { source, outcomes }) => {
                    if let Some(effect) = outcomes.into_iter().next() {
                        if effect.reference == RefOutcome::Published {
                            report.refs[index].status = Some(Status::Ok);
                        }
                        report.refs[index].effects = Some(effect);
                    }
                    return Err(PushError::Uncertain {
                        cause: PushFailure::reference(source),
                        report: Box::new(report),
                    });
                }
            }
        }
        let updated =
            |index: usize| !up_to_date[index] && report.refs[index].status == Some(Status::Ok);
        let post_updates = hook_updates(&updated);
        if !post_updates.is_empty() {
            let (_, messages) = hooks
                .run(
                    "post-receive",
                    &[],
                    &post_updates,
                    control,
                    None,
                    repository.object_dir(),
                )
                .map_err(|error| hook_error(error, &report))?;
            let names: Vec<&[u8]> = post_updates
                .iter()
                .map(|update| update.name.as_bytes())
                .collect();
            let (_, update_messages) = hooks
                .run(
                    "post-update",
                    &names,
                    &[],
                    control,
                    None,
                    repository.object_dir(),
                )
                .map_err(|error| hook_error(error, &report))?;
            report
                .progress
                .extend((!messages.is_empty()).then_some(messages));
            report
                .progress
                .extend((!update_messages.is_empty()).then_some(update_messages));
        }
        Ok(report)
    })();
    match quarantine {
        Some(quarantine) => quarantine.finish(result, pending),
        None => result,
    }
}

fn is_checked_out(
    repository: &Repository,
    name: &RefName,
    config_inputs: &ConfigInputs,
) -> Result<bool, PushFailure> {
    if !name.as_bytes().starts_with(b"refs/heads/") {
        return Ok(false);
    }
    let main = Repository::open_with_config(repository.common_dir(), config_inputs)
        .map_err(PushFailure::destination)?;
    if head_names(&main, name)? {
        return Ok(true);
    }
    let entries = match fs::read_dir(repository.common_dir().join("worktrees")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            return Err(PushFailure::Unsupported("worktree registration"));
        }
        let worktree = Repository::open_with_config(entry.path(), config_inputs)
            .map_err(PushFailure::destination)?;
        if head_names(&worktree, name)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn head_names(repository: &Repository, name: &RefName) -> Result<bool, PushFailure> {
    if repository.is_bare() {
        return Ok(false);
    }
    let head = RefName::HEAD;
    let refs = repository.references().map_err(PushFailure::reference)?;
    if !matches!(
        refs.read(&head).map_err(PushFailure::reference)?,
        Some(Target::Symbolic(_))
    ) {
        return Ok(false);
    }
    Ok(refs
        .resolve_with(&head, 32)
        .map_err(PushFailure::reference)?
        .name
        == *name)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CurrentPolicy {
    Refuse,
    Allow,
    Warn,
}

fn current_policy(repository: &Repository) -> Result<CurrentPolicy, PushError> {
    let value = repository
        .config()
        .value("receive", None, "denycurrentbranch");
    match value {
        None | Some(None) => Ok(CurrentPolicy::Refuse),
        Some(Some(value)) if value.eq_ignore_ascii_case(b"refuse") => Ok(CurrentPolicy::Refuse),
        Some(Some(value)) if value.eq_ignore_ascii_case(b"ignore") => Ok(CurrentPolicy::Allow),
        Some(Some(value)) if value.eq_ignore_ascii_case(b"warn") => Ok(CurrentPolicy::Warn),
        Some(Some(value)) if value.eq_ignore_ascii_case(b"updateInstead") => {
            Err(PushError::NotSent(PushFailure::Unsupported(
                "receive.denyCurrentBranch=updateInstead",
            )))
        }
        Some(Some(value)) if crate::config::boolean(Some(value)) == Some(true) => {
            Ok(CurrentPolicy::Refuse)
        }
        Some(Some(value)) if crate::config::boolean(Some(value)) == Some(false) => {
            Ok(CurrentPolicy::Allow)
        }
        _ => Err(PushError::NotSent(PushFailure::Unsupported(
            "receive.denyCurrentBranch value",
        ))),
    }
}

fn configured_boolean(
    repository: &Repository,
    section: &str,
    name: &str,
) -> Result<Option<bool>, PushError> {
    let value = repository.config().value(section, None, name);
    match value {
        None => Ok(None),
        Some(None) => Ok(Some(true)),
        Some(Some(value)) => {
            crate::config::boolean(Some(value))
                .map(Some)
                .ok_or(PushError::NotSent(PushFailure::Unsupported(
                    "receive boolean policy",
                )))
        }
    }
}

fn delete_current_policy(repository: &Repository) -> Result<bool, PushError> {
    let value = repository
        .config()
        .value("receive", None, "denydeletecurrent");
    match value {
        None | Some(None) => Ok(true),
        Some(Some(value)) if value.eq_ignore_ascii_case(b"refuse") => Ok(true),
        Some(Some(value)) if value.eq_ignore_ascii_case(b"ignore") => Ok(false),
        Some(Some(value)) => crate::config::boolean(Some(value)).ok_or(PushError::NotSent(
            PushFailure::Unsupported("receive.denyDeleteCurrent value"),
        )),
    }
}

#[derive(Clone, Copy)]
enum LogPolicy {
    Existing,
    Standard,
    Always,
}

fn log_policy(repository: &Repository) -> Result<LogPolicy, PushError> {
    let value = repository.config().value("core", None, "logallrefupdates");
    match value {
        Some(Some(value)) if value.eq_ignore_ascii_case(b"always") => Ok(LogPolicy::Always),
        None if repository.is_bare() => Ok(LogPolicy::Existing),
        None => Ok(LogPolicy::Standard),
        _ => match configured_boolean(repository, "core", "logallrefupdates")? {
            Some(true) => Ok(LogPolicy::Standard),
            Some(false) => Ok(LogPolicy::Existing),
            None => unreachable!(),
        },
    }
}

fn reflog_policy(
    refs: &crate::refs::References<'_>,
    command: &super::PushCommand,
    policy: LogPolicy,
    identity: Option<&Signature>,
) -> Result<Reflog, PushFailure> {
    if command.deletes() {
        return Ok(Reflog::Delete);
    }
    let name = command.name.as_bytes();
    let standard = name.starts_with(b"refs/heads/")
        || name.starts_with(b"refs/remotes/")
        || name.starts_with(b"refs/notes/");
    let create =
        matches!(policy, LogPolicy::Always) || matches!(policy, LogPolicy::Standard) && standard;
    let exists = refs
        .has_reflog(&command.name)
        .map_err(PushFailure::reference)?;
    if !create && !exists {
        return Ok(Reflog::Preserve);
    }
    let identity = identity.ok_or(PushFailure::Identity)?;
    Ok(Reflog::Append {
        committer: identity.clone(),
        message: b"push".to_vec(),
    })
}
