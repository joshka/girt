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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LockWait {
    /// Try once without sleeping.
    #[default]
    Immediate,
    /// Retry contention for this duration. Zero is equivalent to [`Self::Immediate`].
    For(Duration),
    /// Retry until acquisition succeeds or the caller's cancellation flag is set.
    UntilCancelled,
}

/// Lock waits for a files reference transaction; both default to immediate acquisition.
///
/// Reference locks include HEAD, symbolic dependencies and placeholders. Reflog locks always fail
/// immediately on contention. These options neither load Git configuration nor apply to reftable.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FilesTransactionOptions {
    /// Wait for each loose reference lock independently.
    pub reference_lock_wait: LockWait,
    /// Wait for the common `packed-refs.lock`.
    pub packed_refs_lock_wait: LockWait,
}

impl FilesTransactionOptions {
    /// Git's default waits: 100 ms for each loose reference lock (`core.filesRefLockTimeout`)
    /// and one second for `packed-refs.lock` (`core.packedRefsTimeout`).
    pub const GIT_DEFAULT: Self = Self {
        reference_lock_wait: LockWait::For(Duration::from_millis(100)),
        packed_refs_lock_wait: LockWait::For(Duration::from_secs(1)),
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
        let started = Instant::now();
        let mut delay = Duration::from_millis(1);
        loop {
            check_cancelled(cancel)?;
            let error = match Self::acquire(destination.clone()) {
                Ok(lock) => return Ok(lock),
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
            if let LockWait::For(budget) = wait
                && started.elapsed() >= budget
            {
                return Err(error);
            }
            delay = (delay * 2).min(Duration::from_millis(20));
        }
    }
}
