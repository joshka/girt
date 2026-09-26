//! Native local pack installation followed by conditional reference publication.

use std::fs;
use std::path::Path;

use super::{PreparedPush, PushError, PushFailure, PushReport, Status};
use crate::Repository;
use crate::fetch::{
    Advertisement, FetchError, FetchLimits, KnownHistory, NativeContents, ReceivedFetch,
};
use crate::refs::{Expected, RefEdit, RefName, Reflog, Target, TransactionError};
use crate::transport::TransportControl;

pub(super) fn send(
    destination: &Path,
    prepared: &PreparedPush,
    control: TransportControl<'_>,
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
    if !repository.shallow_roots().is_empty() {
        return Err(PushError::NotSent(PushFailure::Unsupported(
            "shallow destination",
        )));
    }
    let refs = repository
        .references()
        .map_err(|error| PushError::NotSent(PushFailure::reference(error)))?;
    let mut report = PushReport::pending(&prepared.commands);
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
        let actual = refs
            .read(&command.name)
            .map_err(|error| PushError::NotSent(PushFailure::reference(error)))?;
        let actual_id = match actual {
            Some(Target::Direct(id)) => Some(id),
            Some(Target::Symbolic(_)) => {
                return Err(PushError::NotSent(PushFailure::Unsupported(
                    "symbolic push destination",
                )));
            }
            None => None,
        };
        if actual_id != command.expected {
            return Err(PushError::NotSent(PushFailure::Stale {
                name: command.name.clone(),
                expected: command.expected,
                actual: actual_id,
            }));
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
    let received = ReceivedFetch::native(
        Advertisement {
            refs: Vec::new(),
            capabilities: Vec::new(),
        },
        prepared
            .commands
            .iter()
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
    report.unpack = Some(Status::Ok);
    for (index, command) in prepared.commands.iter().enumerate() {
        if let Err(error) = control.check() {
            return Err(PushError::Uncertain {
                cause: error.into(),
                report: Box::new(report),
            });
        }
        if is_checked_out(&repository, &command.name).map_err(|error| PushError::Uncertain {
            cause: error,
            report: Box::new(report.clone()),
        })? {
            report.refs[index].status = Some(Status::Rejected(
                b"branch is currently checked out".to_vec(),
            ));
            continue;
        }
        if command.force == super::ForcePolicy::Allow
            && repository
                .config()
                .value("receive", None, "denynonfastforwards")
                .is_some_and(|value| crate::config::boolean(value) == Some(true))
        {
            report.refs[index].status = Some(Status::Rejected(b"non-fast-forward".to_vec()));
            continue;
        }
        let edit = RefEdit {
            name: command.name.clone(),
            dereference: false,
            target: Some(Target::Direct(command.new)),
            expected: command
                .expected
                .map_or(Expected::Absent, |id| Expected::Value(Target::Direct(id))),
            reflog: Reflog::Preserve,
        };
        match refs.transaction(&[edit]) {
            Ok(_) => report.refs[index].status = Some(Status::Ok),
            Err(TransactionError::Prepare { .. }) => {
                report.refs[index].status = Some(Status::Rejected(
                    b"reference precondition or storage rejected".to_vec(),
                ));
            }
            Err(TransactionError::Publish { source, .. }) => {
                return Err(PushError::Uncertain {
                    cause: PushFailure::reference(source),
                    report: Box::new(report),
                });
            }
        }
    }
    Ok(report)
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
