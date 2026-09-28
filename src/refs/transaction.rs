use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::sync::atomic::AtomicBool;

use super::lock_wait::check_cancelled;
use super::store::{
    Lock, check_expected, conflicts, io_error, read_optional, remove_loose, validate_target,
};
use super::{
    Expected, FilesTransactionOptions, LockWait, RefName, ReferenceError, References, Reflog,
    ReflogEntry, Target, packed, reflog,
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
    /// Requested logs in symbolic traversal order; empty for `Preserve` or a skipped
    /// conditional append policy.
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
                validate_edit(self.object_format, edit).map_err(|source| {
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

    /// Applies a files-only transaction with cancellable, per-acquisition lock waits.
    ///
    /// Holds the packed lock first, then reference locks in name-byte order, then reflog locks
    /// in name-byte order. Each packed/reference acquisition gets its own configured budget;
    /// earlier locks remain held. Reflog contention always fails immediately. This sorted order
    /// need not match another writer's edit order, so contention timing can differ.
    ///
    /// Cancellation is checked before acquisitions, during waits at intervals of at most 20 ms
    /// excluding scheduling/filesystem delays, and after preparation before publication. Callers
    /// choosing [`LockWait::UntilCancelled`] must arrange cancellation if an unbounded wait is
    /// unacceptable. Cancellation does not interrupt publication once it starts.
    ///
    /// Uses the same locked rechecks, expected values, cleanup and publication as
    /// [`Self::transaction`]. It never refreshes preconditions or retries preparation/publication.
    /// No configuration or environment is read. Existing transaction methods still fail
    /// immediately on contention.
    ///
    /// # Errors
    ///
    /// Reftable returns [`TransactionError::Prepare`] with [`ReferenceError::Unsupported`] before
    /// effects. Cancellation, timeout and other preparation failures preserve ref/log contents.
    /// Publication failures retain their effects in [`TransactionError::Publish`]; never retry
    /// them without inspecting current state.
    ///
    /// ```no_run
    /// use std::sync::atomic::AtomicBool;
    /// use std::time::Duration;
    /// use girt::refs::{FilesTransactionOptions, LockWait};
    /// # fn example(repo: &girt::Repository, edits: &[girt::refs::RefEdit]) -> Result<(), Box<dyn std::error::Error>> {
    /// let options = FilesTransactionOptions {
    ///     reference_lock_wait: LockWait::For(Duration::from_millis(100)),
    ///     packed_refs_lock_wait: LockWait::For(Duration::from_secs(1)),
    /// };
    /// repo.references()?.transaction_files_with_options(edits, options, &AtomicBool::new(false))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn transaction_files_with_options(
        &self,
        edits: &[RefEdit],
        options: FilesTransactionOptions,
        cancel: &AtomicBool,
    ) -> Result<Vec<RefEditOutcome>, TransactionError> {
        self.transaction_files_with_log_identity(edits, options, cancel, LogIdentity::Resolved)
    }

    /// Applies a files-only transaction whose reflogs identify stored values without resolving
    /// symbolic references.
    ///
    /// Every edit must have `dereference: false` and a direct replacement or deletion. The old
    /// reflog ID is the stored direct ID, or null for a symbolic or absent value; the new ID is the
    /// replacement ID, or null for deletion. Symbolic chains are never traversed or locked. Only
    /// the edited name is rechecked under its reference lock, and the caller's exact expected
    /// value remains authoritative. Conditional append policies compare stored targets.
    ///
    /// Uses the same packed-reference coordination, reflog selection, lock waits, cancellation,
    /// cleanup and publication outcomes as [`Self::transaction_files_with_options`]. In particular,
    /// existing-log selection happens under the log lock, and cancellation cannot interrupt
    /// publication once it starts. No caller-supplied reflog IDs are accepted.
    ///
    /// # Errors
    ///
    /// Rejects reftable, dereferencing edits and symbolic replacements before acquiring locks.
    /// All edits are validated before preparation. Preparation errors preserve reference and log
    /// contents; [`TransactionError::Publish`] reports any partial publication and must not be
    /// retried without inspecting current state.
    ///
    /// ```no_run
    /// use std::sync::atomic::AtomicBool;
    /// use girt::refs::FilesTransactionOptions;
    /// # fn example(repo: &girt::Repository, edits: &[girt::refs::RefEdit]) -> Result<(), Box<dyn std::error::Error>> {
    /// repo.references()?.transaction_files_with_stored_log_ids(
    ///     edits, FilesTransactionOptions::default(), &AtomicBool::new(false),
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn transaction_files_with_stored_log_ids(
        &self,
        edits: &[RefEdit],
        options: FilesTransactionOptions,
        cancel: &AtomicBool,
    ) -> Result<Vec<RefEditOutcome>, TransactionError> {
        self.transaction_files_with_log_identity(edits, options, cancel, LogIdentity::Stored)
    }

    fn transaction_files_with_log_identity(
        &self,
        edits: &[RefEdit],
        options: FilesTransactionOptions,
        cancel: &AtomicBool,
        identity: LogIdentity,
    ) -> Result<Vec<RefEditOutcome>, TransactionError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt", "refs.transaction", outcome = "incomplete",
            failure_class = tracing::field::Empty, effects = tracing::field::Empty,
            edits = edits.len(),
        );
        let operation = || {
            if self.reference_backend != super::Backend::Files {
                return Err(TransactionError::Prepare {
                    operation: None,
                    source: ReferenceError::Unsupported(
                        "files transaction options require files backend",
                    ),
                });
            }
            check_cancelled(cancel).map_err(|source| TransactionError::Prepare {
                operation: None,
                source,
            })?;
            if edits.is_empty() {
                return Ok(Vec::new());
            }
            for (index, edit) in edits.iter().enumerate() {
                if identity == LogIdentity::Stored
                    && (edit.dereference || matches!(edit.target, Some(Target::Symbolic(_))))
                {
                    return Err(TransactionError::Prepare {
                        operation: Some(index),
                        source: ReferenceError::Unsupported(
                            "stored log identities require non-dereferencing direct edits or deletions",
                        ),
                    });
                }
                validate_edit(self.object_format, edit).map_err(|source| {
                    TransactionError::Prepare {
                        operation: Some(index),
                        source,
                    }
                })?;
            }
            let prepared =
                self.prepare_files_transaction_with_options(edits, options, cancel, identity)?;
            check_cancelled(cancel).map_err(|source| TransactionError::Prepare {
                operation: None,
                source,
            })?;
            prepared.publish()
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = operation();
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
        if self.reference_backend == super::Backend::Reftable {
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
        self.prepare_files_transaction_with_options(
            edits,
            FilesTransactionOptions::default(),
            &AtomicBool::new(false),
            LogIdentity::Resolved,
        )
    }

    fn prepare_files_transaction_with_options(
        &self,
        edits: &[RefEdit],
        options: FilesTransactionOptions,
        cancel: &AtomicBool,
        identity: LogIdentity,
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
            let packed_lock = Lock::acquire_wait(
                self.common_dir.join("packed-refs"),
                options.packed_refs_lock_wait,
                cancel,
            )
            .map_err(batch_error)?;
            let bytes = read_optional(&packed_lock.destination)
                .map_err(batch_error)?
                .unwrap_or_default();
            let packed = packed::parse(self.object_format, &bytes, &packed_lock.destination)
                .map_err(batch_error)?;
            let mut plans = Vec::new();
            let mut names = BTreeSet::new();
            for (index, edit) in edits.iter().enumerate() {
                let error = |source| TransactionError::Prepare {
                    operation: Some(index),
                    source,
                };
                let chain = self
                    .discover_chain(edit, &packed, identity)
                    .map_err(error)?;
                let new_chain = if let Some(Target::Symbolic(target)) = &edit.target
                    && edit.reflog.append_fields().is_some()
                {
                    let dependency = RefEdit {
                        name: target.clone(),
                        dereference: true,
                        target: None,
                        expected: Expected::Any,
                        reflog: Reflog::Preserve,
                    };
                    let new_chain = self
                        .discover_chain(&dependency, &packed, LogIdentity::Resolved)
                        .map_err(error)?;
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
                    Lock::acquire_wait(
                        self.path(&name).map_err(batch_error)?,
                        options.reference_lock_wait,
                        cancel,
                    )
                    .map_err(batch_error)?,
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
                let logged_chain = if edit.dereference {
                    &chain[..]
                } else {
                    &chain[..1]
                };
                if edit.reflog.append_fields().is_some() {
                    log_names.extend(logged_chain.iter().map(|(name, _)| name.clone()));
                }
                let skip_log = matches!(
                    edit.reflog,
                    Reflog::AppendIfChanged { .. } | Reflog::AppendExistingIfChanged { .. }
                ) && actual == &edit.target;
                if let Some((committer, message)) = edit.reflog.append_fields()
                    && !skip_log
                {
                    // Resolved mode includes the terminal old value; stored mode has only the
                    // edited name. Every discovered value was rechecked under its lock.
                    let old = log_id(
                        self.object_format,
                        chain.last().unwrap().1.as_ref(),
                        identity,
                    )
                    .map_err(error)?;
                    let new_target = new_chain
                        .last()
                        .map_or(edit.target.as_ref(), |(_, value)| value.as_ref());
                    let new = log_id(self.object_format, new_target, identity).map_err(error)?;
                    let record = ReflogEntry {
                        old,
                        new,
                        committer: committer.clone(),
                        message: message.to_vec(),
                    };
                    let record = record.encode().map_err(error)?;
                    for (log_name, _) in logged_chain {
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
                let lock =
                    Lock::acquire_wait(path, LockWait::Immediate, cancel).map_err(batch_error)?;
                log_locks.insert(name, lock);
            }
            for (operation, edit) in operations.iter_mut().zip(edits) {
                if matches!(edit.reflog, Reflog::AppendExistingIfChanged { .. }) {
                    // Presence is a transaction decision, never a caller's pre-read.
                    let mut existing = Vec::new();
                    for (name, record) in std::mem::take(&mut operation.logs) {
                        if self.has_reflog(&name).map_err(batch_error)? {
                            existing.push((name, record));
                        }
                    }
                    operation.logs = existing;
                }
                if !operation.delete_log {
                    for (name, _) in &operation.logs {
                        reflog::check_append_tail(&log_locks[name].destination)
                            .map_err(batch_error)?;
                    }
                }
            }
            let mut replacement = bytes;
            for operation in &operations {
                if operation.packed_removed {
                    replacement =
                        packed::without_ref(self.object_format, &replacement, &operation.name);
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
        identity: LogIdentity,
    ) -> Result<Vec<(RefName, Option<Target>)>, ReferenceError> {
        let resolve_old = identity == LogIdentity::Resolved
            && (edit.dereference || edit.reflog.append_fields().is_some());
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
    if let Some((committer, message)) = edit.reflog.append_fields() {
        reflog::validate(committer, message)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LogIdentity {
    Resolved,
    Stored,
}

fn log_id(
    format: crate::ObjectFormat,
    target: Option<&Target>,
    identity: LogIdentity,
) -> Result<ObjectId, ReferenceError> {
    match target {
        Some(Target::Symbolic(_)) if identity == LogIdentity::Stored => Ok(ObjectId::null(format)),
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
