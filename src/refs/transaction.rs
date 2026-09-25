use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{self, Write};

use super::store::{
    Lock, check_expected, conflicts, io_error, read_optional, remove_loose, validate_target,
};
use super::{
    Expected, RefName, ReferenceError, References, Reflog, ReflogEntry, Target, packed, reflog,
};
use crate::ObjectId;

/// A conditional change to one stored name or one symbolic chain's terminal name.
#[derive(Clone, Debug)]
pub struct RefEdit {
    /// Initial name; also the destination when `dereference` is false.
    pub name: RefName,
    /// Follow up to 32 symbolic hops, keeping the chain unchanged.
    ///
    /// Only direct updates or deletion can be dereferenced. Preconditions apply to the terminal
    /// stored value. With `false`, a direct replacement with append logging still locks and
    /// resolves the old symbolic chain for the log identity, but edits/logs only `name` and checks
    /// the precondition against its stored value. Symbolic replacement locks both old and new
    /// chains to record their terminal IDs. Overlapping operations are rejected.
    pub dereference: bool,
    /// Replacement, or `None` to delete. Object existence/type is not checked.
    pub target: Option<Target>,
    /// Compared under locks before any publication.
    pub expected: Expected,
    /// Explicit history policy.
    pub reflog: Reflog,
}

/// Reference publication state at the time a transaction returns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefOutcome {
    /// No reference publication was attempted successfully for this operation.
    Unchanged,
    /// Its packed record was removed; loose publication/deletion has not succeeded.
    ///
    /// A packed-only reference is already absent. A loose shadow, if present, remains.
    PackedRemoved,
    /// Replacement or deletion completed, including deleting an already absent ref.
    Published,
}

/// Reflog mutation state at the time a transaction returns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogOutcome {
    /// No log mutation was attempted.
    NotAttempted,
    /// The complete record was appended.
    Appended,
    /// The requested log is absent, including when already absent before deletion.
    Deleted,
    /// Mutation failed; this many record bytes were written before the failure.
    /// Deletion failures always report zero and preserve the log under the writer contract.
    ///
    /// Zero can still mean an empty log was created. A nonzero count can leave a malformed tail.
    Failed {
        /// Bytes from this record written before the error.
        bytes_written: usize,
    },
}

/// Effects for one input operation, retained in input order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefEditOutcome {
    /// Terminal destination (or original name for a stored edit).
    pub name: RefName,
    /// Reference publication result.
    pub reference: RefOutcome,
    /// Requested logs in symbolic traversal order; empty for `Preserve`.
    pub logs: Vec<(RefName, LogOutcome)>,
}

/// Failure before publication or a report of effects already made.
#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    /// No reference or log contents were changed. Empty directories can remain.
    #[error("reference transaction preparation failed: {source}")]
    Prepare {
        /// Input index when the failure belongs to one operation; otherwise a batch/lock failure.
        operation: Option<usize>,
        /// Validation, precondition, lock, or read failure.
        #[source]
        source: ReferenceError,
    },
    /// Publication stopped at the first error, without rollback.
    #[error("reference transaction publication failed: {source}")]
    Publish {
        /// Exact completed effects in caller order; later operations can have packed removals.
        outcomes: Vec<RefEditOutcome>,
        /// Underlying error. A loose unlink after packed removal retains `PackedDeleted`.
        #[source]
        source: ReferenceError,
    },
}

