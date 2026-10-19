use std::fs;
use std::io::{self, BufWriter};
#[cfg(test)]
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use tempfile::NamedTempFile;

use super::{RetentionError, RetentionPolicy};
use crate::pack::publish::{Pair, Progress, Step, pair_checked as publish_pair_checked};
use crate::pack::{
    PackObject, PackWriteError, PackWriteLimits, PackWritten, Packed, ReuseError, write_reusing,
};
use crate::{DeltaOptions, ObjectId, ObjectReadError, PackCompression, Repository};

/// Write buffer for a new pack: copied entries arrive a few hundred bytes at a time, and the
/// default 8 KiB buffer would make one write call per few dozen of them.
const PACK_BUFFER: usize = 1024 * 1024;

/// Bounds one additive repack.
///
/// Entries copied from existing packs stream from their files. Objects that are not packed yet
/// remain decoded in memory until the pack is written.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct RepackLimits {
    /// Bounds object count, logical payload bytes (including copied objects), and output
    /// artifacts.
    pub write: PackWriteLimits,
    /// Selects delta search among objects not already packed (default: bounded deltas).
    pub compression: PackCompression,
    /// Whether to write a pack bitmap. `None` uses the last resolved assignment of
    /// `repack.writeBitmaps` or its deprecated `pack.writeBitmaps` synonym, defaulting to
    /// false for both bare and nonbare repositories. Unlike Git, bare repositories require an
    /// explicit opt-in until bitmap writing costs at most about 1.5 times a plain repack.
    /// Configured/default limit exhaustion skips
    /// optional output; `Some(true)` requests strict writing and returns bitmap limit errors.
    pub write_bitmaps: Option<bool>,
    /// Independent graph, traversal and artifact bounds for optional bitmap output.
    pub bitmap: crate::bitmap::WriteLimits,
}

impl RepackLimits {
    /// Sets [`Self::write`].
    #[must_use]
    pub fn with_write(mut self, write: PackWriteLimits) -> Self {
        self.write = write;
        self
    }

    /// Sets [`Self::compression`].
    #[must_use]
    pub fn with_compression(mut self, compression: PackCompression) -> Self {
        self.compression = compression;
        self
    }
}

impl Default for RepackLimits {
    fn default() -> Self {
        Self {
            write: PackWriteLimits::default(),
            compression: PackCompression::Delta(DeltaOptions::DEFAULT),
            write_bitmaps: None,
            bitmap: crate::bitmap::WriteLimits::default(),
        }
    }
}

impl RepackLimits {
    /// Returns limits for a repository the caller trusts as Git trusts the user's own.
    ///
    /// Git bounds none of these values. The defaults suit untrusted input; these are large enough
    /// that only available memory bounds the work, while keeping headroom for internal arithmetic.
    /// Bitmap graph/input/traversal/output bounds are also effectively unbounded. Encoding retains
    /// the object graph, one selected-commit bitset at a time, and a traversal stack; choose
    /// explicit bitmap limits when the repository or available memory is not trusted.
    pub const fn trusted() -> Self {
        Self {
            write: PackWriteLimits::trusted(),
            compression: PackCompression::Delta(DeltaOptions::DEFAULT),
            write_bitmaps: None,
            bitmap: crate::bitmap::WriteLimits {
                max_objects: u32::MAX as usize,
                max_packs: u32::MAX as usize,
                max_edges: usize::MAX,
                max_input_bytes: u64::MAX,
                max_visits: usize::MAX,
                max_bytes: usize::MAX,
                read: crate::ReadLimits::trusted(),
            },
        }
    }
}

/// A complete indexed pack is visible; existing storage has not been removed.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct RepackPublished {
    /// Identity, count and artifact sizes of the newly published or reused pair.
    pub written: PackWritten,
    /// Optional bitmap outcome. A completed pack pair remains visible even if later bitmap work
    /// fails; see [`RepackError::Bitmap`].
    pub bitmap: BitmapOutcome,
}

