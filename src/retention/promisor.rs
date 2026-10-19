//! Repacking a partial clone into one promisor pack and one ordinary pack.
//!
//! A partial clone's `.promisor` packs do two jobs. They store objects, and every object they hold
//! promises the objects it names, so a missing blob counts as promised only while some promisor
//! pack object names it. Repacking therefore keeps every object found in an old primary promisor
//! pack, reachable or not, in one new promisor pack. An unreachable promisor tree can be the only
//! witness that still promises a blob a local ordinary tree names. Retained objects found only
//! loose or in ordinary packs go to a separate ordinary pack, so local objects never become
//! promisor objects. Git's `git gc` produces the same split.
//!
//! An old promisor pack retires only after every object it holds decodes and hashes correctly from
//! the new, durable promisor pack. Its marker is removed last, after its index and pack, so the
//! pack is never visible without its marker. A marker whose pack is gone promises nothing and is
//! harmless.

#[cfg(unix)]
use std::collections::BTreeSet;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
use super::repack::{Leg, LegProgress};
#[cfg(unix)]
use super::retire::{multi_pack_index_paths, read_cause, recheck, same_old_generation, snapshot};
use super::{MaintenanceIsolation, RepackLimits, RepackPublished, RetentionPolicy, RetireCause};
#[cfg(unix)]
use super::{PartialClonePolicy, RepackError, RetentionError};
use crate::Repository;
#[cfg(unix)]
use crate::pack::FilePack;
#[cfg(unix)]
use crate::pack::publish::{Step, sync_directory};

/// Packs published and old promisor packs retired or kept by
/// [`Repository::repack_retained_isolated`].
///
/// A failure carries the same report with the effects that completed before it.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct IsolatedRepackReport {
    /// Pack of retained objects found only loose, in ordinary packs or in alternates. `None` when
    /// there were none, so a repository whose objects all came from its promisor remote gets no
    /// empty ordinary pack.
    pub ordinary: Option<RepackPublished>,
    /// Pack, marked by a `.promisor` file, holding every object found in an old primary promisor
    /// pack. The marker is empty, unless the new pack is byte-identical to an existing one, whose
    /// marker and its contents are kept. `None` outside partial clones and when there were no
    /// promisor packs.
    pub promisor: Option<RepackPublished>,
    /// Both published packs, their markers and the object directory were synchronized.
    pub durable: bool,
    /// Names publication made visible, in order: each leg's `.promisor` marker, `.pack` and
    /// `.idx`. On failure this includes a partial leg; a leg whose `.idx` is here is a complete,
    /// visible pair even if [`Self::durable`] is false.
    pub visible: Vec<PathBuf>,
    /// Old multi-pack-index artifacts and old promisor pack artifacts whose unlink returned
    /// success, in execution order. Each old pack's `.promisor` marker follows its index and pack.
    /// The multi-pack-index, index and pack unlinks are each synchronized before the next kind
    /// goes, so a pack is never visible without its `.promisor` marker. The final marker unlink is
    /// not synchronized: after a crash, a listed marker can reappear without a pack, and
    /// promises nothing.
    pub removed: Vec<PathBuf>,
    /// Old promisor packs left in place, and why.
    pub retained: Vec<RetainedPack>,
}

/// An old promisor pack that [`Repository::repack_retained_isolated`] left in place.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RetainedPack {
    /// The old `.pack` path.
    pub pack: PathBuf,
    /// Why it was not retired.
    pub reason: RetainReason,
}

/// Why an old promisor pack stays in place.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RetainReason {
    /// A `.keep` file, a modification time at or after the policy's recent cutoff, or another
    /// sidecar such as a cruft pack's `.mtimes` protects the pack.
    Protected,
    /// No new durable promisor pack is known to hold this pack's objects: the pack was not part of
    /// the scan that chose the objects, or no promisor pack was published.
    NoDurablePromisorCoverage,
}

