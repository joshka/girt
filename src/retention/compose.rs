use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{
    ExpireFailure, ExpireReport, MaintenanceIsolation, PruneFailure, PruneReport, RepackLimits,
    RetentionPolicy, RetireFailure, RetireReport,
};
use crate::Repository;

/// Known completed phases of one isolated maintenance attempt.
#[derive(Clone, Debug, Default)]
pub struct MaintenanceReport {
    /// Reflog expiry completed; an empty report means no records qualified.
    pub expired: Option<ExpireReport>,
    /// A replacement pack was verified and eligible old packs were retired.
    pub retired: Option<RetireReport>,
    /// Old unreachable primary loose objects were swept.
    pub pruned: Option<PruneReport>,
}

/// Why maintenance stopped; a phase error carries that phase's partial effects.
#[derive(Debug, thiserror::Error)]
pub enum MaintenanceCause {
    /// The caller could not hold a repository-wide exclusion boundary.
    #[error("maintenance isolation unavailable")]
    Isolation(#[source] io::Error),
    /// Directory durability for destructive publication is not established on this platform.
    #[error("composed maintenance is unsupported on this platform")]
    UnsupportedPlatform,
    /// Cancellation was observed before a phase began.
    #[error("maintenance cancelled")]
    Cancelled,
    /// Reflog expiry stopped; inspect its partial report and uncertain source or stack.
    #[error(transparent)]
    Expire(Box<ExpireFailure>),
    /// Pack retirement stopped; inspect its visible publication and old-artifact removals.
    #[error(transparent)]
    Retire(Box<RetireFailure>),
    /// Loose pruning stopped; inspect its completed removals and uncertain target.
    #[error(transparent)]
    Prune(Box<PruneFailure>),
}

/// Failure with completed earlier phases and a partial current phase in its cause.
#[derive(Debug, thiserror::Error)]
#[error("maintenance failed")]
pub struct MaintenanceFailure {
    /// The phase that stopped.
    #[source]
    pub cause: MaintenanceCause,
    /// Phases completed before the failure.
    pub report: Box<MaintenanceReport>,
}

impl Repository {
    /// Expires reflogs, replaces old packs, then prunes old unreachable loose objects.
    ///
    /// The caller's [`MaintenanceIsolation`] guard must exclude external Git and girt writers,
    /// old readers and alternate-store dependents for the entire sequence. Each phase opens a
    /// fresh repository view and performs its own complete, bounded scans and generation checks.
    /// Expiry uses the policy cutoffs; pack retirement then retains every remaining history,
    /// protected pack and alternate, regardless of those cutoffs. Loose pruning also retains all
    /// remaining histories. An earlier phase's success does not authorize a later deletion by
    /// itself. A failed phase stops the sequence and reports known earlier effects plus that
    /// phase's partial effects. Retry with a new isolation guard and fresh scans. On non-Unix
    /// platforms the sequence refuses before any publication or deletion.
    ///
    /// # Errors
    ///
    /// Isolation refusal, unsupported platform, cancellation and any phase failure preserve the
    /// completed-phase report and the current phase's partial result when applicable.
    pub fn run_maintenance<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
    ) -> Result<MaintenanceReport, MaintenanceFailure> {
        if cancel.load(Ordering::Relaxed) {
            return Err(failed(
                MaintenanceCause::Cancelled,
                MaintenanceReport::default(),
            ));
        }
        let _guard = isolation.acquire(self).map_err(|error| {
            failed(
                MaintenanceCause::Isolation(error),
                MaintenanceReport::default(),
            )
        })?;
        #[cfg(not(unix))]
        {
            let _ = (policy, limits);
            Err(failed(
                MaintenanceCause::UnsupportedPlatform,
                MaintenanceReport::default(),
            ))
        }
        #[cfg(unix)]
        self.run_maintenance_exclusive(policy, limits, cancel, || {})
    }

    #[cfg(unix)]
    fn run_maintenance_exclusive(
        &self,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
        after_expiry: impl FnOnce(),
    ) -> Result<MaintenanceReport, MaintenanceFailure> {
        let mut report = MaintenanceReport::default();
        let expired = self
            .expire_reflogs_exclusive(policy, cancel)
            .map_err(|error| failed(MaintenanceCause::Expire(Box::new(error)), report.clone()))?;
        report.expired = Some(expired);
        after_expiry();
        if cancel.load(Ordering::Relaxed) {
            return Err(failed(MaintenanceCause::Cancelled, report));
        }
        let retired = self
            .retire_old_packs_exclusive(policy, limits, cancel)
            .map_err(|error| failed(MaintenanceCause::Retire(Box::new(error)), report.clone()))?;
        report.retired = Some(retired);
        if cancel.load(Ordering::Relaxed) {
            return Err(failed(MaintenanceCause::Cancelled, report));
        }
        let pruned = self
            .prune_unreachable_loose_exclusive(policy, cancel)
            .map_err(|error| failed(MaintenanceCause::Prune(Box::new(error)), report.clone()))?;
        report.pruned = Some(pruned);
        Ok(report)
    }
}

fn failed(cause: MaintenanceCause, report: MaintenanceReport) -> MaintenanceFailure {
    MaintenanceFailure {
        cause,
        report: Box::new(report),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn cancellation_after_expiry_reports_completed_prefix() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args([
                    "init",
                    "--bare",
                    "--initial-branch=main",
                    "--template=",
                    root.path().to_str().unwrap(),
                ])
                .output()
                .unwrap()
                .status
                .success()
        );
        let repository = Repository::open(root.path()).unwrap();
        let cancel = AtomicBool::new(false);
        let error = repository
            .run_maintenance_exclusive(
                &RetentionPolicy::default(),
                RepackLimits::default(),
                &cancel,
                || cancel.store(true, Ordering::Relaxed),
            )
            .unwrap_err();
        assert!(matches!(error.cause, MaintenanceCause::Cancelled));
        assert!(error.report.expired.is_some());
        assert!(error.report.retired.is_none());
        assert!(error.report.pruned.is_none());
    }
}