/// Why an additive replacement has, or does not have, a bitmap index.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum BitmapOutcome {
    /// Configuration or the caller disabled writing.
    Disabled,
    /// Complete bitmap and reverse-index sidecars are visible.
    Written(crate::bitmap::Published),
    /// Shallow parent boundaries cannot establish a full-DAG bitmap.
    SkippedShallow,
    /// Protected pack survivors were omitted from this replacement.
    SkippedProtected,
    /// Optional configured/default output exhausted a resource bound; maintenance continues.
    SkippedLimit {
        /// Bound that prevented bitmap output.
        limit: &'static str,
    },
    /// Bitmap work failed after complete pair publication; the typed error carries its cause.
    Failed,
    /// A partial clone's packs omit promised objects, so no full-DAG bitmap can be established.
    SkippedPartialClone,
}

/// How far one leg's publication got, kept when a later step fails.
#[derive(Debug, Default)]
pub(super) struct LegProgress {
    /// The written pack's identity and sizes, once its temporary files are complete.
    #[cfg_attr(
        not(unix),
        allow(dead_code, reason = "only Unix isolated repack reports partial legs")
    )]
    pub(super) written: Option<PackWritten>,
    /// Names the publisher made visible and the name whose sync failed.
    pub(super) publish: Progress,
}

/// One pack a repack publishes: its objects and whether it is a promisor pack.
pub(super) struct Leg<'a> {
    pub(super) ids: &'a std::collections::BTreeSet<ObjectId>,
    /// `.promisor` marker contents, published durably before the pack.
    pub(super) marker: Option<&'a [u8]>,
    /// The repository is a partial clone, so no bitmap is written.
    pub(super) partial_clone: bool,
}