/// [`Repository::repack_retained_isolated`] failure, with the effects that completed before it.
///
/// Publication can succeed before verification, synchronization or an unlink fails. Read
/// `report` and `uncertain` together, and retry from a fresh scan under a new isolation guard;
/// never finish the removal sequence by hand.
#[derive(Debug, thiserror::Error)]
#[error("isolated repack failed")]
#[non_exhaustive]
pub struct IsolatedRepackFailure {
    /// Failure class. [`RetireCause::ReplacementMissing`] names an object the new promisor pack
    /// should hold but does not.
    #[source]
    pub cause: RetireCause,
    /// Packs published and artifacts removed before the failure.
    pub report: Box<IsolatedRepackReport>,
    /// Artifact or directory at a failed unlink or synchronization, whose state is unknown.
    pub uncertain: Option<PathBuf>,
}

/// The visible change an isolated repack is about to make, so tests can stop there.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Checkpoint {
    /// A publisher step of the promisor pack.
    Promisor(Step),
    /// A publisher step of the ordinary pack.
    Ordinary(Step),
    /// Both legs are published; planning, coverage and removal follow.
    Published,
    /// Unlinking an old promisor pack's index.
    RemoveIndex,
    /// Unlinking an old promisor pack's `.pack`.
    RemovePack,
    /// Unlinking an old promisor pack's `.promisor` marker.
    RemoveMarker,
}

impl Repository {
    /// Repacks retained objects under caller isolation, splitting a partial clone's objects into a
    /// promisor pack and an ordinary pack, and retires the old promisor packs.
    ///
    /// This is the only repack that runs in a partial clone, and only when `policy` uses
    /// [`Promised`](crate::retention::PartialClonePolicy::Promised); otherwise a partial
    /// clone is refused with [`PartialClone`](crate::retention::RetentionError::PartialClone)
    /// as [`Self::repack_retained`] refuses it. The configuration is read from disk before and
    /// after planning.
    ///
    /// In a partial clone, every object found in any old primary `.promisor` pack, reachable or
    /// not, goes into one new pack published with an empty `.promisor` marker, as `git gc` does. A
    /// new pack byte-identical to an existing one reuses it and keeps that pack's marker
    /// contents. Retained objects found only loose, in ordinary packs or in alternates go into
    /// an ordinary pack. An object with a copy in any promisor pack is a promisor object,
    /// whatever other copies exist. Promised objects stay missing; nothing is fetched. Old
    /// ordinary packs and loose objects stay in place, as with [`Self::repack_retained`].
    ///
    /// Each old primary promisor pack is then retired unless it is protected (see
    /// [`RetainReason`]). Before anything is removed, the new promisor pack's index and checksum
    /// are verified, and every object the old pack holds is decoded and hashed from the new pack
    /// under [`RetentionPolicy::read`]; a name in the new index is not enough. Each pack retires
    /// in this order: index, directory sync, `.rev`/`.bitmap`, pack, directory sync, `.promisor`
    /// marker. The marker unlink is not synchronized; a marker that reappears after a crash has
    /// no pack and promises nothing. Multi-pack-index artifacts are removed first, as
    /// [`Self::retire_old_packs`] does.
    ///
    /// Outside partial clones, the call publishes the retained objects as one ordinary pack, like
    /// [`Self::repack_retained`] under isolation, and never retires anything: `.promisor` files
    /// promise nothing there, and their packs stay. The only extra cost there is reading the
    /// configuration. A partial clone writes no bitmap
    /// ([`SkippedPartialClone`](crate::retention::BitmapOutcome::SkippedPartialClone)).
    ///
    /// The caller's [`MaintenanceIsolation`] guard must exclude writers, old readers pinned to
    /// packs and dependent repositories for the whole call. On non-Unix platforms the call refuses
    /// before planning because directory durability is not established.
    ///
    /// # Errors
    ///
    /// Isolation refusal, unsupported platforms and incomplete scans fail before any change.
    /// Publication failures, a new promisor pack that lacks an old object
    /// ([`RetireCause::ReplacementMissing`]), decode or hash failures, read limits, changed
    /// storage, cancellation and I/O fail with the packs already published and artifacts already
    /// removed in [`IsolatedRepackFailure::report`]. A failure can leave a new `.promisor` marker
    /// without a pack, or an unindexed pack; readers ignore both, and a retry reuses them.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(unix)]
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::retention::{
    ///     MaintenanceIsolation, PartialClonePolicy, RepackLimits, RetentionPolicy,
    /// };
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// // This private repository has no external users or alternate dependents.
    /// struct PrivateFixture;
    /// impl MaintenanceIsolation for PrivateFixture {
    ///     type Guard = ();
    ///     fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// repo.loose_objects().write_blob(b"local")?;
    /// let policy = RetentionPolicy::default().with_partial_clone(PartialClonePolicy::Promised);
    /// let report = repo.repack_retained_isolated(
    ///     &mut PrivateFixture,
    ///     &policy,
    ///     RepackLimits::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    /// // Not a partial clone: one ordinary pack, no promisor pack, nothing retired.
    /// assert_eq!(report.ordinary.map(|pack| pack.written.objects), Some(1));
    /// assert!(report.promisor.is_none());
    /// assert!(report.removed.is_empty());
    /// # Ok(())
    /// # }
    /// # #[cfg(not(unix))]
    /// # fn main() {}
    /// ```
    pub fn repack_retained_isolated<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
    ) -> Result<IsolatedRepackReport, IsolatedRepackFailure> {
        if cancel.load(Ordering::Relaxed) {
            return Err(failed(
                RetireCause::Cancelled,
                &IsolatedRepackReport::default(),
            ));
        }
        let _guard = isolation.acquire(self).map_err(|error| {
            failed(
                RetireCause::Isolation(error),
                &IsolatedRepackReport::default(),
            )
        })?;
        #[cfg(not(unix))]
        {
            let _ = (policy, limits);
            Err(failed(
                RetireCause::UnsupportedPlatform,
                &IsolatedRepackReport::default(),
            ))
        }
        #[cfg(unix)]
        self.repack_isolated_at(policy, limits, cancel, &mut |_| Ok(()))
    }

