use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::sync::atomic::AtomicBool;

use super::lock_wait::check_cancelled;
use super::store::{Lock, check_expected, io_error, remove_loose, validate_target};
use super::{
    Expected, FilesTransactionOptions, LockWait, RefName, ReferenceError, References, Reflog,
    ReflogEntry, Target, packed, reflog,
};
use crate::ObjectId;

mod control;
use control::Control;

use super::FilesTransactionLimits;

/// Which stored reference a conditional edit updates.
///
/// Choose [`Self::Stored`] when replacing the named symbolic spelling itself, and [`Self::Follow`]
/// when advancing its
/// terminal destination while preserving aliases. Preconditions and logging scope follow this
/// choice; changing it is not a mere convenience.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum Dereference {
    /// Edit the named reference itself.
    Stored,
    /// Follow symbolic references and edit the terminal stored reference.
    Follow,
}

/// A conditional change to one stored name or one symbolic chain's terminal name.
///
/// ```
/// # use girt::refs::{Expected, RefEdit, RefName, Reflog, Target};
/// # use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};
/// # let directory = tempfile::tempdir()?;
/// # let repo = Repository::init(ObjectFormat::Sha1, directory.path().join("r.git"), InitKind::Bare)?;
/// # let loose = repo.loose_objects();
/// # let person = Signature { name: b"A. Writer".to_vec(), email: b"a@example.com".to_vec(),
/// #                          seconds: 1_700_000_000, offset_minutes: 0 };
/// # let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
/// # let id = loose.write_commit(&Commit::new(CommitFields {
/// #     tree, parents: vec![], author: person.clone(), committer: person.clone(),
/// #     extra_headers: vec![], message: b"Initial snapshot\n".to_vec(),
/// # })?)?;
/// # let refs = repo.references()?;
/// # let main = RefName::new("refs/heads/main")?;
/// let head = RefName::HEAD;
/// let edit = RefEdit::new(head.clone(), Some(Target::Direct(id)), Expected::Absent,
///     Reflog::Append { committer: person, message: b"commit (initial)".to_vec() })
///     .with_dereference(girt::refs::Dereference::Follow);
/// refs.transaction(&[edit])?;
/// assert_eq!(refs.read(&head)?, Some(Target::Symbolic(main.clone())));
/// assert_eq!(refs.resolve(&head)?.id, Some(id));
/// assert_eq!(refs.reflog(&head)?.ok_or("HEAD log missing")?.len(), 1);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RefEdit {
    /// Initial name; also the destination when `dereference` is `Stored`.
    pub name: RefName,
    /// Follow up to 32 symbolic hops, keeping the chain unchanged.
    ///
    /// Only direct updates or deletion can be dereferenced. Preconditions apply to the terminal
    /// stored value. With `Stored`, a direct replacement with append logging still locks and
    /// resolves the old symbolic chain for the log identity, but edits/logs only `name` and checks
    /// the precondition against its stored value. Symbolic replacement locks both old and new
    /// chains to record their terminal IDs. Overlapping operations are rejected.
    pub dereference: Dereference,
    /// Replacement, or `None` to delete. Object existence/type is not checked.
    pub target: Option<Target>,
    /// Compared under locks before any publication.
    pub expected: Expected,
    /// Explicit history policy.
    pub reflog: Reflog,
}

impl RefEdit {
    /// Creates an edit of the stored name with explicit precondition and history policy.
    ///
    /// A `target` of `None` deletes the reference; `Some` replaces its stored target.
    /// See [`RefEdit`] for publication through HEAD and [`References::transaction`] for an
    /// expected-value update. Construction alone neither locks nor writes storage.
    pub fn new(name: RefName, target: Option<Target>, expected: Expected, reflog: Reflog) -> Self {
        Self {
            name,
            target,
            expected,
            reflog,
            dereference: crate::refs::Dereference::Stored,
        }
    }

