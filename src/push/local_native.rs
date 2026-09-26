//! Native local pack installation followed by conditional reference publication.

use std::fs;
use std::path::Path;

use super::{PreparedPush, PushError, PushFailure, PushReport, Status};
use crate::fetch::{
    Advertisement, FetchError, FetchLimits, KnownHistory, NativeContents, ReceivedFetch,
};
use crate::refs::{Expected, RefEdit, RefName, RefOutcome, Reflog, Target, TransactionError};
use crate::transport::TransportControl;
use crate::{Repository, Signature};

pub(super) fn send(
    destination: &Path,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
    identity: Option<&Signature>,
) -> Result<PushReport, PushError> {
    control
        .check()
        .map_err(|error| PushError::NotSent(error.into()))?;
    let repository = Repository::open(destination)
        .map_err(|error| PushError::NotSent(PushFailure::destination(error)))?;
    if repository.object_format() != prepared.format {
        return Err(PushError::NotSent(PushFailure::Unsupported(
            "destination object format",
        )));
    }
    if prepared.has_options {
        return Err(PushError::NotSent(PushFailure::Unsupported(
            "native local push options",
        )));
    }
    if identity.is_some_and(|person| person.validate().is_err() || person.seconds < 0) {
        return Err(PushError::NotSent(PushFailure::Identity));
    }
    let current_policy = current_policy(&repository)?;
    let deny_delete_current = delete_current_policy(&repository)?;
    let deny_deletes = configured_boolean(&repository, "receive", "denydeletes")?.unwrap_or(false);
    let deny_non_fast_forwards =
        configured_boolean(&repository, "receive", "denynonfastforwards")?.unwrap_or(false);
    let log_policy = log_policy(&repository)?;
    let hide = HideRules::parse(&repository)?;
    for (section, key) in [
        ("receive", "procreceiverefs"),
        ("receive", "maxinputsize"),
        ("receive", "fsckobjects"),
        ("transfer", "fsckobjects"),
    ] {
        if repository.config().value(section, None, key).is_some() {
            return Err(PushError::NotSent(PushFailure::Unsupported(
                "configured receive policy",
            )));
        }
    }
    if !repository.shallow_roots().is_empty() {
        return Err(PushError::NotSent(PushFailure::Unsupported(
            "shallow destination",
        )));
    }
    let refs = repository
        .references()
        .map_err(|error| PushError::NotSent(PushFailure::reference(error)))?;
    let mut report = PushReport::pending(&prepared.commands);
    for reference in &mut report.refs {
        reference.rewrite_complete = true;
    }
    if prepared.commands.is_empty() {
        return Ok(report);
    }
    if repository
        .config()
        .value("core", None, "hookspath")
        .is_some()
    {
        return Err(PushError::NotSent(PushFailure::Unsupported(
            "configured receive hooks",
        )));
    }
    for hook in [
        "pre-receive",
        "update",
        "post-receive",
        "proc-receive",
        "reference-transaction",
        "push-to-checkout",
    ] {
        let path = repository.common_dir().join("hooks").join(hook);
        match fs::symlink_metadata(path) {
            Ok(_) => {
                return Err(PushError::NotSent(PushFailure::Unsupported(
                    "receive hooks",
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(PushError::NotSent(error.into())),
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
        let checked_out = is_checked_out(&repository, &command.name).map_err(PushError::NotSent)?;
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
    if !prepared.receiver_roots.is_empty() {
        let store = repository.objects(Default::default()).map_err(|error| {
            PushError::NotSent(PushFailure::install(FetchError::Destination(error)))
        })?;
        KnownHistory::new_local(
            &store,
            &prepared.receiver_roots,
            FetchLimits {
                max_known_objects: prepared.limits.pack.max_objects as usize,
                max_known_bytes: prepared
                    .limits
                    .pack
                    .max_input_bytes
                    .try_into()
                    .unwrap_or(usize::MAX),
                max_known_edges: prepared.limits.max_edges,
                known_read: prepared.limits.read,
                ..FetchLimits::default()
            },
            control.cancel,
        )
        .map_err(|error| PushError::NotSent(PushFailure::install(error)))?;
    }
    let limits = FetchLimits {
        max_pack_bytes: prepared
            .limits
            .pack
            .max_pack_bytes
            .try_into()
            .unwrap_or(usize::MAX),
        max_objects: prepared.limits.pack.max_objects as usize,
        ..FetchLimits::default()
    };
    if !prepared.pack.is_empty() {
        let received = ReceivedFetch::native(
            Advertisement {
                refs: Vec::new(),
                capabilities: Vec::new(),
            },
            prepared
                .commands
                .iter()
                .filter(|command| !command.deletes())
                .map(|command| command.new)
                .collect(),
            NativeContents {
                format: prepared.format,
                pack: Vec::new(),
                index: Vec::new(),
                checksum: prepared.checksum,
                objects: prepared.object_count() as usize,
                dependencies: Vec::new(),
                limits,
            },
        );
        received
            .install_bytes(
                &repository,
                Default::default(),
                control.cancel,
                &prepared.pack,
                &prepared.index,
                false,
            )
            .map_err(|error| PushError::NotSent(PushFailure::install(error)))?;
    }
    report.unpack = Some(Status::Ok);
    for (index, command) in prepared.commands.iter().enumerate() {
        if let Err(error) = control.check() {
            return Err(PushError::Uncertain {
                cause: error.into(),
                report: Box::new(report),
            });
        }
        report.refs[index].attempted = true;
        if hide.matches(&command.name) {
            report.refs[index].status = Some(Status::Rejected(b"hidden reference".to_vec()));
            continue;
        }
        let checked_out =
            is_checked_out(&repository, &command.name).map_err(|error| PushError::Uncertain {
                cause: error,
                report: Box::new(report.clone()),
            })?;
        if checked_out {
            if command.deletes() && deny_delete_current {
                report.refs[index].status = Some(Status::Rejected(
                    b"deletion of current branch denied".to_vec(),
                ));
                continue;
            }
            if !command.deletes() && current_policy == CurrentPolicy::Refuse {
                report.refs[index].status = Some(Status::Rejected(
                    b"branch is currently checked out".to_vec(),
                ));
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
            continue;
        }
        if command.force == super::ForcePolicy::Allow
            && command.name.as_bytes().starts_with(b"refs/heads/")
            && let Some(old) = command.expected
            && !command.deletes()
            && deny_non_fast_forwards
        {
            let objects =
                repository
                    .objects(Default::default())
                    .map_err(|error| PushError::Uncertain {
                        cause: PushFailure::install(FetchError::Destination(error)),
                        report: Box::new(report.clone()),
                    })?;
            let fast_forward = objects
                .is_ancestor(old, command.new, crate::HistoryLimits::default())
                .unwrap_or(false);
            if !fast_forward {
                report.refs[index].status = Some(Status::Rejected(b"non-fast-forward".to_vec()));
                continue;
            }
        }
        let edit = RefEdit {
            name: command.name.clone(),
            dereference: false,
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
            Err(TransactionError::Prepare { .. }) => {
                report.refs[index].status = Some(Status::Rejected(
                    b"reference precondition or storage rejected".to_vec(),
                ));
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
    Ok(report)
}

struct HideRules(Vec<(Vec<u8>, bool)>);

impl HideRules {
    fn parse(repository: &Repository) -> Result<Self, PushError> {
        let mut rules = Vec::new();
        for entry in repository.config().entries() {
            if entry.subsection.is_some()
                || !entry.name.eq_ignore_ascii_case(b"hiderefs")
                || !(entry.section.eq_ignore_ascii_case(b"transfer")
                    || entry.section.eq_ignore_ascii_case(b"receive"))
            {
                continue;
            }
            let value =
                entry
                    .value
                    .as_deref()
                    .ok_or(PushError::NotSent(PushFailure::Unsupported(
                        "implicit hideRefs pattern",
                    )))?;
            let (visible, pattern) = if let Some(pattern) = value.strip_prefix(b"!") {
                (true, pattern)
            } else {
                (false, value)
            };
            // Namespace rewriting is not active in the native adapter, so ^ matches the same
            // spelling as an ordinary pattern. Reject an empty pattern rather than hiding all refs.
            let pattern = pattern.strip_prefix(b"^").unwrap_or(pattern);
            if pattern.is_empty() {
                return Err(PushError::NotSent(PushFailure::Unsupported(
                    "empty hideRefs pattern",
                )));
            }
            rules.push((pattern.to_vec(), visible));
        }
        Ok(Self(rules))
    }

    fn matches(&self, name: &RefName) -> bool {
        let mut hidden = false;
        for (pattern, visible) in &self.0 {
            let pattern = pattern.strip_suffix(b"/").unwrap_or(pattern);
            let rest = name.as_bytes().strip_prefix(pattern);
            if rest.is_some_and(|rest| rest.is_empty() || rest.starts_with(b"/")) {
                hidden = !visible;
            }
        }
        hidden
    }
}

fn is_checked_out(repository: &Repository, name: &RefName) -> Result<bool, PushFailure> {
    if !name.as_bytes().starts_with(b"refs/heads/") {
        return Ok(false);
    }
    let main = Repository::open(repository.common_dir()).map_err(PushFailure::destination)?;
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
        let worktree = Repository::open(entry.path()).map_err(PushFailure::destination)?;
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
    let head = RefName::new(b"HEAD").expect("valid HEAD");
    let refs = repository.references().map_err(PushFailure::reference)?;
    if !matches!(
        refs.read(&head).map_err(PushFailure::reference)?,
        Some(Target::Symbolic(_))
    ) {
        return Ok(false);
    }
    Ok(refs
        .resolve(&head, 32)
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