    /// Runs the isolated repack, calling `checkpoint` before each visible change; an error there
    /// stops the run as an I/O failure at that point.
    #[cfg(unix)]
    pub(super) fn repack_isolated_at(
        &self,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
        checkpoint: &mut dyn FnMut(Checkpoint) -> io::Result<()>,
    ) -> Result<IsolatedRepackReport, IsolatedRepackFailure> {
        let _headers = crate::pack::HeaderScope::new();
        let mut report = IsolatedRepackReport::default();
        check(cancel, &report)?;
        let current = self.reopen_for_repack(&report)?;
        let partial = current.is_partial_clone();
        if partial && policy.partial_clone != PartialClonePolicy::Promised {
            return Err(failed(
                RetireCause::Incomplete(RetentionError::PartialClone),
                &report,
            ));
        }
        let directory = current.object_dir().join("pack");
        let created_directory = !directory.exists();
        // Taken before planning, so any pack-directory change until removal is detected.
        let before = snapshot(&directory, policy.max_entries, cancel)
            .map_err(|cause| failed(cause, &report))?;
        let mut plan = current.plan_retention(policy, cancel);
        if let super::RetentionOutcome::Incomplete(reason) = plan.outcome {
            let cause = if cancel.load(Ordering::Relaxed) {
                RetireCause::Cancelled
            } else {
                RetireCause::Incomplete(reason)
            };
            return Err(failed(cause, &report));
        }
        // A repository that became, or stopped being, a partial clone during planning refuses.
        if self.reopen_for_repack(&report)?.is_partial_clone() != partial {
            return Err(failed(RetireCause::Changed, &report));
        }
        // Repacking publishes every retained object; protection only decides what retires.
        let protected = std::mem::take(&mut plan.protected_packs);
        let store = plan.store.as_ref().ok_or_else(|| {
            failed(
                RetireCause::Incomplete(RetentionError::MissingPinnedPlanStore),
                &report,
            )
        })?;
        let mut promisor_ids = BTreeSet::new();
        let mut sources = BTreeSet::new();
        if partial {
            for (path, pack) in store.primary_promisor_packs() {
                sources.insert(path.to_path_buf());
                for id in pack.identities() {
                    check(cancel, &report)?;
                    promisor_ids.insert(id.map_err(|error| read_failure(error, &report))?);
                    if promisor_ids.len() > policy.max_entries {
                        return Err(failed(
                            RetireCause::Incomplete(RetentionError::PromisedObjectLimit),
                            &report,
                        ));
                    }
                }
            }
        }
        let ordinary_ids: BTreeSet<_> = plan.required.difference(&promisor_ids).copied().collect();

        let promisor_leg = Leg {
            ids: &promisor_ids,
            marker: Some(b""),
            partial_clone: true,
        };
        if !promisor_ids.is_empty() {
            let mut progress = LegProgress::default();
            let (published, _) = current
                .publish_leg(
                    &plan,
                    promisor_leg,
                    policy,
                    limits,
                    cancel,
                    || {},
                    &mut progress,
                    |step| checkpoint(Checkpoint::Promisor(step)).map_err(RepackError::Io),
                )
                .map_err(|error| {
                    repack_failure(error, std::mem::take(&mut progress), &mut report, false)
                })?;
            report.visible.extend(progress.publish.visible);
            report.promisor = Some(published);
        }
        let ordinary_leg = Leg {
            ids: &ordinary_ids,
            marker: None,
            partial_clone: partial,
        };
        if !ordinary_ids.is_empty() {
            let mut progress = LegProgress::default();
            let (published, _) = current
                .publish_leg(
                    &plan,
                    ordinary_leg,
                    policy,
                    limits,
                    cancel,
                    || {},
                    &mut progress,
                    |step| checkpoint(Checkpoint::Ordinary(step)).map_err(RepackError::Io),
                )
                .map_err(|error| {
                    repack_failure(error, std::mem::take(&mut progress), &mut report, true)
                })?;
            report.visible.extend(progress.publish.visible);
            report.ordinary = Some(published);
        }
        if report.promisor.is_some() || report.ordinary.is_some() {
            // The publisher's final sync made every pack-directory name durable. Only a pack
            // directory this call created needs its own name synchronized in the object directory.
            if created_directory {
                sync_directory(current.object_dir())
                    .map_err(|error| uncertain(error, &report, &directory))?;
            }
            report.durable = true;
        }
        checkpoint(Checkpoint::Published)
            .map_err(|error| failed(RetireCause::Io(error), &report))?;
        if !partial {
            return Ok(report);
        }

        let basename = |published: &RepackPublished| {
            directory.join(format!("pack-{}", published.written.checksum))
        };
        let new_promisor = report.promisor.as_ref().map(basename);
        let mut replacement_paths = Vec::new();
        for (published, marker) in [(&report.promisor, true), (&report.ordinary, false)] {
            if let Some(published) = published {
                let base = basename(published);
                replacement_paths.push(base.with_extension("pack"));
                replacement_paths.push(base.with_extension("idx"));
                if marker {
                    replacement_paths.push(base.with_extension("promisor"));
                }
            }
        }
        let mut retiring = Vec::new();
        for marker in before
            .keys()
            .filter(|path| path.extension().is_some_and(|ext| ext == "promisor"))
        {
            let pack = marker.with_extension("pack");
            let index = marker.with_extension("idx");
            // A marker whose pack or index is gone promises nothing; leave it alone.
            if !before.contains_key(&pack) || !before.contains_key(&index) {
                continue;
            }
            if new_promisor.as_deref() == Some(&*marker.with_extension("")) {
                continue;
            }
            let sidecar = before.keys().any(|path| {
                path.file_stem() == pack.file_stem()
                    && !matches!(
                        path.extension().and_then(|ext| ext.to_str()),
                        Some("idx" | "pack" | "rev" | "bitmap" | "promisor")
                    )
            });
            let reason = if protected.contains(&pack) || sidecar {
                Some(RetainReason::Protected)
            } else if new_promisor.is_none() || !sources.contains(&pack) {
                Some(RetainReason::NoDurablePromisorCoverage)
            } else {
                None
            };
            match reason {
                Some(reason) => report.retained.push(RetainedPack { pack, reason }),
                None => retiring.push(index),
            }
        }
        let Some(new_promisor) = new_promisor.filter(|_| !retiring.is_empty()) else {
            return Ok(report);
        };

        // Coverage: every object of every retiring pack decodes and hashes from the new pack.
        let mut budget = policy.packs;
        let replacement = FilePack::open(
            self.object_format(),
            &new_promisor.with_extension("idx"),
            &mut budget,
            cancel,
        )
        .map_err(|error| read_failure(error, &report))?;
        replacement
            .verify_index()
            .map_err(|error| read_failure(error, &report))?;
        let mut positions = BTreeSet::new();
        for (path, old) in store.primary_promisor_packs() {
            if !retiring.contains(&path.with_extension("idx")) {
                continue;
            }
            for id in old.identities() {
                check(cancel, &report)?;
                let id = id.map_err(|error| read_failure(error, &report))?;
                let position = replacement
                    .find(id)
                    .map_err(|error| read_failure(error, &report))?
                    .ok_or_else(|| failed(RetireCause::ReplacementMissing(id), &report))?;
                let offset = replacement
                    .offset(position)
                    .map_err(|error| read_failure(error, &report))?;
                positions.insert((offset, position));
            }
        }
        replacement
            .verify(cancel)
            .map_err(|error| failed(read_cause(error), &report))?;
        let positions: Vec<usize> = positions
            .into_iter()
            .map(|(_, position)| position)
            .collect();
        replacement
            .read_each(&positions, policy.read, policy.max_threads, cancel)
            .map_err(|error| failed(read_cause(error), &report))?;

        // Nothing else changed the pack directory, the pinned packs or the configuration.
        let after = snapshot(&directory, policy.max_entries, cancel)
            .map_err(|cause| failed(cause, &report))?;
        if !same_old_generation(&before, &after, &replacement_paths) {
            return Err(failed(RetireCause::Changed, &report));
        }
        plan.check_packs_unchanged(cancel)
            .map_err(|error| failed(RetireCause::PackChanged(Box::new(error)), &report))?;
        if !self.reopen_for_repack(&report)?.is_partial_clone() {
            return Err(failed(RetireCause::Changed, &report));
        }

        // A multi-pack index naming a retired pack would make reads fail; Git rebuilds it.
        let indices = multi_pack_index_paths(&directory)
            .map_err(|error| failed(RetireCause::Io(error), &report))?;
        let removed_index = !indices.is_empty();
        for path in indices {
            let result = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            result.map_err(|error| uncertain(error, &report, &path))?;
            report.removed.push(path);
        }
        if removed_index {
            // Ordering: the multi-pack index must be durably gone before any pack it names.
            sync_directory(&directory).map_err(|error| uncertain(error, &report, &directory))?;
        }
        for index in retiring {
            check(cancel, &report)?;
            let marker = index.with_extension("promisor");
            let mut artifacts = vec![index.clone()];
            for extension in ["rev", "bitmap"] {
                let path = index.with_extension(extension);
                if before.contains_key(&path) {
                    artifacts.push(path);
                }
            }
            artifacts.push(index.with_extension("pack"));
            for path in artifacts.iter().chain([&marker]) {
                recheck(path, before[path]).map_err(|cause| failed(cause, &report))?;
            }
            for path in artifacts {
                replacement
                    .check_unchanged()
                    .map_err(|error| failed(RetireCause::PackChanged(Box::new(error)), &report))?;
                let step = match path.extension().and_then(|ext| ext.to_str()) {
                    Some("idx") => Some(Checkpoint::RemoveIndex),
                    Some("pack") => Some(Checkpoint::RemovePack),
                    _ => None,
                };
                step.map_or(Ok(()), &mut *checkpoint)
                    .and_then(|()| fs::remove_file(&path))
                    .map_err(|error| uncertain(error, &report, &path))?;
                let index_removed = path == index;
                report.removed.push(path);
                if index_removed {
                    // Ordering: the index unlink must be durable before the pack goes, so a crash
                    // never leaves a visible index naming a missing pack.
                    sync_directory(&directory)
                        .map_err(|error| uncertain(error, &report, &index))?;
                }
            }
            // Ordering: the pack unlink must be durable before the marker goes, so a crash never
            // leaves the pack visible without its marker.
            sync_directory(&directory)
                .map_err(|error| uncertain(error, &report, &index.with_extension("pack")))?;
            checkpoint(Checkpoint::RemoveMarker)
                .and_then(|()| fs::remove_file(&marker))
                .map_err(|error| uncertain(error, &report, &marker))?;
            // No sync after the marker unlink: a marker that reappears after a crash has no pack
            // and promises nothing. The next pack's index sync, or the caller's next directory
            // sync, persists it.
            report.removed.push(marker.clone());
        }
        Ok(report)
    }