impl References<'_> {
    /// Applies a conditional batch with explicit reflog policy.
    ///
    /// Reftable publishes references and logs atomically within each stack, with explicit partial
    /// effects across linked-worktree stacks; see [`References`]. The remaining storage details
    /// below describe the files backend.
    ///
    /// Preparation holds `packed-refs.lock`, discovers symbolic chains, locks their union in
    /// name-byte order, and rechecks every stored chain value and precondition. Reflog locks follow
    /// in name-byte order. Duplicate/overlapping chains and ancestor/descendant names are rejected,
    /// including delete/create namespace swaps. Existing packed conflicts are rejected. Contention
    /// fails immediately; locks are never stolen. A changed chain fails rather than being retried.
    ///
    /// Publication first removes all selected packed records in one replacement. It then publishes
    /// refs in caller order, appending each operation's requested logs after its ref succeeds.
    /// All owned locks remain held until return. Readers can observe any intermediate state.
    /// An I/O error stops publication, returning per-ref and per-log effects; no rollback occurs.
    /// In particular, a ref can advance with a missing or partial reflog entry.
    ///
    /// Logs use append writes, never whole-file replacement. Ref locks coordinate writers of the
    /// same destination; log locks coordinate girt transactions. Git can append automatic HEAD logs
    /// without honoring those log locks, so ordering against such appends is not promised. Reflog
    /// expiry and external log rewriting must be excluded by the caller while writing.
    /// `Reflog::Delete` removes only the edited destination's log after reference deletion.
    /// Direct branch edits do not automatically log HEAD or discover aliases; use a resolved HEAD
    /// edit to log that chain. A stored direct replacement with append logging locks and resolves
    /// the old symbolic chain, recording its terminal ID (zero when unborn) in only the edited
    /// name's log. Its precondition still compares the original stored value, and the old branch
    /// and its log remain unchanged. A stored symbolic replacement additionally locks the new
    /// chain and records its terminal ID, or zero if unborn. Stored symbolic deletion logs the
    /// old terminal ID and zero, without deleting the target.
    ///
    /// No hooks, config/environment policy, object validation, fsync, crash recovery, or
    /// filesystem-wide atomic visibility is provided. Cleanup is best effort; termination or
    /// cleanup failure can leave locks/temporary files. See [`References`] for filesystem trust.
    ///
    /// # Errors
    ///
    /// [`TransactionError::Prepare`] preserves ref/log contents on malformed data, invalid edits,
    /// precondition mismatch, namespace conflicts, or lock failure. [`TransactionError::Publish`]
    /// reports effects at meaningful publication boundaries. An empty batch is a successful no-op.
    pub fn transaction(&self, edits: &[RefEdit]) -> Result<Vec<RefEditOutcome>, TransactionError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "refs.transaction",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
            edits = edits.len(),
        );

        let operation = || {
            if edits.is_empty() {
                return Ok(Vec::new());
            }
            for (index, edit) in edits.iter().enumerate() {
                validate_edit(self.repository.object_format(), edit).map_err(|source| {
                    TransactionError::Prepare {
                        operation: Some(index),
                        source,
                    }
                })?;
            }
            self.prepare_transaction(edits)?.publish()
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| {
            crate::trace::transaction(error, &span)
        });

        result
    }

    pub(crate) fn prepare_transaction(
        &self,
        edits: &[RefEdit],
    ) -> Result<PreparedBackend, TransactionError> {
        if self.repository.reference_backend() == super::Backend::Reftable {
            super::reftable::backend::prepare(self, edits).map(PreparedBackend::Reftable)
        } else {
            self.prepare_files_transaction(edits)
                .map(Box::new)
                .map(PreparedBackend::Files)
        }
    }

    pub(super) fn prepare_files_transaction(
        &self,
        edits: &[RefEdit],
    ) -> Result<Prepared, TransactionError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "refs.prepare_transaction",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            let batch_error = |source| TransactionError::Prepare {
                operation: None,
                source,
            };
            let packed_lock = Lock::acquire(self.repository.common_dir().join("packed-refs"))
                .map_err(batch_error)?;
            let bytes = read_optional(&packed_lock.destination)
                .map_err(batch_error)?
                .unwrap_or_default();
            let packed = packed::parse(
                self.repository.object_format(),
                &bytes,
                &packed_lock.destination,
            )
            .map_err(batch_error)?;
            let mut plans = Vec::new();
            let mut names = BTreeSet::new();
            for (index, edit) in edits.iter().enumerate() {
                let error = |source| TransactionError::Prepare {
                    operation: Some(index),
                    source,
                };
                let chain = self.discover_chain(edit, &packed).map_err(error)?;
                let new_chain = if let (Some(Target::Symbolic(target)), Reflog::Append { .. }) =
                    (&edit.target, &edit.reflog)
                {
                    let dependency = RefEdit {
                        name: target.clone(),
                        dereference: true,
                        target: None,
                        expected: Expected::Any,
                        reflog: Reflog::Preserve,
                    };
                    let new_chain = self.discover_chain(&dependency, &packed).map_err(error)?;
                    if new_chain.iter().any(|(name, _)| name == &edit.name) {
                        return Err(error(ReferenceError::Cycle(edit.name.clone())));
                    }
                    new_chain
                } else {
                    Vec::new()
                };
                let dependencies: BTreeSet<_> = chain
                    .iter()
                    .chain(&new_chain)
                    .map(|(name, _)| name)
                    .collect();
                for name in dependencies {
                    if !names.insert(name.clone())
                        || names
                            .iter()
                            .any(|other: &RefName| conflicts(name.as_bytes(), other.as_bytes()))
                    {
                        return Err(error(ReferenceError::Conflict(
                            self.path(name).map_err(error)?,
                        )));
                    }
                }
                plans.push((chain, new_chain));
            }

            let mut locks = BTreeMap::new();
            for name in names {
                self.check_packed_namespace(&name, &packed)
                    .map_err(batch_error)?;
                locks.insert(
                    name.clone(),
                    Lock::acquire(self.path(&name).map_err(batch_error)?).map_err(batch_error)?,
                );
            }
            let mut operations = Vec::new();
            let mut log_names = BTreeSet::new();
            for (index, (edit, (chain, new_chain))) in edits.iter().zip(plans).enumerate() {
                let error = |source| TransactionError::Prepare {
                    operation: Some(index),
                    source,
                };
                for (name, observed) in chain.iter().chain(&new_chain) {
                    check_expected(
                        self.read_locked(name, &packed).map_err(error)?,
                        match observed {
                            Some(target) => Expected::Value(target.clone()),
                            None => Expected::Absent,
                        },
                    )
                    .map_err(error)?;
                }
                let (name, actual) = if edit.dereference {
                    chain.last().unwrap()
                } else {
                    chain.first().unwrap()
                };
                check_expected(actual.clone(), edit.expected.clone()).map_err(error)?;
                let mut logs = Vec::new();
                if let Reflog::Append { committer, message } = &edit.reflog {
                    // Discovery includes the old chain for a stored direct replacement. All its
                    // values have now been rechecked under locks; only the edited name is
                    // published.
                    let old = log_id(
                        self.repository.object_format(),
                        chain.last().unwrap().1.as_ref(),
                    )
                    .map_err(error)?;
                    let new_target = new_chain
                        .last()
                        .map_or(edit.target.as_ref(), |(_, value)| value.as_ref());
                    let new = log_id(self.repository.object_format(), new_target).map_err(error)?;
                    let record = ReflogEntry {
                        old,
                        new,
                        committer: committer.clone(),
                        message: message.clone(),
                    };
                    let record = record.encode().map_err(error)?;
                    let logged_chain = if edit.dereference {
                        &chain[..]
                    } else {
                        &chain[..1]
                    };
                    for (log_name, _) in logged_chain {
                        log_names.insert(log_name.clone());
                        logs.push((log_name.clone(), record.clone()));
                    }
                }
                if edit.reflog == Reflog::Delete {
                    log_names.insert(name.clone());
                    logs.push((name.clone(), Vec::new()));
                }
                operations.push(Operation {
                    name: name.clone(),
                    target: edit.target.clone(),
                    delete_log: edit.reflog == Reflog::Delete,
                    packed_removed: edit.target.is_none() && packed.contains_key(name),
                    logs,
                });
            }
            let mut log_locks = BTreeMap::new();
            for name in log_names {
                let path = self.reflog_path(&name).map_err(batch_error)?;
                let lock = Lock::acquire(path.clone()).map_err(batch_error)?;
                if !operations.iter().any(|op| op.delete_log && op.name == name)
                    && let Some(bytes) = read_optional(&path).map_err(batch_error)?
                {
                    reflog::parse(self.repository.object_format(), &bytes, &path)
                        .map_err(batch_error)?;
                }
                log_locks.insert(name, lock);
            }
            let mut replacement = bytes;
            for operation in &operations {
                if operation.packed_removed {
                    replacement = packed::without_ref(
                        self.repository.object_format(),
                        &replacement,
                        &operation.name,
                    );
                }
            }
            Ok(Prepared {
                packed_lock,
                replacement,
                locks,
                log_locks,
                operations,
            })
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| {
            crate::trace::transaction(error, &span)
        });

        result
    }

    fn discover_chain(
        &self,
        edit: &RefEdit,
        packed: &packed::Packed,
    ) -> Result<Vec<(RefName, Option<Target>)>, ReferenceError> {
        let resolve_old = edit.dereference || matches!(edit.reflog, Reflog::Append { .. });
        let mut chain = Vec::new();
        let mut name = edit.name.clone();
        loop {
            if chain.iter().any(|(seen, _)| seen == &name) {
                return Err(ReferenceError::Cycle(name));
            }
            let target = self.read_locked(&name, packed)?;
            chain.push((name.clone(), target.clone()));
            match target {
                Some(Target::Symbolic(next)) if resolve_old => {
                    if chain.len() > 32 {
                        return Err(ReferenceError::Depth(32));
                    }
                    name = next;
                }
                _ => return Ok(chain),
            }
        }
    }
}

