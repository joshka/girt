use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
use super::consolidate::SelectError;
use super::{
    ExpireFailure, ExpireReport, MaintenanceIsolation, PruneFailure, PruneReport, RepackLimits,
    RetentionError, RetentionPolicy, RetireFailure, RetireReport,
};
use crate::Repository;

/// Known completed phases of one isolated maintenance attempt.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct MaintenanceReport {
    /// Reflog expiry completed; an empty report means no records qualified.
    pub expired: Option<ExpireReport>,
    /// A replacement pack was verified and eligible old packs were retired.
    pub retired: Option<RetireReport>,
    /// Pack consolidation was selected for this attempt. Its publication and removals are in
    /// `retired`, or in the [`MaintenanceCause::Retire`] failure.
    pub consolidated: bool,
    /// Old unreachable primary loose objects were swept.
    pub pruned: Option<PruneReport>,
}

/// Why maintenance stopped; a phase error carries that phase's partial effects.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MaintenanceCause {
    /// A pinned pack/index changed or a content read found damage before this mutation boundary.
    #[error(transparent)]
    PackChanged(Box<crate::ObjectReadError>),
    /// The caller could not hold a repository-wide exclusion boundary.
    #[error("maintenance isolation unavailable")]
    Isolation(#[source] io::Error),
    /// `gc.autoPackLimit` is not a 32-bit Git integer; no phase has started.
    #[error("invalid pack consolidation configuration")]
    Config(#[source] crate::config::InvalidValue),
    /// Directory durability for destructive publication is not established on this platform.
    #[error("composed maintenance is unsupported on this platform")]
    UnsupportedPlatform,
    /// Cancellation was observed before a phase began.
    #[error("maintenance cancelled")]
    Cancelled,
    /// A bounded plan or input observation was incomplete; no phase has started.
    #[error("retention scan incomplete")]
    Incomplete(#[source] RetentionError),
    /// An input changed after planning; retry with a fresh isolated plan.
    #[error("maintenance inputs changed")]
    Changed,
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
///
/// The outer report holds completed earlier phases. Inspect the nested phase failure in `cause`
/// for the current phase's partial changes and uncertain boundary. Cancellation never implies
/// rollback. Reconcile the actual storage and retry only with fresh scans under a new guard.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{InitKind, ObjectFormat, Repository};
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Bare,
/// )?;
/// let cancel = AtomicBool::new(false);
///
/// // This fixture is private to this single-threaded example. No external writer, reader or
/// // alternate-store dependent exists. A shared repository needs a real exclusion mechanism.
/// struct PrivateFixture;
/// impl girt::retention::MaintenanceIsolation for PrivateFixture {
///     type Guard = ();
///     fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
///         Ok(())
///     }
/// }
///
/// use girt::retention::{MaintenanceCause, RepackLimits, RetentionPolicy};
///
/// let failure = repo
///     .run_maintenance(
///         &mut PrivateFixture,
///         &RetentionPolicy::default(),
///         RepackLimits::default(),
///         &AtomicBool::new(true),
///     )
///     .unwrap_err();
///
/// assert!(matches!(failure.cause, MaintenanceCause::Cancelled));
/// assert!(failure.report.expired.is_none());
/// assert!(failure.report.retired.is_none());
/// assert!(failure.report.pruned.is_none());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[error("maintenance failed")]
#[non_exhaustive]
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
    /// old readers and alternate-store dependents for the entire sequence. One bounded retention
    /// plan serves all phases. Input fingerprints are revalidated before mutation, with exact
    /// phase outputs advancing the observation. An external change refuses the attempt; a new
    /// isolated call re-plans. The graph is not re-walked per phase.
    /// Expiry uses the policy cutoffs; pack retirement then retains every remaining history,
    /// protected pack and alternate, regardless of those cutoffs. Loose pruning also retains all
    /// remaining histories. An earlier phase's success does not authorize a later deletion by
    /// itself. A failed phase stops the sequence and reports known earlier effects plus that
    /// phase's partial effects. Retry with a new isolation guard and fresh scans. On non-Unix
    /// platforms the sequence refuses before any publication or deletion.
    ///
    /// When [`RetentionPolicy::pack_consolidation`] selects consolidation, retirement also retires
    /// recent packs other than `.keep` and promisor packs, keeping every object they hold; see
    /// [`PackConsolidation`](super::PackConsolidation).
    ///
    /// # Errors
    ///
    /// Isolation refusal, unsupported platform, malformed `gc.autoPackLimit`, cancellation and
    /// any phase failure preserve the completed-phase report and the current phase's partial
    /// result when applicable.
    ///
    /// # Maintain an exclusively owned disposable repository
    ///
    /// Defaults retain every loose object and reflog record and protect existing packs. The
    /// sequence can still publish additive replacement storage. Select expiry policy explicitly
    /// when reclamation is intended; do not treat successful maintenance as a full integrity
    /// check.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let cancel = AtomicBool::new(false);
    ///
    /// # if cfg!(not(unix)) { return Ok(()); }
    ///
    /// // This fixture is private to this single-threaded example. No external writer, reader or
    /// // alternate-store dependent exists. A shared repository needs a real exclusion mechanism.
    /// struct PrivateFixture;
    /// impl girt::retention::MaintenanceIsolation for PrivateFixture {
    ///     type Guard = ();
    ///     fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// use girt::retention::{RepackLimits, RetentionPolicy};
    ///
    /// let id = repo.loose_objects().write_blob(b"retained by default")?;
    /// let report = repo.run_maintenance(
    ///     &mut PrivateFixture,
    ///     &RetentionPolicy::default(),
    ///     RepackLimits::default(),
    ///     &cancel,
    /// )?;
    ///
    /// assert!(report.expired.is_some() && report.retired.is_some() && report.pruned.is_some());
    /// assert!(report.pruned.unwrap().deleted.is_empty());
    /// assert!(
    ///     repo.objects_with(Default::default())?
    ///         .read_with(id, Default::default())?
    ///         .is_some()
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
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

    /// Expires reflogs, consolidates packs when selected, and prunes old unreachable loose objects.
    ///
    /// This is the explicit-limits form of [`Self::collect_garbage`]; `limits` bound the
    /// consolidated pack. Consolidation follows [`RetentionPolicy::pack_consolidation`]: by
    /// default it runs when the indexed primary packs without a `.keep` file outnumber
    /// `gc.autoPackLimit` (50 unless configured; 0 or a negative value turns it off). It
    /// publishes one pack holding every retained object and every object of the recent packs it
    /// replaces, synchronizes it, decodes every retained object from it, and only then retires
    /// the old packs, as [`Self::retire_old_packs`] does. `.keep` packs, promisor packs, packs
    /// with other sidecars and alternates stay in place. Old unreachable objects in retired packs
    /// older than [`RetentionPolicy::recent_cutoff`] are reclaimed; with no cutoff, every packed
    /// object is kept. Unreachable objects copied from recent packs live until the new pack is
    /// older than the cutoff. Without consolidation, pack files stay untouched.
    ///
    /// All other contracts are those of [`Self::collect_garbage`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::collect_garbage`]. A malformed `gc.autoPackLimit` refuses
    /// with [`MaintenanceCause::Config`] before any phase; a consolidation failure is a
    /// [`MaintenanceCause::Retire`] carrying its publication and removals.
    ///
    /// # Consolidate a repository's packs
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let cancel = AtomicBool::new(false);
    ///
    /// # if cfg!(not(unix)) { return Ok(()); }
    ///
    /// // This fixture is private to this single-threaded example. No external writer, reader or
    /// // alternate-store dependent exists. A shared repository needs a real exclusion mechanism.
    /// struct PrivateFixture;
    /// impl girt::retention::MaintenanceIsolation for PrivateFixture {
    ///     type Guard = ();
    ///     fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// use girt::retention::{PackConsolidation, RepackLimits, RetentionPolicy};
    ///
    /// let first = repo.loose_objects().write_blob(b"first")?;
    /// repo.repack_retained(
    ///     &RetentionPolicy::default(),
    ///     RepackLimits::default(),
    ///     &cancel,
    /// )?;
    ///
    /// let second = repo.loose_objects().write_blob(b"second")?;
    /// repo.repack_retained(
    ///     &RetentionPolicy::default(),
    ///     RepackLimits::default(),
    ///     &cancel,
    /// )?;
    ///
    /// let policy = RetentionPolicy::default().with_pack_consolidation(PackConsolidation::Always);
    /// let report = repo.collect_garbage_with(
    ///     &mut PrivateFixture,
    ///     &policy,
    ///     RepackLimits::default(),
    ///     &cancel,
    /// )?;
    /// assert!(report.consolidated);
    ///
    /// let packs = std::fs::read_dir(repo.object_dir().join("pack"))?
    ///     .filter(|entry| {
    ///         entry
    ///             .as_ref()
    ///             .is_ok_and(|entry| entry.path().extension().is_some_and(|ext| ext == "pack"))
    ///     })
    ///     .count();
    /// assert_eq!(packs, 1);
    ///
    /// let objects = repo.objects_with(Default::default())?;
    /// assert!(objects.read_with(first, Default::default())?.is_some());
    /// assert!(objects.read_with(second, Default::default())?.is_some());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn collect_garbage_with<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
    ) -> Result<MaintenanceReport, MaintenanceFailure> {
        self.collect_garbage_controlled(isolation, policy, limits, cancel, || {})
    }

    /// Expires reflogs and prunes old unreachable loose objects under one isolation guard.
    ///
    /// Builds one bounded retention plan. Before either phase mutates storage, fingerprints of
    /// refs, histories, worktree metadata, indexes, shallow state, loose generations, packs and
    /// alternate topology must still match. Expiry advances the observation only for its exact
    /// outputs; pruning uses the plan's post-expiry required closure. External changes refuse the
    /// attempt; retry under a new guard to re-plan.
    ///
    /// Between expiry and pruning, packs are consolidated when
    /// [`RetentionPolicy::pack_consolidation`] selects it, which by default means more than
    /// `gc.autoPackLimit` packs. This form bounds the consolidated pack with
    /// [`RepackLimits::default`]; see [`Self::collect_garbage_with`] for the consolidation
    /// contract and for larger repositories. Otherwise pack files stay untouched.
    ///
    /// # Errors
    ///
    /// Returns isolation, configuration, incomplete/changed input, cancellation or phase errors,
    /// preserving any completed expiry, consolidation and partial pruning. Non-Unix hosts refuse
    /// before mutation.
    ///
    /// # Reclaim an old, unreferenced loose blob
    ///
    /// The timestamp is set explicitly so this disposable example does not depend on clock
    /// resolution or a sleep. The cutoff is demonstration policy, not a recommended production
    /// retention window.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let cancel = AtomicBool::new(false);
    ///
    /// # if cfg!(not(unix)) { return Ok(()); }
    ///
    /// // This fixture is private to this single-threaded example. No external writer, reader or
    /// // alternate-store dependent exists. A shared repository needs a real exclusion mechanism.
    /// struct PrivateFixture;
    /// impl girt::retention::MaintenanceIsolation for PrivateFixture {
    ///     type Guard = ();
    ///     fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// use std::time::{Duration, UNIX_EPOCH};
    ///
    /// use girt::retention::RetentionPolicy;
    ///
    /// let id = repo.loose_objects().write_blob(b"old and unreachable")?;
    /// let hex = id.to_string();
    /// let path = repo.object_dir().join(&hex[..2]).join(&hex[2..]);
    /// std::fs::File::open(path)?.set_modified(UNIX_EPOCH + Duration::from_secs(1))?;
    ///
    /// let policy = RetentionPolicy::default().with_recent_cutoff(UNIX_EPOCH + Duration::from_secs(2));
    /// let report = repo.collect_garbage(&mut PrivateFixture, &policy, &cancel)?;
    /// assert_eq!(report.pruned.unwrap().deleted, [id]);
    /// assert!(
    ///     repo.objects_with(Default::default())?
    ///         .read_with(id, Default::default())?
    ///         .is_none()
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn collect_garbage<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
    ) -> Result<MaintenanceReport, MaintenanceFailure> {
        self.collect_garbage_with(isolation, policy, RepackLimits::default(), cancel)
    }

    /// Collects garbage with an observation checkpoint after planning and before mutation.
    ///
    /// `after_plan` runs synchronously while the caller's isolation guard is held. It may inspect
    /// progress or coordinate a deterministic race test. Any input changes it causes refuse the
    /// attempt before mutation. All other contracts are those of [`Self::collect_garbage`].
    ///
    /// # Errors
    ///
    /// Returns the errors and completed effects described by [`Self::collect_garbage`].
    pub fn collect_garbage_with_checkpoint<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
        after_plan: impl FnOnce(),
    ) -> Result<MaintenanceReport, MaintenanceFailure> {
        self.collect_garbage_controlled(
            isolation,
            policy,
            RepackLimits::default(),
            cancel,
            after_plan,
        )
    }

    fn collect_garbage_controlled<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
        after_plan: impl FnOnce(),
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
            let _ = (policy, limits, after_plan);
            Err(failed(
                MaintenanceCause::UnsupportedPlatform,
                MaintenanceReport::default(),
            ))
        }
        #[cfg(unix)]
        self.maintain_exclusive(
            policy,
            Phases::consolidating(limits),
            cancel,
            after_plan,
            || {},
            || {},
        )
    }

    #[cfg(unix)]
    fn run_maintenance_exclusive(
        &self,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
        after_expiry: impl FnOnce(),
    ) -> Result<MaintenanceReport, MaintenanceFailure> {
        self.maintain_exclusive(
            policy,
            Phases::retiring(limits),
            cancel,
            || {},
            after_expiry,
            || {},
        )
    }

    #[cfg(unix)]
    fn maintain_exclusive(
        &self,
        policy: &RetentionPolicy,
        phases: Phases,
        cancel: &AtomicBool,
        after_plan: impl FnOnce(),
        after_expiry: impl FnOnce(),
        after_retirement: impl FnOnce(),
    ) -> Result<MaintenanceReport, MaintenanceFailure> {
        let _headers = crate::pack::HeaderScope::new();
        let consolidated = policy
            .pack_consolidation
            .selected(self, policy.max_entries, cancel)
            .map_err(|error| {
                failed(
                    match error {
                        SelectError::Config(error) => MaintenanceCause::Config(error),
                        SelectError::Observe(reason) => MaintenanceCause::Incomplete(reason),
                        SelectError::Cancelled => MaintenanceCause::Cancelled,
                    },
                    MaintenanceReport::default(),
                )
            })?;
        let mut report = MaintenanceReport {
            consolidated,
            ..MaintenanceReport::default()
        };
        let repack = if phases.retire || report.consolidated {
            Some(phases.limits)
        } else {
            None
        };
        let mut session =
            super::inputs::Session::new_folding(self, policy, report.consolidated, cancel)
                .map_err(|reason| {
                    failed(
                        if cancel.load(Ordering::Relaxed) {
                            MaintenanceCause::Cancelled
                        } else {
                            MaintenanceCause::Incomplete(reason)
                        },
                        report.clone(),
                    )
                })?;
        after_plan();
        if cancel.load(Ordering::Relaxed) {
            return Err(failed(MaintenanceCause::Cancelled, report));
        }
        session
            .plan
            .check_packs_unchanged(cancel)
            .map_err(|error| {
                failed(
                    MaintenanceCause::PackChanged(Box::new(error)),
                    report.clone(),
                )
            })?;
        session.validate(self, policy, cancel).map_err(|_| {
            failed(
                if cancel.load(Ordering::Relaxed) {
                    MaintenanceCause::Cancelled
                } else {
                    MaintenanceCause::Changed
                },
                report.clone(),
            )
        })?;
        let expired = self
            .expire_reflogs_planned(policy, cancel, &mut session)
            .map_err(|error| failed(MaintenanceCause::Expire(Box::new(error)), report.clone()))?;
        report.expired = Some(expired);
        session.plan.reachable = session.plan.required.clone();
        session.plan.roots = session.plan.strong_roots.clone();
        session.plan.reflog_expiry_candidates.clear();
        after_expiry();
        if cancel.load(Ordering::Relaxed) {
            return Err(failed(MaintenanceCause::Cancelled, report));
        }
        if let Some(limits) = repack {
            let retired = self
                .retire_planned(policy, limits, cancel, &mut session)
                .map_err(|error| {
                    failed(MaintenanceCause::Retire(Box::new(error)), report.clone())
                })?;
            let changes: Vec<_> = retired
                .removed
                .iter()
                .map(|path| (path.clone(), None))
                .collect();
            report.retired = Some(retired);
            after_retirement();
            if cancel.load(Ordering::Relaxed) {
                return Err(failed(MaintenanceCause::Cancelled, report));
            }
            let observed =
                super::inputs::Inputs::observe(self, policy, cancel).map_err(|reason| {
                    failed(
                        if cancel.load(Ordering::Relaxed) {
                            MaintenanceCause::Cancelled
                        } else {
                            MaintenanceCause::Incomplete(reason)
                        },
                        report.clone(),
                    )
                })?;
            session.inputs.accept(observed, &changes).map_err(|_| {
                failed(
                    if cancel.load(Ordering::Relaxed) {
                        MaintenanceCause::Cancelled
                    } else {
                        MaintenanceCause::Changed
                    },
                    report.clone(),
                )
            })?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(failed(MaintenanceCause::Cancelled, report));
        }
        let pruned = self
            .prune_planned(policy, cancel, &session)
            .map_err(|error| failed(MaintenanceCause::Prune(Box::new(error)), report.clone()))?;
        report.pruned = Some(pruned);
        Ok(report)
    }
}