    /// Creates `name` pointing at `id`, requiring that it doesn't exist yet.
    ///
    /// Equivalent to `RefEdit::new(name, Some(Target::Direct(id)), Expected::Absent, reflog)`,
    /// like `git update-ref <name> <id> ""`. Pass [`Repository::reflog_for`] as `reflog` to log
    /// as Git would by default. Use [`Self::new`] for a symbolic target.
    ///
    /// [`Repository::reflog_for`]: crate::Repository::reflog_for
    ///
    /// ```
    /// use girt::refs::{RefEdit, RefName, Reflog, Target};
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let id = repo.loose_objects().write_blob(b"release notes")?;
    /// let refs = repo.references()?;
    /// let tag = RefName::tag("notes")?;
    /// refs.transaction(&[RefEdit::create(tag.clone(), id, Reflog::Preserve)])?;
    /// assert_eq!(refs.read(&tag)?, Some(Target::Direct(id)));
    ///
    /// // A second create fails its precondition instead of replacing the tag.
    /// assert!(
    ///     refs.transaction(&[RefEdit::create(tag, id, Reflog::Preserve)])
    ///         .is_err()
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn create(name: RefName, id: ObjectId, reflog: Reflog) -> Self {
        Self::new(name, Some(Target::Direct(id)), Expected::Absent, reflog)
    }

    /// Moves `name` from `old` to `new`, requiring that it still points directly at `old`.
    ///
    /// Equivalent to
    /// `RefEdit::new(name, Some(Target::Direct(new)), Expected::Value(Target::Direct(old)),
    /// reflog)`, like `git update-ref <name> <new> <old>`. Note the argument order: `old` comes
    /// first here. This is a compare-and-swap, not a fast-forward check. A symbolic `name`
    /// fails the precondition; add [`Dereference::Follow`] with [`Self::with_dereference`] to
    /// move the branch it names.
    ///
    /// ```
    /// use girt::refs::{RefEdit, RefName, Reflog, Target};
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let loose = repo.loose_objects();
    /// let (first, second) = (loose.write_blob(b"one")?, loose.write_blob(b"two")?);
    ///
    /// let refs = repo.references()?;
    /// let branch = RefName::branch("topic")?;
    /// refs.transaction(&[RefEdit::create(branch.clone(), first, Reflog::Preserve)])?;
    /// refs.transaction(&[RefEdit::update(
    ///     branch.clone(),
    ///     first,
    ///     second,
    ///     Reflog::Preserve,
    /// )])?;
    /// assert_eq!(refs.read(&branch)?, Some(Target::Direct(second)));
    ///
    /// // `first` is stale now, so repeating the move fails without changing the branch.
    /// assert!(
    ///     refs.transaction(&[RefEdit::update(branch, first, second, Reflog::Preserve)])
    ///         .is_err()
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn update(name: RefName, old: ObjectId, new: ObjectId, reflog: Reflog) -> Self {
        Self::new(
            name,
            Some(Target::Direct(new)),
            Expected::Value(Target::Direct(old)),
            reflog,
        )
    }

    /// Deletes `name`, requiring that it still points directly at `old`.
    ///
    /// Equivalent to `RefEdit::new(name, None, Expected::Value(Target::Direct(old)), reflog)`, like
    /// `git update-ref -d <name> <old>`. Git deletes a branch's reflog with the branch; pass
    /// [`Reflog::Delete`] for that, or [`Reflog::Preserve`] to keep any log.
    ///
    /// ```
    /// use girt::refs::{RefEdit, RefName, Reflog};
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let id = repo.loose_objects().write_blob(b"draft")?;
    /// let refs = repo.references()?;
    /// let branch = RefName::branch("draft")?;
    /// refs.transaction(&[RefEdit::create(branch.clone(), id, Reflog::Preserve)])?;
    /// refs.transaction(&[RefEdit::delete(branch.clone(), id, Reflog::Delete)])?;
    /// assert_eq!(refs.read(&branch)?, None);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn delete(name: RefName, old: ObjectId, reflog: Reflog) -> Self {
        Self::new(name, None, Expected::Value(Target::Direct(old)), reflog)
    }

    /// Sets [`Self::name`].
    #[must_use]
    pub fn with_name(mut self, name: RefName) -> Self {
        self.name = name;
        self
    }

    /// Sets [`Self::dereference`].
    #[must_use]
    pub fn with_dereference(mut self, dereference: Dereference) -> Self {
        self.dereference = dereference;
        self
    }

    /// Sets [`Self::target`].
    #[must_use]
    pub fn with_target(mut self, target: Option<Target>) -> Self {
        self.target = target;
        self
    }

    /// Sets [`Self::expected`].
    #[must_use]
    pub fn with_expected(mut self, expected: Expected) -> Self {
        self.expected = expected;
        self
    }

    /// Sets [`Self::reflog`].
    #[must_use]
    pub fn with_reflog(mut self, reflog: Reflog) -> Self {
        self.reflog = reflog;
        self
    }
}