pub(super) fn validate_edit(
    format: crate::ObjectFormat,
    edit: &RefEdit,
) -> Result<(), ReferenceError> {
    super::store::validate_expected(format, &edit.expected)?;
    if edit.reflog == Reflog::Delete && edit.target.is_some() {
        return Err(ReferenceError::Unsupported(
            "log deletion requires reference deletion",
        ));
    }
    if let Some(target) = &edit.target {
        validate_target(format, target)?;
        if edit.name.as_bytes() == b"HEAD"
            && matches!(target, Target::Symbolic(next) if next.as_bytes() == b"HEAD")
        {
            return Err(ReferenceError::InvalidHeadTarget);
        }
        if edit.dereference && matches!(target, Target::Symbolic(_)) {
            return Err(ReferenceError::Unsupported("resolved symbolic replacement"));
        }
    }
    if let Reflog::Append { committer, message } = &edit.reflog {
        reflog::validate(committer, message)?;
    }
    Ok(())
}

fn log_id(
    format: crate::ObjectFormat,
    target: Option<&Target>,
) -> Result<ObjectId, ReferenceError> {
    match target {
        None => Ok(ObjectId::null(format)),
        Some(Target::Direct(id)) => Ok(*id),
        Some(Target::Symbolic(_)) => Err(ReferenceError::Unsupported(
            "stored symbolic edits with reflogs",
        )),
    }
}