    /// Opens this repository again, so checks use the configuration on disk.
    #[cfg(unix)]
    fn reopen_for_repack(
        &self,
        report: &IsolatedRepackReport,
    ) -> Result<Repository, IsolatedRepackFailure> {
        let current = Repository::open(self.git_dir()).map_err(|error| {
            failed(RetireCause::Incomplete(RetentionError::read(error)), report)
        })?;
        if current.object_dir() != self.object_dir()
            || current.object_format() != self.object_format()
        {
            return Err(failed(RetireCause::Changed, report));
        }
        Ok(current)
    }
}

/// Records a leg the publisher completed before optional bitmap work failed.
#[cfg(unix)]
fn repack_failure(
    error: RepackError,
    progress: LegProgress,
    report: &mut IsolatedRepackReport,
    ordinary: bool,
) -> IsolatedRepackFailure {
    let leg = if ordinary {
        &mut report.ordinary
    } else {
        &mut report.promisor
    };
    if let RepackError::Bitmap { published, .. } = &error {
        *leg = Some(**published);
    } else if progress.publish.pair_visible()
        && let Some(written) = progress.written
    {
        // The pair is visible although its final sync failed.
        *leg = Some(RepackPublished {
            written,
            bitmap: super::BitmapOutcome::Disabled,
        });
    }
    report.visible.extend(progress.publish.visible);
    let cause = match error {
        RepackError::Cancelled => RetireCause::Cancelled,
        RepackError::Incomplete(reason) => RetireCause::Incomplete(reason),
        error => RetireCause::Repack(Box::new(error)),
    };
    IsolatedRepackFailure {
        cause,
        report: Box::new(report.clone()),
        uncertain: progress.publish.uncertain,
    }
}

#[cfg(unix)]
fn read_failure(
    error: crate::ObjectReadError,
    report: &IsolatedRepackReport,
) -> IsolatedRepackFailure {
    failed(RetireCause::Read(Box::new(error)), report)
}

#[cfg(unix)]
fn uncertain(
    error: io::Error,
    report: &IsolatedRepackReport,
    path: &Path,
) -> IsolatedRepackFailure {
    IsolatedRepackFailure {
        cause: RetireCause::Io(error),
        report: Box::new(report.clone()),
        uncertain: Some(path.to_path_buf()),
    }
}

#[cfg(unix)]
fn check(cancel: &AtomicBool, report: &IsolatedRepackReport) -> Result<(), IsolatedRepackFailure> {
    if cancel.load(Ordering::Relaxed) {
        Err(failed(RetireCause::Cancelled, report))
    } else {
        Ok(())
    }
}

fn failed(cause: RetireCause, report: &IsolatedRepackReport) -> IsolatedRepackFailure {
    IsolatedRepackFailure {
        cause,
        report: Box::new(report.clone()),
        uncertain: None,
    }
}
