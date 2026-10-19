#[cfg(unix)]
use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::fs::{self, File};
use std::io;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::time::SystemTime;

#[cfg(unix)]
use super::RetentionPlan;
use super::{
    MaintenanceIsolation, RepackError, RepackLimits, RepackPublished, RetentionError,
    RetentionPolicy,
};
#[cfg(unix)]
use crate::pack::FilePack;
use crate::{ObjectId, ObjectReadError, Repository};

/// Additive publication and known old-artifact removals from one isolated attempt.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct RetireReport {
    /// Replacement pair visibly published; later synchronization or verification may fail.
    pub published: Option<RepackPublished>,
    /// Both pack and object directories synchronized after publication.
    pub durable: bool,
    /// Old index/pack/sidecar paths and multi-pack-index artifacts whose unlink returned success,
    /// in execution order. Index and multi-pack-index unlinks are synchronized before the packs
    /// they name go, which is what makes retirement safe; the final pack unlinks are not
    /// synchronized, so after a crash a listed `.pack` can reappear as an unindexed file that
    /// readers ignore. Girt maintenance does not remove such a file, and neither does `git gc`.
    pub removed: Vec<PathBuf>,
}

/// Why old pack retirement stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RetireCause {
    /// A pinned pack/index changed or a content read found damage before this mutation boundary.
    #[error(transparent)]
    PackChanged(Box<crate::ObjectReadError>),
    /// The caller could not exclude writers, old readers or alternate dependents.
    #[error("maintenance isolation unavailable")]
    Isolation(#[source] io::Error),
    /// Directory durability is unavailable for this platform.
    #[error("pack retirement is unsupported on this platform")]
    UnsupportedPlatform,
    /// A bounded root or artifact observation was incomplete.
    #[error("retention scan incomplete")]
    Incomplete(#[source] RetentionError),
    /// Roots, shallow state or an old pack generation changed before deletion.
    #[error("maintenance generation changed")]
    Changed,
    /// Additive replacement publication failed without old-pack deletion.
    #[error(transparent)]
    Repack(Box<RepackError>),
    /// The replacement does not contain a retained object.
    #[error("replacement pack missing retained object {0}")]
    ReplacementMissing(ObjectId),
    /// The replacement could not be verified.
    #[error(transparent)]
    Read(Box<ObjectReadError>),
    /// Cancellation was observed between publication or pair-retirement steps.
    #[error("pack retirement cancelled")]
    Cancelled,
    /// An artifact or directory operation failed.
    #[error("pack retirement I/O")]
    Io(#[source] io::Error),
}

/// Failure with the known successful effects and any uncertain artifact.
///
/// Publication can succeed before verification, synchronization or an old-artifact unlink fails.
/// Use `report.published`, `report.durable`, `report.removed` and `uncertain` together. A visible
/// replacement alone does not establish that removing old storage is safe. Re-observe under
/// isolation before retrying; never complete the deletion sequence manually from this report.
///
/// An isolation refusal occurs before scanning or mutation. Later failure classes can carry
/// completed effects, so inspect the report for every failure.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::retention::{MaintenanceIsolation, RetentionPolicy, RetireCause};
/// use girt::{InitKind, ObjectFormat, Repository};
///
/// let directory = tempfile::tempdir()?;
/// let path = directory.path().join("r");
/// let repo = Repository::init(ObjectFormat::Sha1, path, InitKind::Bare)?;
///
/// struct Refuse;
/// impl MaintenanceIsolation for Refuse {
///     type Guard = ();
///     fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
///         Err(std::io::Error::new(
///             std::io::ErrorKind::WouldBlock,
///             "store is in use",
///         ))
///     }
/// }
///
/// let failure = repo
///     .retire_old_packs(
///         &mut Refuse,
///         &RetentionPolicy::default(),
///         Default::default(),
///         &AtomicBool::new(false),
///     )
///     .unwrap_err();
///
/// assert!(matches!(failure.cause, RetireCause::Isolation(_)));
/// assert!(failure.report.published.is_none());
/// assert!(failure.report.removed.is_empty());
/// assert!(failure.uncertain.is_none());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[error("pack retirement failed")]
#[non_exhaustive]
pub struct RetireFailure {
    /// Failure class.
    #[source]
    pub cause: RetireCause,
    /// Visible publication, its durability state, and old paths whose unlink returned success.
    pub report: Box<RetireReport>,
    /// Artifact at a failed unlink or directory-sync boundary, if effects are uncertain.
    pub uncertain: Option<PathBuf>,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Stamp {
    len: u64,
    modified: SystemTime,
}

impl Repository {
    /// Publishes every currently retained object and retires unprotected old pack pairs.
    ///
    /// The caller's [`MaintenanceIsolation`] guard must exclude external Git and girt writers,
    /// old readers pinned to packs, and repositories depending on this store as an alternate for
    /// the whole operation. The operation ignores reflog expiry cutoffs: every observed history
    /// remains a root. It builds one complete plan, revalidates input fingerprints, publishes a
    /// bounded replacement pair,
    /// synchronizes the pack and object directories, verifies each required ID through the
    /// replacement or a surviving protected pack, and compares
    /// old pack generations before deleting an old index followed by its pack. `.keep`, recent
    /// packs and packs with unsupported sidecars remain untouched. Optional `.rev` and `.bitmap`
    /// sidecars are retired with their pair. Alternate stores are never
    /// mutated. A replacement identical to an old pair is retained. On non-Unix platforms the
    /// operation refuses before publication because directory durability is not established.
    ///
    /// If any pack will retire, first removes `multi-pack-index*` artifacts in the primary pack
    /// directory, including bitmaps, reverse indexes and incremental-chain directories. These
    /// derived indexes could otherwise name removed packs and make Git reads fail. Git can
    /// rebuild them; their speedups are unavailable until then. Removal and synchronization
    /// failures preserve the partial report and identify the uncertain artifact/directory.
    ///
    /// An old pair is removed only after its retained objects are present in a verified durable
    /// replacement or surviving protected pack. Unreachable objects in unprotected packs may be
    /// reclaimed; loose roots strictly newer than `recent_cutoff` retain their complete closure.
    /// Packs exactly at the cutoff remain protected: pack protection conservatively preserves the
    /// whole pack rather than applying Git's per-loose-file expiry rule. Failure after publication
    /// can leave the new pair and a prefix of old paths removed. Retirement is made safe by
    /// synchronizing each index (and multi-pack-index) unlink before its pack goes; the pack
    /// unlink itself is not synchronized. A failure or crash after an index unlink can therefore
    /// leave an unindexed old pack file. Readers ignore it, and every object stays readable. While
    /// it is newer than the cutoff, later planning refuses with
    /// [`RetentionError::MissingProtectedIndex`]; once older, maintenance completes but leaves the
    /// file in place, as `git gc` does. Cancellation is checked before each pair, not
    /// between its two unlinks. A failed unlink or directory synchronization names its
    /// uncertain path. Retry only after a fresh scan.
    ///
    /// # Errors
    ///
    /// Isolation refusal, unsupported platform, incomplete or changed observations, publication,
    /// verification, cancellation and I/O report known effects and an uncertain boundary if any.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(unix)]
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::retention::{MaintenanceIsolation, RepackLimits, RetentionPolicy};
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let path = directory.path().join("r");
    /// let repo = Repository::init(ObjectFormat::Sha1, path, InitKind::Bare)?;
    ///
    /// let cancel = AtomicBool::new(false);
    /// // This private repository has no external users or alternate dependents.
    /// struct PrivateFixture;
    /// impl MaintenanceIsolation for PrivateFixture {
    ///     type Guard = ();
    ///     fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let id = repo.loose_objects().write_blob(b"retained")?;
    /// let first = repo.repack_retained(
    ///     &RetentionPolicy::default(),
    ///     RepackLimits::default(),
    ///     &cancel,
    /// )?;
    ///
    /// let other = repo.loose_objects().write_blob(b"second retained object")?;
    /// repo.repack_retained(
    ///     &RetentionPolicy::default(),
    ///     RepackLimits::default(),
    ///     &cancel,
    /// )?;
    ///
    /// // A future cutoff makes freshly created packs eligible only for this private fixture.
    /// let cutoff = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    /// let policy = RetentionPolicy::default()
    ///     .with_heads(vec![id, other])
    ///     .with_recent_cutoff(cutoff);
    /// let report = repo.retire_old_packs(
    ///     &mut PrivateFixture,
    ///     &policy,
    ///     RepackLimits::default(),
    ///     &cancel,
    /// )?;
    /// assert!(report.durable);
    ///
    /// let old_pack = repo
    ///     .git_dir()
    ///     .join("objects/pack")
    ///     .join(format!("pack-{}.pack", first.written.checksum));
    /// assert!(report.removed.contains(&old_pack));
    /// assert!(!old_pack.exists());
    ///
    /// let objects = repo.objects_with(Default::default())?;
    /// assert!(objects.read_with(id, Default::default())?.is_some());
    /// assert!(objects.read_with(other, Default::default())?.is_some());
    /// # Ok(())
    /// # }
    /// # #[cfg(not(unix))]
    /// # fn main() {}
    /// ```
    pub fn retire_old_packs<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
    ) -> Result<RetireReport, RetireFailure> {
        check(cancel, &RetireReport::default())?;
        let _guard = isolation
            .acquire(self)
            .map_err(|error| failed(RetireCause::Isolation(error), RetireReport::default()))?;
        #[cfg(not(unix))]
        {
            let _ = (policy, limits);
            Err(failed(
                RetireCause::UnsupportedPlatform,
                RetireReport::default(),
            ))
        }
        #[cfg(unix)]
        self.retire_old_packs_exclusive(policy, limits, cancel)
    }

    #[cfg(unix)]
    pub(super) fn retire_old_packs_exclusive(
        &self,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
    ) -> Result<RetireReport, RetireFailure> {
        self.retire_old_packs_at(
            policy,
            limits,
            cancel,
            || {},
            |path| fs::remove_file(path),
            |directory| File::open(directory).and_then(|dir| dir.sync_all()),
        )
    }

    // Checkpoints keep writer races and unlink faults deterministic in local tests.
    #[cfg(unix)]
    fn retire_old_packs_at(
        &self,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
        after_publication: impl FnOnce(),
        remove: impl FnMut(&Path) -> io::Result<()>,
        sync_directory: impl FnMut(&Path) -> io::Result<()>,
    ) -> Result<RetireReport, RetireFailure> {
        self.retire_planned_at(
            policy,
            limits,
            cancel,
            None,
            after_publication,
            (remove, sync_directory),
        )
    }

    #[cfg(unix)]
    pub(super) fn retire_planned(
        &self,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
        session: &mut super::inputs::Session,
    ) -> Result<RetireReport, RetireFailure> {
        self.retire_planned_at(
            policy,
            limits,
            cancel,
            Some(session),
            || {},
            (
                |path| fs::remove_file(path),
                |directory| File::open(directory).and_then(|dir| dir.sync_all()),
            ),
        )
    }

    #[cfg(unix)]
    fn retire_planned_at(
        &self,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
        session: Option<&mut super::inputs::Session>,
        after_publication: impl FnOnce(),
        actions: (
            impl FnMut(&Path) -> io::Result<()>,
            impl FnMut(&Path) -> io::Result<()>,
        ),
    ) -> Result<RetireReport, RetireFailure> {
        let _headers = crate::pack::HeaderScope::new();
        let (mut remove, mut sync_directory) = actions;
        let mut report = RetireReport::default();
        check(cancel, &report)?;
        let mut keep_all_logs = policy.clone();
        keep_all_logs.reflog_expiry = super::ReflogExpiryPolicy::KeepAll;
        let current = Repository::open(self.git_dir()).map_err(|error| {
            failed(
                RetireCause::Incomplete(RetentionError::read(error)),
                report.clone(),
            )
        })?;
        if current.object_dir() != self.object_dir()
            || current.object_format() != self.object_format()
        {
            return Err(failed(RetireCause::Changed, report));
        }
        let mut owned = if session.is_none() {
            Some(
                super::inputs::Session::new(&current, &keep_all_logs, cancel).map_err(
                    |reason| {
                        failed(
                            if cancel.load(Ordering::Relaxed) {
                                RetireCause::Cancelled
                            } else {
                                RetireCause::Incomplete(reason)
                            },
                            report.clone(),
                        )
                    },
                )?,
            )
        } else {
            None
        };
        let session = session.or(owned.as_mut()).expect("retention session");
        session
            .plan
            .check_packs_unchanged(cancel)
            .map_err(|error| failed(RetireCause::PackChanged(Box::new(error)), report.clone()))?;
        session.validate(&current, policy, cancel).map_err(|_| {
            failed(
                if cancel.load(Ordering::Relaxed) {
                    RetireCause::Cancelled
                } else {
                    RetireCause::Changed
                },
                report.clone(),
            )
        })?;
        session
            .plan
            .required
            .extend(session.plan.protected_objects.iter().copied());
        let first = &session.plan;
        let directory = current.object_dir().join("pack");
        let before = snapshot(&directory, policy.max_entries, cancel)
            .map_err(|cause| failed(cause, report.clone()))?;
        if old_indices(&before, first, None).is_empty()
            && protected_survivors_only(&current, first, policy, cancel)
                .map_err(|cause| failed(cause, report.clone()))?
        {
            session
                .plan
                .check_packs_unchanged(cancel)
                .map_err(|error| {
                    failed(RetireCause::PackChanged(Box::new(error)), report.clone())
                })?;
            session.validate(&current, policy, cancel).map_err(|_| {
                failed(
                    if cancel.load(Ordering::Relaxed) {
                        RetireCause::Cancelled
                    } else {
                        RetireCause::Changed
                    },
                    report.clone(),
                )
            })?;
            // No replacement or deletion needs a readability proof: all storage stays in place.
            return Ok(report);
        }
        // The isolated first scan is still current; the post-publication scan rechecks it.
        let (published, held) = current
            .publish_required(first, &keep_all_logs, limits, cancel)
            .map_err(|error| {
                if let RepackError::Bitmap { published, .. } = &error {
                    report.published = Some(**published);
                }
                failed(RetireCause::Repack(Box::new(error)), report.clone())
            })?;
        let replacement = directory.join(format!("pack-{}", published.written.checksum));
        let replacement_pack = replacement.with_extension("pack");
        let replacement_index = replacement.with_extension("idx");
        report.published = Some(published);
        // The publisher's final sync already made the pair's names durable in the pack
        // directory; this sync makes the pack directory's own name durable in case it was new.
        sync_directory(current.object_dir())
            .map_err(|error| failed(RetireCause::Io(error), report.clone()))?;
        report.durable = true;
        after_publication();
        check(cancel, &report)?;
        let refreshed = Repository::open(self.git_dir()).map_err(|error| {
            failed(
                RetireCause::Incomplete(RetentionError::read(error)),
                report.clone(),
            )
        })?;
        if refreshed.object_dir() != current.object_dir()
            || refreshed.object_format() != current.object_format()
            || !refreshed
                .shallow_roots()
                .iter()
                .eq(current.shallow_roots().iter())
        {
            return Err(failed(RetireCause::Changed, report));
        }
        let published_inputs =
            super::inputs::Inputs::observe(&refreshed, policy, cancel).map_err(|reason| {
                failed(
                    if cancel.load(Ordering::Relaxed) {
                        RetireCause::Cancelled
                    } else {
                        RetireCause::Incomplete(reason)
                    },
                    report.clone(),
                )
            })?;
        let mut replacement_paths = vec![replacement_pack.clone(), replacement_index.clone()];
        if matches!(published.bitmap, super::BitmapOutcome::Written(_)) {
            replacement_paths.push(replacement.with_extension("bitmap"));
            replacement_paths.push(replacement.with_extension("rev"));
        }
        if !session
            .inputs
            .same_except(&published_inputs, &replacement_paths)
        {
            return Err(failed(RetireCause::Changed, report));
        }
        session.inputs = published_inputs;
        let mut pack_budget = policy.packs;
        let pack = FilePack::open(
            self.object_format(),
            &replacement_index,
            &mut pack_budget,
            cancel,
        )
        .map_err(|error| failed(RetireCause::Read(Box::new(error)), report.clone()))?;
        // The index checksum and structure, including the offset order that gives each decoded
        // entry its exact end and CRC, are checked here. The whole-pack checksum and CRC pass
        // streams the file alongside the survivor decode below; both finish before any unlink.
        pack.verify_index()
            .map_err(|error| failed(RetireCause::Read(Box::new(error)), report.clone()))?;
        let retiring = old_indices(&before, first, Some(&replacement_pack));
        // Publishing already decodes protected survivors before omitting them. Decode them again
        // at the deletion boundary only when old pairs will actually be unlinked. The input
        // fingerprint is checked in either case; replacement objects are always verified below.
        let mut unverified = if retiring.is_empty() {
            BTreeSet::new()
        } else {
            held.clone()
        };
        for protected in &first.protected_packs {
            if unverified.is_empty() {
                break;
            }
            let index = protected.with_extension("idx");
            if !before.contains_key(&index) {
                continue;
            }
            let protected_pack =
                FilePack::open(self.object_format(), &index, &mut pack_budget, cancel)
                    .map_err(|error| failed(RetireCause::Read(Box::new(error)), report.clone()))?;
            let mut found = Vec::new();
            for &id in &unverified {
                check(cancel, &report)?;
                if let Some(position) = protected_pack
                    .find(id)
                    .map_err(|error| failed(RetireCause::Read(Box::new(error)), report.clone()))?
                {
                    let offset = protected_pack.offset(position).map_err(|error| {
                        failed(RetireCause::Read(Box::new(error)), report.clone())
                    })?;
                    found.push((offset, position, id));
                }
            }
            // Pack order lets the delta base caches serve each base once. Each read hashes the
            // payload against the identity at `position`, which `find` matched to `id`.
            found.sort_unstable();
            let positions: Vec<usize> = found.iter().map(|&(_, position, _)| position).collect();
            protected_pack
                .read_each(&positions, policy.read, policy.max_threads, cancel)
                .map_err(|error| failed(read_cause(error), report.clone()))?;
            for (_, _, id) in found {
                unverified.remove(&id);
            }
        }
        if let Some(id) = unverified.into_iter().next() {
            return Err(failed(RetireCause::ReplacementMissing(id), report));
        }
        let mut replaced = Vec::new();
        for id in first.required.difference(&held) {
            check(cancel, &report)?;
            let position = pack
                .find(*id)
                .map_err(|error| failed(RetireCause::Read(Box::new(error)), report.clone()))?
                .ok_or_else(|| failed(RetireCause::ReplacementMissing(*id), report.clone()))?;
            let offset = pack
                .offset(position)
                .map_err(|error| failed(RetireCause::Read(Box::new(error)), report.clone()))?;
            replaced.push((offset, position));
        }
        // Every retained object is decoded and identity-checked before anything is unlinked.
        // Each read hashes the payload against the identity `find` matched; pack order lets each
        // worker's delta base cache serve a base once instead of rebuilding chains in identity
        // order.
        replaced.sort_unstable();
        let positions: Vec<usize> = replaced.into_iter().map(|(_, position)| position).collect();
        let decode = || pack.read_each(&positions, policy.read, policy.max_threads, cancel);
        let verify = |result: Result<(), crate::ObjectReadError>| {
            result.map_err(|error| failed(read_cause(error), report.clone()))
        };
        let decoded = if policy.max_threads < 2 {
            verify(pack.verify(cancel))?;
            decode()
        } else {
            std::thread::scope(|scope| {
                let verified = scope.spawn(|| pack.verify(cancel));
                let decoded = decode();
                verify(
                    verified
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                )
                .map(|()| decoded)
            })?
        };
        decoded.map_err(|error| failed(read_cause(error), report.clone()))?;

        let after = snapshot(&directory, policy.max_entries, cancel)
            .map_err(|cause| failed(cause, report.clone()))?;
        if !same_old_generation(&before, &after, &replacement_paths) {
            return Err(failed(RetireCause::Changed, report));
        }
        session
            .plan
            .check_packs_unchanged(cancel)
            .map_err(|error| failed(RetireCause::PackChanged(Box::new(error)), report.clone()))?;
        session.validate(&refreshed, policy, cancel).map_err(|_| {
            failed(
                if cancel.load(Ordering::Relaxed) {
                    RetireCause::Cancelled
                } else {
                    RetireCause::Changed
                },
                report.clone(),
            )
        })?;
        if !retiring.is_empty() {
            // A multi-pack index (and its bitmaps) naming a retired pack would make the object
            // store unreadable to Git; Git rebuilds it when needed, as `git repack -d` does.
            let indices = multi_pack_index_paths(&directory)
                .map_err(|error| failed(RetireCause::Io(error), report.clone()))?;
            let removed_index = !indices.is_empty();
            for path in indices {
                pack.check_unchanged().map_err(|error| {
                    failed(RetireCause::PackChanged(Box::new(error)), report.clone())
                })?;
                session
                    .plan
                    .check_packs_unchanged(cancel)
                    .map_err(|error| {
                        failed(RetireCause::PackChanged(Box::new(error)), report.clone())
                    })?;
                let result = if path.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    remove(&path)
                };
                result.map_err(|error| RetireFailure {
                    cause: RetireCause::Io(error),
                    report: Box::new(report.clone()),
                    uncertain: Some(path.clone()),
                })?;
                report.removed.push(path);
            }
            if removed_index {
                // Ordering: the multi-pack index must be durably gone before any pack it names.
                sync_directory(&directory).map_err(|error| RetireFailure {
                    cause: RetireCause::Io(error),
                    report: Box::new(report.clone()),
                    uncertain: Some(directory.clone()),
                })?;
            }
        }
        for index in retiring {
            check(cancel, &report)?;
            let artifacts = retirable_paths(&index, &before);
            for path in &artifacts {
                recheck(path, before[path]).map_err(|cause| failed(cause, report.clone()))?;
            }
            for path in artifacts {
                pack.check_unchanged().map_err(|error| {
                    failed(RetireCause::PackChanged(Box::new(error)), report.clone())
                })?;
                session
                    .plan
                    .check_packs_unchanged(cancel)
                    .map_err(|error| {
                        failed(RetireCause::PackChanged(Box::new(error)), report.clone())
                    })?;
                remove(&path).map_err(|error| RetireFailure {
                    cause: RetireCause::Io(error),
                    report: Box::new(report.clone()),
                    uncertain: Some(path.clone()),
                })?;
                let index_removed = path == index;
                report.removed.push(path);
                if index_removed {
                    // Ordering: the index unlink must be durable before the pack goes, so a crash
                    // never leaves a visible index naming a missing pack.
                    sync_directory(&directory).map_err(|error| RetireFailure {
                        cause: RetireCause::Io(error),
                        report: Box::new(report.clone()),
                        uncertain: Some(index.clone()),
                    })?;
                }
            }
            // No sync after the pack unlink: if a crash undoes it, the pack has no index and
            // readers ignore it. The next pair's index sync persists it.
        }
        Ok(report)
    }
}

#[cfg(unix)]
pub(super) fn snapshot(
    directory: &Path,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<BTreeMap<PathBuf, Stamp>, RetireCause> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(RetireCause::Io(error)),
    };
    let mut files = BTreeMap::new();
    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            return Err(RetireCause::Cancelled);
        }
        if files.len() >= limit {
            return Err(RetireCause::Incomplete(
                RetentionError::PackDirectoryEntryLimit,
            ));
        }
        let path = entry.map_err(RetireCause::Io)?.path();
        let metadata = fs::symlink_metadata(&path).map_err(RetireCause::Io)?;
        if !metadata.is_file() {
            return Err(RetireCause::Incomplete(RetentionError::NonFilePackEntry(
                path.clone(),
            )));
        }
        files.insert(
            path,
            Stamp {
                len: metadata.len(),
                modified: metadata.modified().map_err(RetireCause::Io)?,
            },
        );
    }
    Ok(files)
}