/// Reference publication state at the time a transaction returns.
///
/// Use this state with the matching log outcomes to diagnose publication. [`Self::Published`]
/// describes visibility, not power-loss durability; [`Self::PackedRemoved`] can already mean an
/// absent packed-only reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
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
///
/// Inspect this state independently of reference publication. An advanced ref can have a failed or
/// partial log record; replaying the transaction can duplicate history rather than repair it.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
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
///
/// Retain these receipts after publication failure. Input order identifies the intended edits,
/// while each reference and log state describes actual effects; obtain fresh observations before
/// retrying.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
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
///
/// [`Self::Prepare`] leaves ref and log contents unchanged. [`Self::Publish`] retains exact
/// outcomes without rollback; a reference may have advanced while its reflog append failed.
/// Retain the outcomes in diagnostics and read fresh state before designing a retry. Replaying
/// the same batch blindly can overwrite intervening work or duplicate log entries.
///
/// ```
/// use girt::refs::{
///     Expected, RefEdit, RefName, RefOutcome, ReferenceError, Reflog, Target, TransactionError,
/// };
/// use girt::{InitKind, ObjectFormat, Repository};
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r.git"),
///     InitKind::Bare,
/// )?;
///
/// let refs = repo.references()?;
/// let name = RefName::new("refs/tags/result")?;
/// let id = repo.loose_objects().write_blob(b"build result")?;
///
/// let edit = RefEdit::new(
///     name.clone(),
///     Some(Target::Direct(id)),
///     Expected::Absent,
///     Reflog::Preserve,
/// );
///
/// let outcomes = refs.transaction(&[edit.clone()])?;
/// assert_eq!(outcomes[0].reference, RefOutcome::Published);
/// assert!(outcomes[0].logs.is_empty());
///
/// // A stale creation is refused before publication; don't replace it with Expected::Any.
/// assert!(matches!(
///     refs.transaction(&[edit]),
///     Err(TransactionError::Prepare {
///         operation: Some(0),
///         source: ReferenceError::Mismatch { .. },
///     })
/// ));
/// assert_eq!(refs.read(&name)?, Some(Target::Direct(id)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransactionError {
    /// No reference or log contents were changed. Empty directories can remain.
    #[error("reference transaction preparation failed")]
    Prepare {
        /// Input index when the failure belongs to one operation; otherwise a batch/lock failure.
        operation: Option<usize>,
        /// Validation, precondition, lock, or read failure.
        #[source]
        source: ReferenceError,
    },
    /// Publication stopped at the first error, without rollback.
    #[error("reference transaction publication failed")]
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
    /// including delete/create namespace swaps. Existing packed conflicts are rejected. Contended
    /// locks are retried for Git's default durations ([`FilesTransactionOptions::GIT_DEFAULT`]);
    /// locks are never stolen. A changed chain fails rather than being retried.
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
    ///
    /// ```
    /// # use girt::refs::{Expected, RefEdit, RefName, Reflog, Target};
    /// # use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};
    /// # let directory = tempfile::tempdir()?;
    /// # let repo = Repository::init(ObjectFormat::Sha1, directory.path().join("r.git"), InitKind::Bare)?;
    /// # let loose = repo.loose_objects();
    /// # let person = Signature { name: b"A. Writer".to_vec(), email: b"a@example.com".to_vec(),
    /// #                          seconds: 1_700_000_000, offset_minutes: 0 };
    /// # let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
    /// # let id = loose.write_commit(&Commit::new(CommitFields {
    /// #     tree, parents: vec![], author: person.clone(), committer: person.clone(),
    /// #     extra_headers: vec![], message: b"Initial snapshot\n".to_vec(),
    /// # })?)?;
    /// # let refs = repo.references()?;
    /// # let main = RefName::new("refs/heads/main")?;
    /// # let edit = RefEdit::new(main.clone(), Some(Target::Direct(id)), Expected::Absent,
    /// #     Reflog::Append { committer: person, message: b"commit (initial)".to_vec() });
    /// # refs.transaction(&[edit])?;
    /// // Publish a child only if the branch still has the stored value we observed.
    /// let observed = refs.read(&main)?.ok_or("branch missing")?;
    ///
    /// let mut fields = loose.read_commit(id, 4096)?.to_fields()?;
    /// fields.parents = vec![id];
    /// fields.message = b"Next snapshot\n".to_vec();
    /// let next = loose.write_commit(&Commit::new(fields)?)?;
    ///
    /// let advance = RefEdit::new(main.clone(), Some(Target::Direct(next)), Expected::Value(observed),
    ///     Reflog::Append { committer: Signature { name: b"A".to_vec(), email: b"a@example.com".to_vec(),
    ///         seconds: 1_700_000_001, offset_minutes: 0 }, message: b"commit: next".to_vec() });
    /// refs.transaction(&[advance])?;
    /// assert_eq!(refs.read(&main)?, Some(Target::Direct(next)));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
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
    /// Add [`FilesTransactionOptions::with_limits`] to bound packed snapshots, aggregate loose and
    /// symbolic rereads, reflog append-tail reads and preparation buffers. Cached packed data still
    /// consumes its retained bytes/record count. Preparation accounting conservatively includes
    /// edit payloads and constructed copies. Parsing/encoding one admitted file or record, and
    /// rewriting one admitted packed snapshot, remain synchronous bounded phases. Namespace
    /// scans poll once per batch; cancellation cannot interrupt filesystem operations or
    /// publication.
    ///
    /// Uses the same locked rechecks, expected values, cleanup and publication as
    /// [`Self::transaction`]. It never refreshes preconditions or retries preparation/publication.
    /// No configuration or environment is read. The ordinary transaction entry point uses
    /// [`FilesTransactionOptions::GIT_DEFAULT`]; no-reflog single-name operations fail immediately.
    ///
    /// # Errors
    ///
    /// Reftable returns [`TransactionError::Prepare`] with [`ReferenceError::Unsupported`] before
    /// effects. Cancellation, timeout and other preparation failures preserve ref/log contents.
    /// Publication failures retain their effects in [`TransactionError::Publish`]; never retry
    /// them without inspecting current state.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::refs::{
    ///     Expected, FilesTransactionLimits, FilesTransactionOptions, RefEdit, RefName, Reflog, Target,
    /// };
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let id = repo.loose_objects().write_blob(b"manifest")?;
    /// let name = RefName::new("refs/tags/exported")?;
    /// let edits = [RefEdit::new(
    ///     name.clone(),
    ///     Some(Target::Direct(id)),
    ///     Expected::Absent,
    ///     Reflog::Preserve,
    /// )];
    ///
    /// let options = FilesTransactionOptions::GIT_DEFAULT
    ///     .with_reference_lock_wait(girt::refs::LockWait::For(std::time::Duration::from_millis(
    ///         25,
    ///     )))
    ///     .with_limits(FilesTransactionLimits::default().with_max_edits(16));
    /// // Shorten loose-ref waits, retain the packed wait, and bound preparation.
    /// let outcomes = repo.references()?.transaction_files_with_options(
    ///     &edits,
    ///     options,
    ///     &AtomicBool::new(false),
    /// )?;
    /// assert_eq!(outcomes.len(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
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
    /// Every edit must use [`Dereference::Stored`] and a direct
    /// replacement or deletion. The old reflog ID is the stored direct ID, or null for a
    /// symbolic or absent value; the new ID is the replacement ID, or null for deletion.
    /// Symbolic chains are never traversed or locked. Only the edited name is rechecked under
    /// its reference lock, and the caller's exact expected value remains authoritative.
    /// Conditional append policies compare stored targets.
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
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::refs::{Expected, FilesTransactionOptions, RefEdit, RefName, Reflog, Target};
    /// use girt::{InitKind, ObjectFormat, Repository, Signature};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let old = repo.loose_objects().write_blob(b"old")?;
    /// let new = repo.loose_objects().write_blob(b"replacement")?;
    ///
    /// let refs = repo.references()?;
    /// let base = RefName::new("refs/tags/base")?;
    /// let alias = RefName::new("refs/tags/alias")?;
    /// refs.update_without_reflog(&base, Target::Direct(old), Expected::Absent)?;
    /// refs.update_without_reflog(&alias, Target::Symbolic(base.clone()), Expected::Absent)?;
    ///
    /// let edit = RefEdit::new(
    ///     alias.clone(),
    ///     Some(Target::Direct(new)),
    ///     Expected::Value(Target::Symbolic(base.clone())),
    ///     Reflog::Append {
    ///         committer: Signature {
    ///             name: b"A".to_vec(),
    ///             email: b"a@example.com".to_vec(),
    ///             seconds: 1_700_000_000,
    ///             offset_minutes: 0,
    ///         },
    ///         message: b"replace alias".to_vec(),
    ///     },
    /// );
    ///
    /// let outcomes = refs.transaction_files_with_stored_log_ids(
    ///     &[edit],
    ///     FilesTransactionOptions::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    /// assert_eq!(outcomes[0].name, alias);
    ///
    /// let log = refs.reflog(&alias)?.ok_or("log absent")?;
    /// assert!(log[0].old.is_null()); // The stored old value was symbolic, not the resolved `old` ID.
    /// assert_eq!(log[0].new, new);
    /// assert_eq!(refs.read(&base)?, Some(Target::Direct(old))); // The target was never edited.
    ///
    /// # Ok::<(), Box<dyn std::error::Error>>(())
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
        if let Some(limits) = options.limits {
            self.transaction_files_with_log_identity_with::<true>(
                edits, options, cancel, identity, limits,
            )
        } else {
            self.transaction_files_with_log_identity_with::<false>(
                edits,
                options,
                cancel,
                identity,
                FilesTransactionLimits::default(),
            )
        }
    }

    fn transaction_files_with_log_identity_with<const CONTROLLED: bool>(
        &self,
        edits: &[RefEdit],
        options: FilesTransactionOptions,
        cancel: &AtomicBool,
        identity: LogIdentity,
        limits: FilesTransactionLimits,
    ) -> Result<Vec<RefEditOutcome>, TransactionError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt", "refs.transaction", outcome = "incomplete",
            failure_class = tracing::field::Empty, effects = tracing::field::Empty,
            edits = edits.len(),
        );
        let operation = || {
            let mut control = Control::<CONTROLLED>::new(limits, cancel);
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
            control
                .edits(edits)
                .map_err(|source| TransactionError::Prepare {
                    operation: None,
                    source,
                })?;
            if edits.is_empty() {
                return Ok(Vec::new());
            }
            for (index, edit) in edits.iter().enumerate() {
                control.step().map_err(|source| TransactionError::Prepare {
                    operation: Some(index),
                    source,
                })?;
                if identity == LogIdentity::Stored
                    && (edit.dereference == crate::refs::Dereference::Follow
                        || matches!(edit.target, Some(Target::Symbolic(_))))
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
            let prepared = self.prepare_files_transaction_with_control(
                edits,
                options,
                cancel,
                identity,
                &mut control,
            )?;
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
            FilesTransactionOptions::GIT_DEFAULT,
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
        let mut control = Control::<false>::new(FilesTransactionLimits::default(), cancel);
        self.prepare_files_transaction_with_control(edits, options, cancel, identity, &mut control)
    }

    fn prepare_files_transaction_with_control<const CONTROLLED: bool>(
        &self,
        edits: &[RefEdit],
        options: FilesTransactionOptions,
        cancel: &AtomicBool,
        identity: LogIdentity,
        control: &mut Control<'_, CONTROLLED>,
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
            let packed = control
                .packed(self, &packed_lock.destination)
                .map_err(batch_error)?;
            let mut plans = Vec::new();
            let mut names = BTreeSet::new();
            for (index, edit) in edits.iter().enumerate() {
                control.step().map_err(batch_error)?;
                let error = |source| TransactionError::Prepare {
                    operation: Some(index),
                    source,
                };
                let chain = self
                    .discover_chain(edit, &packed, identity, control)
                    .map_err(error)?;
                let new_chain = if let Some(Target::Symbolic(target)) = &edit.target
                    && edit.reflog.append_fields().is_some()
                {
                    control.name(target).map_err(error)?;
                    let dependency = RefEdit {
                        name: target.clone(),
                        dereference: crate::refs::Dereference::Follow,
                        target: None,
                        expected: Expected::Any,
                        reflog: Reflog::Preserve,
                    };
                    let new_chain = self
                        .discover_chain(&dependency, &packed, LogIdentity::Resolved, control)
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
                    control.step().map_err(error)?;
                    control.name(name).map_err(error)?;
                    if !names.insert(name.clone())
                        || control.overlap(&names, name).map_err(error)?
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
                if CONTROLLED {
                    control
                        .namespace(self, &name, &packed)
                        .map_err(batch_error)?;
                } else {
                    self.check_packed_namespace(&name, &packed, || Ok(()))
                        .map_err(batch_error)?;
                }
                control.name(&name).map_err(batch_error)?;
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
                    control.step().map_err(error)?;
                    control.target(observed.as_ref()).map_err(error)?;
                    check_expected(
                        control.read_locked(self, name, &packed).map_err(error)?,
                        match observed {
                            Some(target) => Expected::Value(target.clone()),
                            None => Expected::Absent,
                        },
                    )
                    .map_err(error)?;
                }
                let (name, actual) = if edit.dereference == crate::refs::Dereference::Follow {
                    chain.last().unwrap()
                } else {
                    chain.first().unwrap()
                };
                control.target(actual.as_ref()).map_err(error)?;
                control.expected(&edit.expected).map_err(error)?;
                check_expected(actual.clone(), edit.expected.clone()).map_err(error)?;
                let mut logs = Vec::new();
                let logged_chain = if edit.dereference == crate::refs::Dereference::Follow {
                    &chain[..]
                } else {
                    &chain[..1]
                };
                if edit.reflog.append_fields().is_some() {
                    if CONTROLLED {
                        for (name, _) in logged_chain {
                            control.name(name).map_err(error)?;
                        }
                    }
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
                    control.reflog(committer, message).map_err(error)?;
                    let record = ReflogEntry {
                        old,
                        new,
                        committer: committer.clone(),
                        message: message.to_vec(),
                    };
                    let record = record.encode().map_err(error)?;
                    for (log_name, _) in logged_chain {
                        control.name(log_name).map_err(error)?;
                        control.prepare(record.len()).map_err(error)?;
                        logs.push((log_name.clone(), record.clone()));
                    }
                }
                if edit.reflog == Reflog::Delete {
                    control.name(name).map_err(error)?;
                    control.name(name).map_err(error)?;
                    log_names.insert(name.clone());
                    logs.push((name.clone(), Vec::new()));
                }
                control.name(name).map_err(error)?;
                control.target(edit.target.as_ref()).map_err(error)?;
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
                control.step().map_err(batch_error)?;
                let path = self.reflog_path(&name).map_err(batch_error)?;
                let lock =
                    Lock::acquire_wait(path, LockWait::Immediate, cancel).map_err(batch_error)?;
                log_locks.insert(name, lock);
            }
            for (operation, edit) in operations.iter_mut().zip(edits) {
                control.step().map_err(batch_error)?;
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
                        control
                            .append_tail(&log_locks[name].destination)
                            .map_err(batch_error)?;
                    }
                }
            }
            // Publication rewrites `packed-refs` only when an operation removes a record.
            let mut replacement = Vec::new();
            if operations.iter().any(|operation| operation.packed_removed) {
                control.prepare(packed.bytes.len()).map_err(batch_error)?;
                replacement.clone_from(&packed.bytes);
                for operation in &operations {
                    if operation.packed_removed {
                        control.phase().map_err(batch_error)?;
                        control.prepare(replacement.len()).map_err(batch_error)?;
                        replacement =
                            packed::without_ref(self.object_format, &replacement, &operation.name);
                    }
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

    fn discover_chain<const CONTROLLED: bool>(
        &self,
        edit: &RefEdit,
        packed: &packed::Packed,
        identity: LogIdentity,
        control: &mut Control<'_, CONTROLLED>,
    ) -> Result<Vec<(RefName, Option<Target>)>, ReferenceError> {
        let resolve_old = identity == LogIdentity::Resolved
            && (edit.dereference == crate::refs::Dereference::Follow
                || edit.reflog.append_fields().is_some());
        let mut chain = Vec::new();
        control.name(&edit.name)?;
        let mut name = edit.name.clone();
        loop {
            control.step()?;
            if chain.iter().any(|(seen, _)| seen == &name) {
                return Err(ReferenceError::Cycle(name));
            }
            let target = control.read_locked(self, &name, packed)?;
            control.name(&name)?;
            control.target(target.as_ref())?;
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
        if edit.dereference == crate::refs::Dereference::Follow
            && matches!(target, Target::Symbolic(_))
        {
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
        let _lock_operation = crate::locks::operation();
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
            for lock in std::iter::once(&self.packed_lock)
                .chain(self.locks.values())
                .chain(self.log_locks.values())
            {
                lock.check_owned()
                    .map_err(|source| TransactionError::Publish {
                        outcomes: outcomes.clone(),
                        source,
                    })?;
            }
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
        let _lock_operation = crate::locks::operation();
        match self {
            Self::Files(prepared) => prepared.publish(),
            Self::Reftable(prepared) => prepared.publish(),
        }
    }
}