struct Operation {
    name: RefName,
    target: Option<Target>,
    packed_removed: bool,
    delete_log: bool,
    logs: Vec<(RefName, Vec<u8>)>,
}

pub(crate) struct Prepared {
    packed_lock: Lock,
    replacement: Vec<u8>,
    locks: BTreeMap<RefName, Lock>,
    log_locks: BTreeMap<RefName, Lock>,
    operations: Vec<Operation>,
}

impl Prepared {
    pub(crate) fn publish(self) -> Result<Vec<RefEditOutcome>, TransactionError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "refs.publish",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            let mut outcomes: Vec<_> = self
                .operations
                .iter()
                .map(|op| RefEditOutcome {
                    name: op.name.clone(),
                    reference: RefOutcome::Unchanged,
                    logs: op
                        .logs
                        .iter()
                        .map(|(name, _)| (name.clone(), LogOutcome::NotAttempted))
                        .collect(),
                })
                .collect();
            if self.operations.iter().any(|op| op.packed_removed) {
                self.packed_lock
                    .publish_retaining_lock(&self.replacement)
                    .map_err(|source| TransactionError::Publish {
                        outcomes: outcomes.clone(),
                        source,
                    })?;
                for (operation, outcome) in self.operations.iter().zip(&mut outcomes) {
                    if operation.packed_removed {
                        outcome.reference = RefOutcome::PackedRemoved;
                    }
                }
            }
            for (index, operation) in self.operations.iter().enumerate() {
                let lock = &self.locks[&operation.name];
                let result = match &operation.target {
                    Some(target) => {
                        let bytes = match target {
                            Target::Direct(id) => format!("{id}\n").into_bytes(),
                            Target::Symbolic(name) => {
                                let mut bytes = b"ref: ".to_vec();
                                bytes.extend_from_slice(name.as_bytes());
                                bytes.push(b'\n');
                                bytes
                            }
                        };
                        lock.publish_retaining_lock(&bytes)
                    }
                    None => lock
                        .check_owned()
                        .and_then(|()| remove_loose(&lock.destination, operation.packed_removed)),
                };
                if let Err(source) = result {
                    return Err(TransactionError::Publish { outcomes, source });
                }
                outcomes[index].reference = RefOutcome::Published;
                for (log_index, (name, record)) in operation.logs.iter().enumerate() {
                    let path = &self.log_locks[name].destination;
                    let result = self.log_locks[name]
                        .check_owned()
                        .map_err(|error| (0, error))
                        .and_then(|()| {
                            if operation.delete_log {
                                remove_loose(path, false).map_err(|error| (0, error))
                            } else {
                                append(path, record)
                            }
                        });
                    match result {
                        Ok(()) => {
                            outcomes[index].logs[log_index].1 = if operation.delete_log {
                                LogOutcome::Deleted
                            } else {
                                LogOutcome::Appended
                            }
                        }
                        Err((bytes_written, source)) => {
                            outcomes[index].logs[log_index].1 =
                                LogOutcome::Failed { bytes_written };
                            return Err(TransactionError::Publish { outcomes, source });
                        }
                    }
                }
            }
            Ok(outcomes)
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| {
            crate::trace::transaction(error, &span)
        });

        result
    }
}

fn append(path: &std::path::Path, record: &[u8]) -> Result<(), (usize, ReferenceError)> {
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|source| (0, io_error(path, source)))?;
    append_record(&mut file, record).map_err(|(count, source)| (count, io_error(path, source)))
}

// Keep byte progress explicit: write_all discards it on a short write followed by an error.
fn append_record(writer: &mut impl Write, record: &[u8]) -> Result<(), (usize, io::Error)> {
    let mut count = 0;
    while count < record.len() {
        match writer.write(&record[count..]) {
            Ok(0) => return Err((count, io::Error::from(io::ErrorKind::WriteZero))),
            Ok(written) => count += written,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err((count, error)),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "transaction_tests.rs"]
mod tests;

pub(crate) enum PreparedBackend {
    Files(Box<Prepared>),
    Reftable(super::reftable::backend::Prepared),
}

impl PreparedBackend {
    pub(crate) fn publish(self) -> Result<Vec<RefEditOutcome>, TransactionError> {
        match self {
            Self::Files(prepared) => prepared.publish(),
            Self::Reftable(prepared) => prepared.publish(),
        }
    }
}