#[cfg(unix)]
pub(super) fn same_old_generation(
    before: &BTreeMap<PathBuf, Stamp>,
    after: &BTreeMap<PathBuf, Stamp>,
    replacement_paths: &[PathBuf],
) -> bool {
    before
        .iter()
        .filter(|(path, _)| {
            !replacement_paths
                .iter()
                .any(|replacement| path.as_path() == replacement)
        })
        .eq(after.iter().filter(|(path, _)| {
            !replacement_paths
                .iter()
                .any(|replacement| path.as_path() == replacement)
        }))
}

/// A location-only proof is sufficient for a read-only result. It never authorizes omission
/// from a replacement or deletion; those paths retain their full decode proofs.
#[cfg(unix)]
fn protected_survivors_only(
    repository: &Repository,
    plan: &RetentionPlan,
    policy: &RetentionPolicy,
    cancel: &AtomicBool,
) -> Result<bool, RetireCause> {
    if plan.protected_packs.is_empty() {
        return Ok(false);
    }
    let objects = repository
        .objects_controlled(policy.packs, crate::AlternateLimits::default(), cancel)
        .map_err(|error| RetireCause::Read(Box::new(error)))?;
    let primary = objects.primary_packs_with_paths();
    let protected: Vec<_> = primary
        .into_iter()
        .filter(|(path, _)| plan.protected_packs.contains(*path))
        .collect();
    for &id in &plan.required {
        if cancel.load(Ordering::Relaxed) {
            return Err(RetireCause::Cancelled);
        }
        let mut found = false;
        for (_, pack) in &protected {
            if pack
                .find(id)
                .map_err(|error| RetireCause::Read(Box::new(error)))?
                .is_some()
            {
                found = true;
                break;
            }
        }
        if !found {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(unix)]
fn old_indices(
    before: &BTreeMap<PathBuf, Stamp>,
    plan: &RetentionPlan,
    new_pack: Option<&Path>,
) -> Vec<PathBuf> {
    before
        .keys()
        .filter(|path| path.extension().is_some_and(|ext| ext == "idx"))
        .filter(|index| {
            let pack = index.with_extension("pack");
            new_pack != Some(pack.as_path())
                && before.contains_key(&pack)
                && !plan.protected_packs.contains(&pack)
                && !before.keys().any(|path| {
                    path.file_stem() == pack.file_stem()
                        && !matches!(
                            path.extension().and_then(|ext| ext.to_str()),
                            Some("idx" | "pack" | "rev" | "bitmap")
                        )
                })
        })
        .cloned()
        .collect()
}

#[cfg(unix)]
fn retirable_paths(index: &Path, before: &BTreeMap<PathBuf, Stamp>) -> Vec<PathBuf> {
    let mut paths = vec![index.to_path_buf()];
    for extension in ["rev", "bitmap"] {
        let path = index.with_extension(extension);
        if before.contains_key(&path) {
            paths.push(path);
        }
    }
    paths.push(index.with_extension("pack"));
    paths
}

#[cfg(unix)]
pub(super) fn recheck(path: &Path, expected: Stamp) -> Result<(), RetireCause> {
    let metadata = fs::symlink_metadata(path).map_err(RetireCause::Io)?;
    if !metadata.is_file()
        || metadata.len() != expected.len
        || metadata.modified().map_err(RetireCause::Io)? != expected.modified
    {
        return Err(RetireCause::Changed);
    }
    Ok(())
}

// Cancellation keeps its own cause, as the per-object checks this replaced reported it.
#[cfg(unix)]
pub(super) fn read_cause(error: crate::ObjectReadError) -> RetireCause {
    match error {
        crate::ObjectReadError::Cancelled => RetireCause::Cancelled,
        error => RetireCause::Read(Box::new(error)),
    }
}

fn check(cancel: &AtomicBool, report: &RetireReport) -> Result<(), RetireFailure> {
    if cancel.load(Ordering::Relaxed) {
        Err(failed(RetireCause::Cancelled, report.clone()))
    } else {
        Ok(())
    }
}

fn failed(cause: RetireCause, report: RetireReport) -> RetireFailure {
    RetireFailure {
        cause,
        report: Box::new(report),
        uncertain: None,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use super::*;

    fn git(root: &Path, args: &[&str], input: &[u8]) -> String {
        let mut child = Command::new("git")
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().into()
    }

    fn fixture() -> (tempfile::TempDir, Repository, RetentionPolicy, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--bare", "--template=", "."], b"");
        let tree = git(root.path(), &["mktree"], b"");
        let commit = git(root.path(), &["commit-tree", &tree], b"one\n");
        git(
            root.path(),
            &["update-ref", "refs/heads/main", &commit],
            b"",
        );
        git(
            root.path(),
            &["repack", "-ad", "--no-write-bitmap-index"],
            b"",
        );
        // A loose descendant makes the replacement differ from the old pack.
        let child = git(
            root.path(),
            &["commit-tree", &tree, "-p", &commit],
            b"two\n",
        );
        git(root.path(), &["update-ref", "refs/heads/main", &child], b"");
        let repository = Repository::open(root.path()).unwrap();
        let directory = repository.object_dir().join("pack");
        let old_index = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "idx"))
            .unwrap();
        let policy = RetentionPolicy {
            recent_cutoff: Some(SystemTime::now() + Duration::from_secs(60)),
            ..Default::default()
        };
        (root, repository, policy, old_index)
    }

    #[test]
    fn retiring_packs_removes_multi_pack_index_so_git_can_read_the_store() {
        let (root, _repository, policy, _old_index) = fixture();
        let tree = git(root.path(), &["mktree"], b"");
        let parent = git(root.path(), &["rev-parse", "refs/heads/main"], b"");
        let commit = git(
            root.path(),
            &["commit-tree", &tree, "-p", &parent],
            b"two\n",
        );
        git(
            root.path(),
            &["update-ref", "refs/heads/main", &commit],
            b"",
        );
        git(
            root.path(),
            &["repack", "-d", "--no-write-bitmap-index"],
            b"",
        );
        git(root.path(), &["multi-pack-index", "write"], b"");
        let repository = Repository::open(root.path()).unwrap();
        let report = repository
            .retire_old_packs_exclusive(&policy, RepackLimits::default(), &AtomicBool::new(false))
            .unwrap();
        let midx = repository.object_dir().join("pack/multi-pack-index");
        assert!(report.removed.contains(&midx));
        assert!(!midx.exists());
        git(root.path(), &["fsck", "--strict"], b"");
    }

    #[test]
    fn new_root_after_publication_refuses_old_pack_deletion() {
        let (root, repository, policy, old_index) = fixture();
        let result = repository.retire_old_packs_at(
            &policy,
            RepackLimits::default(),
            &AtomicBool::new(false),
            || {
                let id = git(root.path(), &["hash-object", "-w", "--stdin"], b"new");
                git(root.path(), &["update-ref", "refs/tags/new", &id], b"");
            },
            |path| fs::remove_file(path),
            |directory| File::open(directory).and_then(|dir| dir.sync_all()),
        );
        let error = result.unwrap_err();
        assert!(matches!(error.cause, RetireCause::Changed));
        assert!(error.report.published.is_some());
        assert!(error.report.removed.is_empty());
        assert!(old_index.exists());
    }

    #[test]
    fn changed_protection_is_refused_before_survivor_verification() {
        let (_root, repository, policy, old_index) = fixture();
        let keep = old_index.with_extension("keep");
        fs::write(&keep, b"").unwrap();
        let error = repository
            .retire_old_packs_at(
                &policy,
                RepackLimits::default(),
                &AtomicBool::new(false),
                || {
                    fs::remove_file(&keep).unwrap();
                },
                |path| fs::remove_file(path),
                |directory| File::open(directory).and_then(|dir| dir.sync_all()),
            )
            .unwrap_err();
        assert!(matches!(error.cause, RetireCause::Changed));
        assert!(error.report.removed.is_empty());
        assert!(old_index.exists());
    }

    #[test]
    fn cancellation_after_publication_preserves_old_pack() {
        let (_root, repository, policy, old_index) = fixture();
        let cancel = AtomicBool::new(false);
        let error = repository
            .retire_old_packs_at(
                &policy,
                RepackLimits::default(),
                &cancel,
                || cancel.store(true, Ordering::Relaxed),
                |path| fs::remove_file(path),
                |directory| File::open(directory).and_then(|dir| dir.sync_all()),
            )
            .unwrap_err();
        assert!(matches!(error.cause, RetireCause::Cancelled));
        assert!(error.report.published.is_some());
        assert!(error.report.removed.is_empty());
        assert!(old_index.exists());
    }

    /// Survivors decode on several workers. Damage that only an identity check finds, in the
    /// entries a later worker decodes, still refuses before any old pack is unlinked.
    #[test]
    fn replacement_damage_found_by_a_decode_worker_keeps_old_pack() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--bare", "--template=", "."], b"");
        // More blobs than one decode worker's run, so several workers verify survivors.
        let mut stream = String::new();
        for n in 1..=3000 {
            let data = format!("blob {n}\n");
            stream += &format!("blob\nmark :{n}\ndata {}\n{data}\n", data.len());
        }
        stream += "commit refs/heads/main\ncommitter A <a@example.com> 1700000000 +0000\n";
        stream += "data 4\none\n";
        for n in 1..=3000 {
            stream += &format!("M 100644 :{n} f{n}\n");
        }
        git(root.path(), &["fast-import", "--quiet"], stream.as_bytes());
        let tree = git(root.path(), &["rev-parse", "refs/heads/main^{tree}"], b"");
        let commit = git(root.path(), &["rev-parse", "refs/heads/main"], b"");
        git(
            root.path(),
            &["repack", "-ad", "--no-write-bitmap-index"],
            b"",
        );
        // A loose descendant makes the replacement differ from the old pack.
        let child = git(
            root.path(),
            &["commit-tree", &tree, "-p", &commit],
            b"two\n",
        );
        git(root.path(), &["update-ref", "refs/heads/main", &child], b"");
        let repository = Repository::open(root.path()).unwrap();
        let directory = repository.object_dir().join("pack");
        let indexes = || -> BTreeSet<PathBuf> {
            fs::read_dir(&directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "idx"))
                .collect()
        };
        let old = indexes();
        let policy = RetentionPolicy {
            recent_cutoff: Some(SystemTime::now() + Duration::from_secs(60)),
            max_threads: 4,
            ..Default::default()
        };
        let error = repository
            .retire_old_packs_at(
                &policy,
                RepackLimits::default(),
                &AtomicBool::new(false),
                || {
                    let replacement = indexes().difference(&old).next().unwrap().clone();
                    crate::pack::damage::swap_last_entries(&replacement, crate::ObjectFormat::Sha1);
                },
                |path| fs::remove_file(path),
                |directory| File::open(directory).and_then(|dir| dir.sync_all()),
            )
            .unwrap_err();
        assert!(matches!(error.cause, RetireCause::Read(_)), "{error:?}");
        assert!(error.report.published.is_some());
        assert!(error.report.removed.is_empty());
        assert!(old.iter().all(|index| index.exists()));
        // Readers that prefer the old pack still see every object.
        for index in indexes().difference(&old) {
            fs::remove_file(index).unwrap();
        }
        git(root.path(), &["fsck", "--strict"], b"");
    }

    #[test]
    fn unlink_fault_reports_removed_index_and_uncertain_sidecar() {
        let (root, repository, policy, old_index) = fixture();
        let mut calls = 0;
        let error = repository
            .retire_old_packs_at(
                &policy,
                RepackLimits::default(),
                &AtomicBool::new(false),
                || {},
                |path| {
                    calls += 1;
                    if calls == 2 {
                        Err(io::ErrorKind::PermissionDenied.into())
                    } else {
                        fs::remove_file(path)
                    }
                },
                |directory| File::open(directory).and_then(|dir| dir.sync_all()),
            )
            .unwrap_err();
        assert!(matches!(error.cause, RetireCause::Io(_)));
        assert!(error.report.published.is_some());
        assert_eq!(error.report.removed, std::slice::from_ref(&old_index));
        assert!(error.uncertain.is_some());
        let tip = git(root.path(), &["rev-parse", "refs/heads/main"], b"");
        assert_eq!(git(root.path(), &["cat-file", "-t", &tip], b""), "commit");
    }

    // Fault model: unsynchronized directory changes may persist in either order. Replay the
    // index unlink as lost only if no directory sync separated it from the pack unlink.
    // This tests the algorithm under that model, not power-loss behavior of a particular
    // filesystem.
    #[test]
    fn unsynced_index_reappearance_does_not_break_cold_open() {
        use std::cell::Cell;
        let (root, repository, mut policy, old_index) = fixture();
        let extra = git(
            root.path(),
            &["hash-object", "-w", "--stdin"],
            b"fault model extra",
        );
        policy.heads.push(extra.parse().unwrap());
        let saved_index = fs::read(&old_index).unwrap();
        let pending_index_unlink = Cell::new(false);
        let replayed = Cell::new(false);
        let result = repository.retire_old_packs_at(
            &policy,
            RepackLimits::default(),
            &AtomicBool::new(false),
            || {},
            |path| {
                fs::remove_file(path)?;
                if path == old_index {
                    pending_index_unlink.set(true);
                }
                if path == old_index.with_extension("pack") && pending_index_unlink.get() {
                    fs::write(&old_index, &saved_index)?;
                    replayed.set(true);
                    return Err(io::Error::other("simulated crash persistence cut"));
                }
                Ok(())
            },
            |directory| {
                File::open(directory)?.sync_all()?;
                pending_index_unlink.set(false);
                Ok(())
            },
        );
        eprintln!(
            "persistence replay={}, retirement={result:?}",
            replayed.get()
        );
        assert!(
            !old_index.with_extension("pack").exists(),
            "fixture must retire the original pack"
        );
        let fresh = Repository::open(root.path()).unwrap();
        assert!(
            fresh.objects_with(crate::PackLimits::default()).is_ok(),
            "cold open failed after unsynced index unlink was lost but pack unlink persisted"
        );
    }

    #[test]
    fn publication_disk_full_sync_failure_retains_old_pair_and_reports_visible_new_pair() {
        let (_root, repository, policy, old_index) = fixture();
        let error = repository
            .retire_old_packs_at(
                &policy,
                RepackLimits::default(),
                &AtomicBool::new(false),
                || {},
                |path| fs::remove_file(path),
                |_| Err(io::Error::from_raw_os_error(28)),
            )
            .unwrap_err();
        assert!(matches!(error.cause, RetireCause::Io(_)));
        assert!(error.report.published.is_some());
        assert!(!error.report.durable);
        assert!(error.report.removed.is_empty());
        assert!(old_index.exists());
    }

    /// Three history packs, a recent unreachable blob's pack and a `.keep` pack, all newer than
    /// any cutoff, so consolidation folds every pack except the `.keep` one.
    fn consolidation_fixture() -> (tempfile::TempDir, Vec<String>) {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--bare", "--template=", "."], b"");
        let keep = git(root.path(), &["hash-object", "-w", "--stdin"], b"kept\n");
        let checksum = git(
            root.path(),
            &[
                "-c",
                "pack.writeReverseIndex=true",
                "pack-objects",
                "-q",
                "objects/pack/pack",
            ],
            format!("{keep}\n").as_bytes(),
        );
        fs::write(
            root.path()
                .join(format!("objects/pack/pack-{checksum}.keep")),
            b"",
        )
        .unwrap();
        let mut parent: Option<String> = None;
        for number in 0..3 {
            let blob = git(
                root.path(),
                &["hash-object", "-w", "--stdin"],
                format!("content {number}\n").as_bytes(),
            );
            let tree = git(
                root.path(),
                &["mktree"],
                format!("100644 blob {blob}\tfile\n").as_bytes(),
            );
            let mut args = vec!["commit-tree", tree.as_str()];
            if let Some(parent) = &parent {
                args.extend(["-p", parent.as_str()]);
            }
            let commit = git(root.path(), &args, b"commit\n");
            git(
                root.path(),
                &["update-ref", "refs/heads/main", &commit],
                b"",
            );
            git(
                root.path(),
                &[
                    "pack-objects",
                    "-q",
                    "--revs",
                    "--incremental",
                    "objects/pack/pack",
                ],
                b"refs/heads/main\n",
            );
            parent = Some(commit);
        }
        let unreachable = git(
            root.path(),
            &["hash-object", "-w", "--stdin"],
            b"unreachable\n",
        );
        git(
            root.path(),
            &[
                "-c",
                "pack.writeReverseIndex=true",
                "pack-objects",
                "-q",
                "objects/pack/pack",
            ],
            format!("{unreachable}\n").as_bytes(),
        );
        git(root.path(), &["prune-packed"], b"");
        let objects = git(
            root.path(),
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)",
            ],
            b"",
        );
        (root, objects.lines().map(str::to_owned).collect())
    }

    fn consolidate_at(
        repository: &Repository,
        remove: impl FnMut(&Path) -> io::Result<()>,
        sync_directory: impl FnMut(&Path) -> io::Result<()>,
    ) -> Result<RetireReport, RetireFailure> {
        let policy = RetentionPolicy::default();
        let cancel = AtomicBool::new(false);
        let mut session =
            super::super::inputs::Session::new_folding(repository, &policy, true, &cancel).unwrap();
        repository.retire_planned_at(
            &policy,
            RepackLimits::default(),
            &cancel,
            Some(&mut session),
            || {},
            (remove, sync_directory),
        )
    }

    /// Runs consolidation until an injected failure at the given removal or directory-sync call.
    /// Removal order per old pack is index, directory sync, reverse index, pack. The publisher
    /// syncs the pack directory itself, so the first injected sync here is the object directory's.
    fn crash_consolidation_at(
        fail_remove: Option<usize>,
        fail_sync: Option<usize>,
    ) -> (tempfile::TempDir, Vec<String>, Option<PathBuf>) {
        let (root, before) = consolidation_fixture();
        let repository = Repository::open(root.path()).unwrap();
        let (mut removes, mut syncs) = (0, 0);
        let error = consolidate_at(
            &repository,
            |path| {
                removes += 1;
                if Some(removes) == fail_remove {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                fs::remove_file(path)
            },
            |directory| {
                syncs += 1;
                if Some(syncs) == fail_sync {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                File::open(directory).and_then(|dir| dir.sync_all())
            },
        )
        .unwrap_err();
        assert!(matches!(error.cause, RetireCause::Io(_)));
        assert!(error.report.published.is_some());
        (root, before, error.uncertain)
    }

    // A crash at each removal or directory-sync boundary leaves every object readable by Git and
    // by a fresh girt store.
    // `uncertain` is the extension of the path the failure report names: the artifact whose
    // removal failed, the index whose unlink was not synced, or the pack directory otherwise.
    #[rstest::rstest]
    #[case::first_index_unlink(Some(1), None, "idx")]
    #[case::first_reverse_index_unlink(Some(2), None, "rev")]
    #[case::first_pack_unlink(Some(3), None, "pack")]
    #[case::second_index_unlink(Some(4), None, "idx")]
    #[case::object_directory_sync_after_publication(None, Some(1), "")]
    #[case::sync_after_first_index_unlink(None, Some(2), "idx")]
    #[case::sync_after_second_index_unlink(None, Some(3), "idx")]
    fn consolidation_crash_points_keep_every_object_readable(
        #[case] fail_remove: Option<usize>,
        #[case] fail_sync: Option<usize>,
        #[case] uncertain: &str,
    ) {
        let (root, before, failed_at) = crash_consolidation_at(fail_remove, fail_sync);
        let failed_extension = failed_at
            .as_deref()
            .and_then(Path::extension)
            .map(|ext| ext.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert_eq!(failed_extension, uncertain);

        git(root.path(), &["fsck", "--full", "--strict"], b"");
        let present = git(
            root.path(),
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)",
            ],
            b"",
        );
        assert_eq!(present.lines().collect::<Vec<_>>(), before);
        let objects = Repository::open(root.path())
            .unwrap()
            .objects_with(crate::PackLimits::default())
            .unwrap();
        let unreadable: Vec<_> = before
            .iter()
            .filter(|id| {
                objects
                    .read_with(id.parse().unwrap(), crate::ReadLimits::default())
                    .ok()
                    .flatten()
                    .is_none()
            })
            .collect();
        assert!(unreadable.is_empty(), "{unreadable:?}");
    }

    /// A crash right after an index unlink is synced leaves an orphan: an old `.pack` with no
    /// index. Readers ignore it and every object stays readable after a cold reopen. While it is
    /// recent, planning refuses it as a protected pack without an index, so later maintenance
    /// changes nothing; once it is older than the cutoff, retirement and garbage collection
    /// complete, Git's fsck stays clean, and the orphan file remains.
    #[rstest::rstest]
    #[case::orphan_after_first_index_unlink(Some(2))]
    #[case::orphan_after_second_index_unlink(Some(5))]
    fn orphan_pack_after_index_unlink_crash_is_safe(#[case] fail_remove: Option<usize>) {
        let (root, before, _) = crash_consolidation_at(fail_remove, None);
        let orphans: Vec<PathBuf> = fs::read_dir(root.path().join("objects/pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "pack")
                    && !path.with_extension("idx").exists()
            })
            .collect();
        assert_eq!(orphans.len(), 1, "{orphans:?}");
        let readable = |root: &Path, stage: &str| {
            let objects = Repository::open(root)
                .unwrap()
                .objects_with(Default::default())
                .unwrap();
            // Before maintenance runs again every object survives; afterwards, with a cutoff that
            // makes the fixture's unreachable blob garbage, every reachable object does.
            let expected: Vec<String> = if matches!(stage, "crash" | "refused") {
                before.clone()
            } else {
                git(root, &["rev-list", "--objects", "--all"], b"")
                    .lines()
                    .map(|line| line.split_whitespace().next().unwrap().to_owned())
                    .collect()
            };
            for id in &expected {
                let id: ObjectId = id.parse().unwrap();
                assert!(
                    objects.read_with(id, Default::default()).unwrap().is_some(),
                    "{stage}: {id} unreadable"
                );
            }
            git(root, &["fsck", "--full", "--strict"], b"");
        };
        readable(root.path(), "crash");

        // Recent orphan: planning refuses and nothing changes.
        let repository = Repository::open(root.path()).unwrap();
        let cancel = AtomicBool::new(false);
        let error = super::super::inputs::Session::new_folding(
            &repository,
            &RetentionPolicy::default(),
            true,
            &cancel,
        )
        .err()
        .unwrap();
        assert!(
            matches!(error, RetentionError::MissingProtectedIndex),
            "{error:?}"
        );
        assert!(orphans[0].exists());
        readable(root.path(), "refused");

        // Aged orphan: retirement and garbage collection complete.
        let cutoff = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        let policy = RetentionPolicy::default().with_recent_cutoff(cutoff);
        repository
            .retire_old_packs_exclusive(&policy, RepackLimits::default(), &cancel)
            .unwrap();
        readable(root.path(), "retired");
        struct Private;
        impl MaintenanceIsolation for Private {
            type Guard = ();
            fn acquire(&mut self, _: &Repository) -> io::Result<()> {
                Ok(())
            }
        }
        Repository::open(root.path())
            .unwrap()
            .collect_garbage(&mut Private, &policy, &cancel)
            .unwrap();
        readable(root.path(), "collected");
        // Nothing removes the orphan: girt maintenance lists packs by index, and so does Git's gc.
        // It stays an inert file that readers ignore.
        assert!(orphans[0].exists());
        git(root.path(), &["gc", "-q", "--prune=now"], b"");
        assert!(orphans[0].exists());
        readable(root.path(), "git gc");
    }

    // A crash that leaves no unindexed old pack is finished by the next consolidation. An
    // unindexed recent pack instead stays protected, so planning refuses it until it ages.
    #[rstest::rstest]
    #[case::first_index_unlink(Some(1), None)]
    #[case::second_index_unlink(Some(4), None)]
    #[case::object_directory_sync_after_publication(None, Some(1))]
    fn next_consolidation_finishes_after_a_crash(
        #[case] fail_remove: Option<usize>,
        #[case] fail_sync: Option<usize>,
    ) {
        let (root, before, _) = crash_consolidation_at(fail_remove, fail_sync);

        let repository = Repository::open(root.path()).unwrap();
        consolidate_at(
            &repository,
            |path| fs::remove_file(path),
            |directory| File::open(directory).and_then(|dir| dir.sync_all()),
        )
        .unwrap();

        let packs = fs::read_dir(root.path().join("objects/pack"))
            .unwrap()
            .filter(|entry| {
                let path = entry.as_ref().unwrap().path();
                path.extension().is_some_and(|ext| ext == "pack")
            })
            .count();
        assert_eq!(packs, 2, "the .keep pack and one consolidated pack");
        let present = git(
            root.path(),
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)",
            ],
            b"",
        );
        assert_eq!(present.lines().collect::<Vec<_>>(), before);
        git(root.path(), &["fsck", "--full", "--strict"], b"");
    }
}

/// Multi-pack index files in a pack directory: the index, its bitmaps and reverse indexes, and an
/// incremental chain directory.
#[cfg(unix)]
pub(super) fn multi_pack_index_paths(directory: &Path) -> io::Result<Vec<std::path::PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.as_encoded_bytes().starts_with(b"multi-pack-index") {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}