/// Which pack phases one composed maintenance call may run.
#[cfg(unix)]
#[derive(Clone, Copy)]
struct Phases {
    /// Retire old packs whether or not consolidation is selected, as `run_maintenance` does.
    retire: bool,
    /// Bounds a replacement pack, whether for retirement or consolidation.
    limits: RepackLimits,
}

#[cfg(unix)]
impl Phases {
    fn retiring(limits: RepackLimits) -> Self {
        Self {
            retire: true,
            limits,
        }
    }

    fn consolidating(limits: RepackLimits) -> Self {
        Self {
            retire: false,
            limits,
        }
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
    #[rstest::rstest]
    #[case::sha1("sha1")]
    #[case::sha256("sha256")]
    fn ref_change_after_expiry_refuses_prune(#[case] format: &str) {
        let root = tempfile::tempdir().unwrap();
        let output = isolated_git(root.path())
            .args([
                "init",
                "--bare",
                "--template=",
                &format!("--object-format={format}"),
                root.path().to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let repository = Repository::open(root.path()).unwrap();
        let blob = repository
            .loose_objects()
            .write_blob(b"old orphan")
            .unwrap();
        let error = repository
            .maintain_exclusive(
                &RetentionPolicy {
                    recent_cutoff: Some(
                        std::time::SystemTime::now() + std::time::Duration::from_secs(60),
                    ),
                    ..Default::default()
                },
                Phases::consolidating(RepackLimits::default()),
                &AtomicBool::new(false),
                || {},
                || {
                    assert!(
                        isolated_git(root.path())
                            .current_dir(root.path())
                            .args(["update-ref", "refs/tags/saved", &blob.to_string()])
                            .output()
                            .unwrap()
                            .status
                            .success()
                    );
                },
                || {},
            )
            .unwrap_err();
        assert!(
            matches!(error.cause, MaintenanceCause::Prune(ref failure) if matches!(failure.cause, super::super::PruneCause::Changed))
        );
        assert!(error.report.expired.is_some());
        assert!(error.report.pruned.is_none());
        assert!(
            isolated_git(root.path())
                .current_dir(root.path())
                .args(["cat-file", "-e", &blob.to_string()])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    #[test]
    fn cancellation_after_retirement_keeps_completed_report() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            isolated_git(root.path())
                .args(["init", "--bare", "--template=", "."])
                .output()
                .unwrap()
                .status
                .success()
        );
        let repository = Repository::open(root.path()).unwrap();
        let cancel = AtomicBool::new(false);
        let error = repository
            .maintain_exclusive(
                &RetentionPolicy::default(),
                Phases::retiring(RepackLimits::default()),
                &cancel,
                || {},
                || {},
                || cancel.store(true, Ordering::Relaxed),
            )
            .unwrap_err();
        assert!(matches!(error.cause, MaintenanceCause::Cancelled));
        assert!(error.report.expired.is_some());
        assert!(error.report.retired.as_ref().unwrap().durable);
        assert!(error.report.pruned.is_none());
    }

    fn isolated_git(root: &std::path::Path) -> Command {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join("absent-config"));
        command
    }
    #[rstest::rstest]
    #[case::truncate(true)]
    #[case::overwrite(false)]
    fn warmed_pack_change_refuses_maintenance(#[case] truncate: bool) {
        let (_root, repo, policy, pack) = super::super::walk_tests::header_fixture();
        let before = std::fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count();
        let error = repo
            .maintain_exclusive(
                &policy,
                Phases::retiring(RepackLimits::trusted()),
                &AtomicBool::new(false),
                || super::super::walk_tests::change_pack(&pack, truncate),
                || {},
                || {},
            )
            .unwrap_err();
        assert!(matches!(&error.cause, MaintenanceCause::PackChanged(source)
            if matches!(source.as_ref(), crate::ObjectReadError::PackChanged { path } if path == &pack)));
        assert!(error.report.expired.is_none());
        assert!(error.report.retired.is_none());
        assert!(error.report.pruned.is_none());
        assert_eq!(
            std::fs::read_dir(repo.object_dir().join("pack"))
                .unwrap()
                .count(),
            before
        );
    }
}