/// Repack failure, retaining complete pair publication when later bitmap work fails.
/// An unindexed pack may remain after interruption before pair completion.
///
/// This operation leaves old storage intact. An incomplete plan needs its typed retention cause
/// resolved; missing/corrupt input needs recovery. A publication failure may leave an unindexed
/// pack that readers ignore; do not manually delete checksum-named files without fresh ownership
/// and dependency checks. A successful additive repack is never deletion authorization.
///
/// ```
/// use girt::{InitKind, ObjectFormat, Repository};
/// use girt::retention::{RepackError, RepackLimits, RetentionError, RetentionPolicy};
///
/// let directory = tempfile::tempdir()?;
/// let path = directory.path().join("r");
/// let repo = Repository::init(ObjectFormat::Sha1, path, InitKind::Bare)?;
///
/// let missing = girt::ObjectId::for_blob(ObjectFormat::Sha1, b"not stored");
/// let policy = RetentionPolicy::default().with_heads(vec![missing]);
/// let error = repo.repack_retained(&policy, RepackLimits::default(),
///     &std::sync::atomic::AtomicBool::new(false)).unwrap_err();
/// assert!(matches!(
///     error,
///     RepackError::Incomplete(RetentionError::MissingObject(id)) if id == missing
/// ));
/// // No complete plan existed, so no replacement pair could be published.
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RepackError {
    /// Invalid bitmap configuration; checked before pack publication.
    #[error("invalid bitmap writing configuration")]
    BitmapConfiguration(#[from] crate::config::InvalidValue),
    /// A complete pair was published, but optional bitmap work failed. Old storage stays intact.
    #[error("bitmap writing failed after repack publication")]
    Bitmap {
        /// Pair that is already visible, with bitmap outcome `Failed`.
        published: Box<RepackPublished>,
        /// Concrete publication/encoding error and bitmap visibility state.
        #[source]
        source: Box<crate::bitmap::InstallError>,
    },
    /// A fresh retention scan did not establish complete reachability.
    #[error("retention scan incomplete")]
    Incomplete(#[source] RetentionError),
    /// A required object disappeared between planning and reading.
    #[error("required object missing: {0}")]
    Missing(ObjectId),
    /// A required object could not be read.
    #[error("cannot read required object {id}")]
    Read {
        /// Object requested by the plan.
        id: ObjectId,
        /// Storage error.
        #[source]
        source: ObjectReadError,
    },
    /// Object storage changed or could not be opened.
    #[error(transparent)]
    Open(#[from] ObjectReadError),
    /// Input or output exceeded the configured bounds, or artifact writing failed.
    #[error(transparent)]
    Write(#[from] PackWriteError),
    /// The caller cancelled before complete pair publication.
    #[error("repack cancelled")]
    Cancelled,
    /// An existing checksum-named artifact differs from the generated bytes.
    #[error("conflicting existing pack artifact: {0}")]
    Existing(std::path::PathBuf),
    /// Temporary artifact creation, synchronization or publication failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<crate::pack::publish::Error> for RepackError {
    fn from(error: crate::pack::publish::Error) -> Self {
        match error {
            crate::pack::publish::Error::Cancelled => Self::Cancelled,
            crate::pack::publish::Error::Existing(path) => Self::Existing(path),
            crate::pack::publish::Error::Io(error) => Self::Io(error),
        }
    }
}

impl Repository {
    /// Publishes a pack of objects required by a fresh complete retention scan.
    ///
    /// This additive operation rescans roots and recent objects instead of trusting an older
    /// [`super::RetentionPlan`]. A required object already in a pinned pack has its entry copied
    /// without inflating it, in source pack order; a delta keeps its compressed instructions when
    /// its base entry from the same pack is also copied, and is otherwise rebuilt as a whole
    /// object. The scan decodes graph-bearing objects and locates blobs; the writer validates
    /// copied entry CRCs and bounds, while retirement separately verifies surviving identities.
    /// Stored entries come only from primary
    /// packs; alternate objects use the ordinary reader. Additive repack includes protected
    /// objects; retirement can instead verify a protected survivor and omit its replacement copy.
    /// Other required objects are read
    /// and identity-checked, then appended with deltas against each other under
    /// [`RepackLimits::compression`]. A repository whose only pack holds exactly the required
    /// objects can therefore reproduce that pack byte for byte. It writes two bounded temporary
    /// files in the primary pack directory, synchronizes each file, then publishes the pack
    /// before its index. The index is the visibility marker. An existing checksum-named file is
    /// reused only when every byte matches. Old packs, loose objects, reflogs and alternates
    /// remain untouched, including protected packs. Readers with pinned generations keep their
    /// handles; new readers discover the indexed pair and existing readers can refresh
    /// explicitly.
    ///
    /// Bitmap output follows complete pair publication and requires explicit opt-in in both bare
    /// and nonbare repositories. The last resolved `repack.writeBitmaps`/`pack.writeBitmaps`
    /// assignment or [`RepackLimits::write_bitmaps`] controls that choice. This adds verified
    /// graph reads, traversal work and sidecar disk space. Shallow graphs and replacements omitting
    /// protected copies report a skip. Bitmap failure retains the published pair in
    /// [`RepackError::Bitmap`] and never removes old storage.
    ///
    /// The caller must exclude cooperating ref, reflog and object writers when it requires a
    /// stable maintenance observation. Girt has no lock that excludes arbitrary external Git
    /// writers. This operation never deletes data, so a concurrent new root cannot make it prune
    /// that root's object. An external writer that removes or mutates input files can still make
    /// the scan/read fail. Do not use this result as deletion authorization. Destructive
    /// maintenance must rescan under its own execution boundary and account for old pinned
    /// readers before removing files.
    ///
    /// # Errors
    ///
    /// Incomplete scans, missing/corrupt objects, limits, I/O and cancellation fail without
    /// replacing an existing artifact. Failure after pack publication can leave an unindexed pack;
    /// readers ignore it and a retry may reuse it. Cancellation after index publication reports
    /// success. Temporary files are cleaned up on ordinary failure but can survive process death.
    /// On Unix, the pack directory is synchronized before index publication and again after it,
    /// so a successful return leaves both names durable. Publication assumes a trusted local
    /// filesystem with no in-place artifact edits.
    ///
    /// # Add a pack without deleting loose objects
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
    /// use girt::retention::{RepackLimits, RetentionPolicy};
    /// let id = repo.loose_objects().write_blob(b"keep me")?;
    /// let published = repo.repack_retained(
    ///     &RetentionPolicy::default(),
    ///     RepackLimits::default(),
    ///     &cancel,
    /// )?;
    /// assert_eq!(published.written.objects, 1);
    /// assert_eq!(repo.loose_objects().read_blob(id, 1024)?, b"keep me");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn repack_retained(
        &self,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
    ) -> Result<RepackPublished, RepackError> {
        self.repack_retained_after_plan(policy, limits, cancel, || {})
    }

    // The checkpoint makes the scan/write boundary deterministic in concurrency tests.
    fn repack_retained_after_plan(
        &self,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
        after_plan: impl FnOnce(),
    ) -> Result<RepackPublished, RepackError> {
        let _headers = crate::pack::HeaderScope::new();
        check(cancel)?;
        // Publishing omits promised objects it cannot copy, so execution refuses partial clones
        // whatever the planning policy allows. The configuration is read again from disk, before
        // planning and again before publishing, so a handle opened before the repository became a
        // partial clone still refuses.
        self.refuse_partial_clone()?;
        let mut plan = self.plan_retention(policy, cancel);
        if let super::RetentionOutcome::Incomplete(reason) = plan.outcome {
            return Err(RepackError::Incomplete(reason));
        }
        after_plan();
        self.refuse_partial_clone()?;
        // Additive repack publishes the complete retained set. Only retirement omits survivors.
        plan.protected_packs.clear();
        self.publish_required(&plan, policy, limits, cancel)
            .map(|(published, _)| published)
    }

    /// Refuses when the configuration on disk, not this handle's cached copy, marks a partial
    /// clone.
    fn refuse_partial_clone(&self) -> Result<(), RepackError> {
        let current = Repository::open(self.git_dir())
            .map_err(|error| RepackError::Incomplete(RetentionError::read(error)))?;
        if current.is_partial_clone() {
            return Err(RepackError::Incomplete(RetentionError::PartialClone));
        }
        Ok(())
    }

    /// Publishes the objects required by a complete scan of this repository.
    ///
    /// Objects whose first primary copy sits in one of the plan's protected packs are decoded from
    /// that copy to prove it readable and left out of the new pack, as `git repack -a` leaves
    /// out objects in `.keep` packs. Their identities are returned so retirement can verify the
    /// protected copy again before unlinking anything.
    pub(super) fn publish_required(
        &self,
        plan: &super::RetentionPlan,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
    ) -> Result<(RepackPublished, std::collections::BTreeSet<ObjectId>), RepackError> {
        self.publish_required_after_write(plan, policy, limits, cancel, || {})
    }

    fn publish_required_after_write(
        &self,
        plan: &super::RetentionPlan,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
        after_write: impl FnOnce(),
    ) -> Result<(RepackPublished, std::collections::BTreeSet<ObjectId>), RepackError> {
        let leg = Leg {
            ids: &plan.required,
            marker: None,
            partial_clone: false,
        };
        self.publish_leg(
            plan,
            leg,
            policy,
            limits,
            cancel,
            after_write,
            &mut LegProgress::default(),
            |_| Ok(()),
        )
    }

    /// Publishes `leg.ids` as one pack, read through the plan's pinned store.
    ///
    /// Objects the plan did not size, such as unreachable promisor-pack objects, are sized from
    /// their pack entry headers. `step` runs before each publisher step, after the input check.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn publish_leg(
        &self,
        plan: &super::RetentionPlan,
        leg: Leg<'_>,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
        after_write: impl FnOnce(),
        progress: &mut LegProgress,
        mut step: impl FnMut(Step) -> Result<(), RepackError>,
    ) -> Result<(RepackPublished, std::collections::BTreeSet<ObjectId>), RepackError> {
        let _headers = crate::pack::HeaderScope::new();
        check(cancel)?;
        let write_bitmaps = configured_bitmaps(self, limits.write_bitmaps)?;
        plan.check_packs_unchanged(cancel)?;
        let objects = plan
            .store
            .as_ref()
            .ok_or_else(|| RepackError::Incomplete(RetentionError::MissingPinnedPlanStore))?;
        let required = leg.ids;
        let protected = &plan.protected_packs;
        let protected_names: std::collections::BTreeSet<_> = protected
            .iter()
            .filter_map(|path| path.file_name().map(std::ffi::OsStr::to_owned))
            .collect();
        let pack_sources = objects.primary_packs_with_paths();
        let mut held = std::collections::BTreeSet::new();
        let count =
            u32::try_from(required.len()).map_err(|_| PackWriteError::Limit("object count"))?;
        if count > limits.write.max_objects {
            return Err(PackWriteError::Limit("object count").into());
        }
        let packs: Vec<_> = pack_sources.iter().map(|(_, pack)| *pack).collect();
        let mut packed = Vec::new();
        let mut decoded = Vec::new();
        let mut bytes = 0u64;
        'required: for &id in required {
            check(cancel)?;
            if !protected_names.is_empty() {
                for (path, source) in &pack_sources {
                    if !path
                        .file_name()
                        .is_some_and(|name| protected_names.contains(name))
                    {
                        continue;
                    }
                    let found = source
                        .find(id)
                        .map_err(|source| RepackError::Read { id, source })?;
                    if let Some(position) = found {
                        // A later readable protected copy can survive in place. A damaged entry
                        // cannot authorize omission; retain the ordinary first copy instead.
                        if source.read(position, policy.read, cancel).is_ok() {
                            held.insert(id);
                            continue 'required;
                        }
                        check(cancel)?;
                    }
                }
            }
            for (pack, source) in packs.iter().enumerate() {
                let found = source
                    .find(id)
                    .map_err(|source| RepackError::Read { id, source })?;
                if let Some(position) = found {
                    let size = match plan.object_info.get(&id) {
                        Some(info) => info.size,
                        None => {
                            source
                                .locate(position, policy.read, cancel)
                                .map_err(|source| RepackError::Read { id, source })?
                                .1
                        }
                    };
                    charge_input(size, &mut bytes, limits.write)?;
                    packed.push(Packed { id, pack, position });
                    continue 'required;
                }
            }
            let object = objects
                .read_controlled(id, policy.read, cancel)
                .map_err(|source| RepackError::Read { id, source })?
                .or_else(|| super::canonical_empty_tree(id))
                .ok_or(RepackError::Missing(id))?;
            charge_input(object.data().len() as u64, &mut bytes, limits.write)?;
            decoded.push((id, object));
        }
        let fresh: Vec<_> = decoded
            .iter()
            .map(|(id, object)| PackObject {
                id: *id,
                kind: object.kind(),
                data: object.data(),
            })
            .collect();
        let directory = self.object_dir().join("pack");
        fs::create_dir_all(&directory)?;
        let mut pack = NamedTempFile::new_in(&directory)?;
        let mut index = NamedTempFile::new_in(&directory)?;
        let write_result = write_reusing(
            self.object_format(),
            &packs,
            &packed,
            &fresh,
            (
                &mut BufWriter::with_capacity(PACK_BUFFER, pack.as_file_mut()),
                &mut BufWriter::new(index.as_file_mut()),
            ),
            limits.write,
            limits.compression,
            (policy.read, cancel),
            &mut || {
                if cancel.load(Ordering::Relaxed) {
                    Err(PackWriteError::Io(io::ErrorKind::Interrupted.into()))
                } else {
                    Ok(())
                }
            },
        );
        let written = match write_result {
            Err(_) if cancel.load(Ordering::Relaxed) => return Err(RepackError::Cancelled),
            Err(ReuseError::Read { id, source }) => return Err(RepackError::Read { id, source }),
            Err(ReuseError::Write(error)) => return Err(error.into()),
            Ok(written) => written,
        };
        // Fresh delta search must produce objects readable under the caller's limits.
        if written.deltas.max_depth > policy.read.max_delta_depth {
            return Err(PackWriteError::Limit("output delta depth").into());
        }
        pack.as_file().sync_all()?;
        index.as_file().sync_all()?;
        after_write();
        let basename = directory.join(format!("pack-{}", written.checksum));
        progress.written = Some(written);
        publish_pair_checked(
            Pair {
                pack,
                index,
                basename: &basename,
                marker: leg.marker,
            },
            cancel,
            &mut progress.publish,
            |next| {
                plan.check_packs_unchanged(cancel)
                    .map_err(RepackError::from)?;
                step(next)
            },
        )?;
        let mut published = RepackPublished {
            written,
            bitmap: BitmapOutcome::Disabled,
        };
        if write_bitmaps {
            published.bitmap = if leg.partial_clone {
                BitmapOutcome::SkippedPartialClone
            } else if !self.shallow_roots().is_empty() {
                BitmapOutcome::SkippedShallow
            } else if !held.is_empty() {
                BitmapOutcome::SkippedProtected
            } else {
                published.bitmap = BitmapOutcome::Failed;
                match crate::bitmap::write_repack_bitmap(
                    self,
                    written.checksum,
                    limits.bitmap,
                    cancel,
                ) {
                    Ok(bitmap) => BitmapOutcome::Written(bitmap),
                    Err(source) => {
                        if limits.write_bitmaps != Some(true)
                            && let Some(limit) = bitmap_limit(&source.source)
                        {
                            BitmapOutcome::SkippedLimit { limit }
                        } else {
                            return Err(RepackError::Bitmap {
                                published: Box::new(published),
                                source: Box::new(source),
                            });
                        }
                    }
                }
            };
        }
        Ok((published, held))
    }
}

fn bitmap_limit(error: &crate::Error) -> Option<&'static str> {
    use crate::bitmap::WriteError;
    match error.downcast_ref::<WriteError>() {
        Some(WriteError::Limit(limit)) => Some(*limit),
        Some(WriteError::Read(ObjectReadError::Limit(limit))) => Some(*limit),
        _ => match error.downcast_ref::<ObjectReadError>() {
            Some(ObjectReadError::Limit(limit)) => Some(*limit),
            _ => None,
        },
    }
}

