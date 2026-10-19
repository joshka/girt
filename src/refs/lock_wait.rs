//! Caller-selected waits for files transaction lock acquisition.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::ReferenceError;
use super::store::Lock;

/// How long to retry one contended files transaction lock.
///
/// Each acquisition starts a fresh monotonic budget. No lock is stolen. Scheduling and filesystem
/// operations can exceed a finite budget; waits do not impose a deadline on the whole transaction.
///
/// Choose a finite wait for interactive operations or explicit cancellation for an unbounded
/// acquisition. Each lock gets its own budget, so this is not a deadline for the entire
/// transaction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum LockWait {
    /// Try once without sleeping.
    #[default]
    Immediate,
    /// Retry contention for this duration. Zero is equivalent to [`Self::Immediate`].
    For(Duration),
    /// Retry until acquisition succeeds or the caller's cancellation flag is set.
    UntilCancelled,
}

/// Bounds for opt-in files reference transaction preparation.
///
/// Defaults admit 10,000 edits and one million packed records, with 128 MiB of aggregate metadata
/// input and 512 MiB of admitted/constructed preparation data. Counts bound transaction plans;
/// byte budgets bound file contents, edit payloads and buffers. These are cooperative work bounds,
/// not a filesystem deadline or a resident-memory measurement. Symbolic chains keep their existing
/// depth limit. Publication is not interrupted after it starts.
///
/// This type is `Copy`; future fields must also be `Copy`. Set these bounds through
/// [`FilesTransactionOptions::with_limits`]. Existing options do not impose these limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct FilesTransactionLimits {
    /// Maximum number of caller edits, admitted before lock acquisition.
    pub max_edits: usize,
    /// Maximum packed reference records, including records retained in a cached snapshot.
    pub max_packed_refs: usize,
    /// Aggregate bytes admitted from packed refs, loose/symbolic rereads and reflog append tails.
    pub max_input_bytes: usize,
    /// Aggregate bytes admitted from edit fields and constructed during preparation.
    ///
    /// Includes names, targets, reflog encoding/copies and temporary packed replacements. Repeated
    /// construction is charged repeatedly; conservative encoding allowances can refuse before the
    /// final representation would exhaust the budget. Counts describe logical reference data;
    /// collection and filesystem-path overhead additionally scale with edit count and chain depth.
    /// No preparation refusal publishes a ref/log.
    pub max_prepared_bytes: usize,
}

impl Default for FilesTransactionLimits {
    fn default() -> Self {
        Self {
            max_edits: 10_000,
            max_packed_refs: 1_000_000,
            max_input_bytes: 128 * 1024 * 1024,
            max_prepared_bytes: 512 * 1024 * 1024,
        }
    }
}

impl FilesTransactionLimits {
    /// Sets [`Self::max_edits`].
    #[must_use]
    pub fn with_max_edits(mut self, maximum: usize) -> Self {
        self.max_edits = maximum;
        self
    }

    /// Sets [`Self::max_packed_refs`].
    #[must_use]
    pub fn with_max_packed_refs(mut self, maximum: usize) -> Self {
        self.max_packed_refs = maximum;
        self
    }

    /// Sets [`Self::max_input_bytes`].
    #[must_use]
    pub fn with_max_input_bytes(mut self, maximum: usize) -> Self {
        self.max_input_bytes = maximum;
        self
    }

    /// Sets [`Self::max_prepared_bytes`].
    #[must_use]
    pub fn with_max_prepared_bytes(mut self, maximum: usize) -> Self {
        self.max_prepared_bytes = maximum;
        self
    }
}

/// Lock waits for a files reference transaction; both default to immediate acquisition.
///
/// Reference locks include HEAD, symbolic dependencies and placeholders. Reflog locks always fail
/// immediately on contention. These options neither load Git configuration nor apply to reftable.
///
/// Use this entry point to choose waits rather than changing process-global settings. The type
/// default tries immediately, while [`super::References::transaction`] uses
/// [`FilesTransactionOptions::GIT_DEFAULT`]; reflog contention
/// remains immediate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct FilesTransactionOptions {
    /// Wait for each loose reference lock independently.
    pub reference_lock_wait: LockWait,
    /// Wait for the common `packed-refs.lock`.
    pub packed_refs_lock_wait: LockWait,
    pub(super) limits: Option<FilesTransactionLimits>,
}

impl FilesTransactionOptions {
    /// Applies metadata and preparation bounds to this files transaction.
    ///
    /// Limits and cancellation are enforced before publication, including on cached packed refs
    /// and locked loose rereads. Ordinary options leave this policy unset. Lock waits retain
    /// their independent budgets; publication retains its existing partial-effect contract.
    #[must_use]
    pub fn with_limits(mut self, limits: FilesTransactionLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// Sets [`Self::reference_lock_wait`].
    #[must_use]
    pub fn with_reference_lock_wait(mut self, reference_lock_wait: LockWait) -> Self {
        self.reference_lock_wait = reference_lock_wait;
        self
    }

    /// Sets [`Self::packed_refs_lock_wait`].
    #[must_use]
    pub fn with_packed_refs_lock_wait(mut self, packed_refs_lock_wait: LockWait) -> Self {
        self.packed_refs_lock_wait = packed_refs_lock_wait;
        self
    }
}

impl FilesTransactionOptions {
    /// Git's default waits: 100 ms for each loose reference lock (`core.filesRefLockTimeout`)
    /// and one second for `packed-refs.lock` (`core.packedRefsTimeout`).
    pub const GIT_DEFAULT: Self = Self {
        reference_lock_wait: LockWait::For(Duration::from_millis(100)),
        packed_refs_lock_wait: LockWait::For(Duration::from_secs(1)),
        limits: None,
    };
}

pub(super) fn check_cancelled(cancel: &AtomicBool) -> Result<(), ReferenceError> {
    if cancel.load(Ordering::Relaxed) {
        Err(ReferenceError::Cancelled)
    } else {
        Ok(())
    }
}

impl Lock {
    pub(super) fn acquire_wait(
        destination: PathBuf,
        wait: LockWait,
        cancel: &AtomicBool,
    ) -> Result<Self, ReferenceError> {
        let generation = crate::locks::wait_generation();
        let started = Instant::now();
        let mut delay = Duration::from_millis(1);
        loop {
            check_cancelled(cancel)?;
            if crate::locks::wait_generation() != generation {
                return Err(ReferenceError::Cancelled);
            }
            let error = match Self::acquire(destination.clone()) {
                Ok(lock) => {
                    if crate::locks::wait_generation() != generation {
                        return Err(ReferenceError::Cancelled);
                    }
                    return Ok(lock);
                }
                Err(error @ ReferenceError::Locked(_)) => error,
                Err(error) => return Err(error),
            };
            let sleep = match wait {
                LockWait::Immediate => return Err(error),
                LockWait::For(budget) => {
                    let remaining = budget.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        return Err(error);
                    }
                    delay.min(remaining)
                }
                LockWait::UntilCancelled => delay,
            };
            std::thread::sleep(sleep);
            check_cancelled(cancel)?;
            if crate::locks::wait_generation() != generation {
                return Err(ReferenceError::Cancelled);
            }
            if let LockWait::For(budget) = wait
                && started.elapsed() >= budget
            {
                return Err(error);
            }
            delay = (delay * 2).min(Duration::from_millis(20));
        }
    }
}