fn configured_bitmaps(
    repository: &Repository,
    requested: Option<bool>,
) -> Result<bool, RepackError> {
    if let Some(requested) = requested {
        return Ok(requested);
    }
    let last = repository.config().entries().iter().rev().find(|entry| {
        entry.subsection.is_none()
            && entry.name.eq_ignore_ascii_case(b"writeBitmaps")
            && (entry.section.eq_ignore_ascii_case(b"repack")
                || entry.section.eq_ignore_ascii_case(b"pack"))
    });
    let section = if last.is_some_and(|entry| entry.section.eq_ignore_ascii_case(b"pack")) {
        "pack"
    } else {
        "repack"
    };
    Ok(repository
        .config()
        .boolean(section, None, "writeBitmaps")?
        .unwrap_or(false))
}

fn charge_input(len: u64, bytes: &mut u64, limits: PackWriteLimits) -> Result<(), PackWriteError> {
    if len > limits.max_object_bytes {
        return Err(PackWriteError::Limit("object bytes"));
    }
    *bytes = bytes
        .checked_add(len)
        .ok_or(PackWriteError::Limit("input bytes"))?;
    if *bytes > limits.max_input_bytes {
        return Err(PackWriteError::Limit("input bytes"));
    }
    Ok(())
}

#[cfg(test)]
fn publish_pair(
    pack: NamedTempFile,
    index: NamedTempFile,
    basename: &Path,
    cancel: &AtomicBool,
    after_pack: impl FnOnce(),
) -> Result<(), RepackError> {
    let mut after_pack = Some(after_pack);
    publish_pair_checked(
        Pair {
            pack,
            index,
            basename,
            marker: None,
        },
        cancel,
        &mut Progress::default(),
        |step| {
            if step == Step::Index
                && let Some(after_pack) = after_pack.take()
            {
                after_pack();
            }
            Ok(())
        },
    )
}

fn check(cancel: &AtomicBool) -> Result<(), RepackError> {
    if cancel.load(Ordering::Relaxed) {
        Err(RepackError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn publish(temporary: NamedTempFile, path: &Path, cancel: &AtomicBool) -> Result<(), RepackError> {
    crate::pack::publish::file(temporary, path, cancel).map_err(RepackError::from)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn output_depth_limit_refuses_publication_for_loose_delta_search() {
        let root = tempfile::tempdir().unwrap();
        let repository = Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("repo"),
            crate::InitKind::Bare,
        )
        .unwrap();
        let data: Vec<u8> = (0..4096u32)
            .flat_map(|i| i.wrapping_mul(2_654_435_761).to_le_bytes())
            .collect();
        let a = repository.loose_objects().write_blob(&data).unwrap();
        let mut similar = data;
        similar[0] = b'b';
        let b = repository.loose_objects().write_blob(&similar).unwrap();
        let policy = RetentionPolicy {
            heads: vec![a, b],
            read: crate::ReadLimits {
                max_delta_depth: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            repository.repack_retained(&policy, RepackLimits::default(), &AtomicBool::new(false)),
            Err(RepackError::Write(PackWriteError::Limit(
                "output delta depth"
            )))
        ));
        assert_eq!(
            fs::read_dir(repository.object_dir().join("pack"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            repository
                .loose_objects()
                .read_blob(a, 16_384)
                .unwrap()
                .len(),
            16_384
        );
    }

    #[test]
    fn cancellation_between_publications_leaves_only_an_unindexed_pack() {
        let root = tempfile::tempdir().unwrap();
        let basename = root.path().join("pack-test");
        let mut pack = NamedTempFile::new_in(root.path()).unwrap();
        let mut index = NamedTempFile::new_in(root.path()).unwrap();
        pack.write_all(b"pack bytes").unwrap();
        index.write_all(b"index bytes").unwrap();
        let cancelled = AtomicBool::new(false);
        let result = publish_pair(pack, index, &basename, &cancelled, || {
            assert!(basename.with_extension("pack").exists());
            assert!(!basename.with_extension("idx").exists());
            cancelled.store(true, Ordering::Relaxed);
        });
        assert!(matches!(result, Err(RepackError::Cancelled)));
        assert_eq!(
            fs::read(basename.with_extension("pack")).unwrap(),
            b"pack bytes"
        );
        assert!(!basename.with_extension("idx").exists());
    }

    #[test]
    fn conflicting_existing_artifact_is_not_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pack-existing.pack");
        fs::write(&path, b"old bytes").unwrap();
        let mut candidate = NamedTempFile::new_in(root.path()).unwrap();
        candidate.write_all(b"new bytes").unwrap();
        let result = publish(candidate, &path, &AtomicBool::new(false));
        assert!(matches!(result, Err(RepackError::Existing(_))));
        assert_eq!(fs::read(path).unwrap(), b"old bytes");
    }

    #[test]
    fn new_loose_object_after_scan_survives_additive_publication() {
        let root = tempfile::tempdir().unwrap();
        let repository = Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("repo"),
            crate::InitKind::Bare,
        )
        .unwrap();
        let mut new_id = None;
        repository
            .repack_retained_after_plan(
                &RetentionPolicy::default(),
                RepackLimits::default(),
                &AtomicBool::new(false),
                || {
                    new_id = Some(
                        repository
                            .loose_objects()
                            .write_blob(b"new writer")
                            .unwrap(),
                    )
                },
            )
            .unwrap();
        let id = new_id.unwrap();
        assert_eq!(
            repository.loose_objects().read_blob(id, 64).unwrap(),
            b"new writer"
        );
    }
    /// Marks `repository` as a partial clone on disk, behind its cached configuration.
    fn mark_partial_clone(repository: &Repository) {
        let path = repository.git_dir().join("config");
        let mut config = fs::read_to_string(&path).unwrap();
        config.push_str("[remote \"origin\"]\n\tpromisor = true\n");
        fs::write(path, config).unwrap();
    }

    #[rstest::rstest]
    #[case::before_planning(true)]
    #[case::before_publishing(false)]
    fn partial_clone_on_disk_refuses_repack(#[case] before_planning: bool) {
        let root = tempfile::tempdir().unwrap();
        let repository = Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("repo"),
            crate::InitKind::Bare,
        )
        .unwrap();
        repository.loose_objects().write_blob(b"kept").unwrap();
        if before_planning {
            mark_partial_clone(&repository);
        }
        assert!(!repository.is_partial_clone());
        let error = repository
            .repack_retained_after_plan(
                &RetentionPolicy::default(),
                RepackLimits::default(),
                &AtomicBool::new(false),
                || {
                    if !before_planning {
                        mark_partial_clone(&repository);
                    }
                },
            )
            .unwrap_err();
        assert!(
            matches!(error, RepackError::Incomplete(RetentionError::PartialClone)),
            "{error:?}"
        );
        let pack = repository.object_dir().join("pack");
        assert!(
            !pack.exists() || fs::read_dir(pack).unwrap().next().is_none(),
            "nothing is published"
        );
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[case::truncate(true)]
    #[case::overwrite(false)]
    fn warmed_pack_change_refuses_repack_publication(#[case] truncate: bool) {
        let (_root, repo, policy, pack) = super::super::walk_tests::header_fixture();
        let before = fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count();
        let error = repo
            .repack_retained_after_plan(
                &policy,
                RepackLimits::trusted(),
                &AtomicBool::new(false),
                || super::super::walk_tests::change_pack(&pack, truncate),
            )
            .unwrap_err();
        assert!(
            matches!(error, RepackError::Open(ObjectReadError::PackChanged { path }) if path == pack)
        );
        assert_eq!(
            fs::read_dir(repo.object_dir().join("pack"))
                .unwrap()
                .count(),
            before
        );
    }
    #[cfg(unix)]
    #[rstest::rstest]
    #[case::truncate(true)]
    #[case::overwrite(false)]
    fn warmed_pack_change_after_write_refuses_publication(#[case] truncate: bool) {
        let (_root, repo, policy, pack) = super::super::walk_tests::header_fixture();
        let cancel = AtomicBool::new(false);
        let mut plan = repo.plan_retention(&policy, &cancel);
        assert!(plan.is_complete());
        plan.protected_packs.clear();
        let before = fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count();
        let error = repo
            .publish_required_after_write(&plan, &policy, RepackLimits::trusted(), &cancel, || {
                super::super::walk_tests::change_pack(&pack, truncate)
            })
            .unwrap_err();
        assert!(
            matches!(error, RepackError::Open(ObjectReadError::PackChanged { path }) if path == pack)
        );
        assert_eq!(
            fs::read_dir(repo.object_dir().join("pack"))
                .unwrap()
                .count(),
            before
        );
    }
}
