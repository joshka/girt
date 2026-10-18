//! Read-only batch policy over one verified primary-pack snapshot.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use super::{Limits, MultiPackIndex};
use crate::{ObjectId, PackLimits, Repository};

/// Bounds for observing a standalone MIDX and opening its primary packs.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct BatchLimits {
    /// Maximum MIDX bytes, pack names and object entries.
    pub index: Limits,
    /// Aggregate pack/index opening and directory bounds.
    pub packs: PackLimits,
}

/// A primary pack selected for an advisory incremental repack batch.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct BatchPack {
    /// Absolute path to the primary `.pack` file observed by the snapshot.
    pub path: PathBuf,
    /// Number of objects whose MIDX locations select this pack.
    pub objects: usize,
    /// Pack bytes multiplied by its owned-object fraction, rounded down.
    pub estimated_bytes: u64,
}

/// Why a named pack was left outside an incremental repack batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BatchSkipReason {
    /// A `.keep` marker protects the pack.
    Kept,
    /// Promisor-aware batch publication is not supported by this planner.
    Promisor,
    /// A `.mtimes` sidecar identifies a protected cruft pack.
    Cruft,
    /// No object location in the MIDX selects this pack; expiry is a separate operation.
    NoOwnedObjects,
    /// Its effective size is at least the nonzero batch target.
    TooLarge,
    /// Smaller eligible packs already filled the batch.
    OutsideBatch,
    /// Fewer than two eligible packs were available; no repack is useful.
    InsufficientPacks,
}

/// A named primary pack omitted from an advisory batch, with its policy reason.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct BatchSkipped {
    /// Observed primary `.pack` path.
    pub path: PathBuf,
    /// Policy that excluded it.
    pub reason: BatchSkipReason,
}

/// Advisory work selected from one standalone MIDX and its pinned primary packs.
///
/// This report never authorizes deletion or proves packed payload readability. Paths can change
/// after return. An executor must refresh and repeat selection under caller-owned maintenance
/// isolation, keep that snapshot through copying, and verify published survivors before retirement.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct RepackBatch {
    /// Selected packs in increasing whole-second modification time, then path order.
    /// Empty when fewer than two packs qualify.
    pub packs: Vec<BatchPack>,
    /// Named packs excluded by protection, size, ownership or batch capacity.
    pub skipped: Vec<BatchSkipped>,
    /// Sum of selected effective pack sizes; this is an estimate, not an output-size limit.
    pub estimated_bytes: u64,
}

/// Failure to observe and select an incremental repack batch; no files were changed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BatchError {
    /// The MIDX is absent, malformed, over budget or disagrees with primary indexes.
    #[error("invalid incremental repack index")]
    Index(#[source] super::Error),
    /// A pinned primary pack/index could not be opened or verified.
    #[error("cannot inspect incremental repack packs")]
    Read(#[source] Box<crate::ObjectReadError>),
    /// A pack or sidecar could not be observed.
    #[error("observing incremental repack input {path}")]
    Path {
        /// Path whose metadata observation failed.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// Only standalone indexes without incremental-chain manifests are supported.
    #[error("incremental-chain repack selection is unsupported")]
    IncrementalChain,
    /// Caller cancellation was observed.
    #[error("incremental repack selection cancelled")]
    Cancelled,
    /// A source pack path is no longer a regular file.
    #[error("incremental repack inputs changed")]
    Changed,
}

impl Repository {
    /// Plans a size-limited incremental repack from a verified standalone MIDX.
    ///
    /// Each named pack must be available in this repository's primary store. Its index and every
    /// MIDX identity/offset claim are verified; alternates cannot fill missing primary packs.
    /// Whole-pack payloads are not decoded. Packs with `.keep`, `.promisor` or `.mtimes` sidecars
    /// are reported as protected. Zero-owner packs are left for MIDX expiry.
    ///
    /// Effective size is pack bytes multiplied by the fraction of indexed objects owned by that
    /// pack in the MIDX, rounded down. A nonzero `batch_bytes` excludes packs whose effective size
    /// is at least that target, then selects the oldest packs until their sum reaches the target
    /// or candidates end. Age uses whole-second modification times, with path order breaking ties.
    /// Zero selects all eligible packs. Fewer than two eligible packs selects nothing.
    /// Size and age policy follows Git; zero-owner packs are left for expiry, and promisor/cruft
    /// packs remain protected.
    ///
    /// No configuration is read to choose the target, no files are written and no work is spawned.
    /// This is an advisory report: see [`RepackBatch`] before executing it. Incremental chains
    /// refuse even when a standalone MIDX shadows them. Cancellation is checked between packs and
    /// object entries; one bounded index parse or pack-index checksum cannot be interrupted.
    ///
    /// # Errors
    ///
    /// Returns [`BatchError`] for missing/invalid indexes, missing primary packs, unsupported
    /// chains, exhausted bounds, changed artifacts, I/O failures or cancellation. All storage
    /// remains intact.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::multi_pack_index::{BatchError, BatchLimits, Error};
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let result = repo.plan_incremental_repack(
    ///     2 * 1024 * 1024,
    ///     BatchLimits::default(),
    ///     &AtomicBool::new(false),
    /// );
    /// assert!(matches!(
    ///     result,
    ///     Err(BatchError::Index(Error::MissingIndex))
    /// ));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn plan_incremental_repack(
        &self,
        batch_bytes: u64,
        limits: BatchLimits,
        cancel: &AtomicBool,
    ) -> Result<RepackBatch, BatchError> {
        check(cancel)?;
        let directory = self.object_dir().join("pack");
        if present(&directory.join("multi-pack-index.d/multi-pack-index-chain"))? {
            return Err(BatchError::IncrementalChain);
        }
        let bytes =
            super::verify::read(&directory.join("multi-pack-index"), limits.index.max_bytes)
                .map_err(BatchError::Index)?
                .ok_or(BatchError::Index(super::Error::MissingIndex))?;
        let index = MultiPackIndex::parse_with(&bytes, self.object_format(), limits.index)
            .map_err(BatchError::Index)?;
        if index.base_count() != 0 {
            return Err(BatchError::IncrementalChain);
        }
        check(cancel)?;
        let objects = self
            .objects_controlled(limits.packs, Default::default(), cancel)
            .map_err(|error| BatchError::Read(Box::new(error)))?;
        let primary: BTreeMap<_, _> = objects.primary_packs_with_paths().into_iter().collect();
        let mut named = Vec::with_capacity(index.pack_names().len());
        for name in index.pack_names() {
            check(cancel)?;
            let path = directory.join(name).with_extension("pack");
            let pack = primary
                .get(path.as_path())
                .ok_or(BatchError::Index(super::Error::MissingPack))?;
            pack.verify_index()
                .map_err(|error| BatchError::Read(Box::new(error)))?;
            named.push((path, *pack));
        }
        let mut owned = vec![0usize; named.len()];
        for (position, bytes) in index
            .ids
            .chunks_exact(self.object_format().digest_len())
            .enumerate()
        {
            check(cancel)?;
            let id = ObjectId::from_bytes(self.object_format(), bytes)
                .map_err(|_| BatchError::Index(super::Error::Malformed("object identity")))?;
            let location = index.location(position).map_err(BatchError::Index)?;
            let pack = named[location.pack as usize].1;
            let found = pack
                .find(id)
                .map_err(|error| BatchError::Read(Box::new(error)))?
                .ok_or(BatchError::Index(super::Error::Malformed(
                    "object absent from named pack",
                )))?;
            if pack
                .offset(found)
                .map_err(|error| BatchError::Read(Box::new(error)))? as u64
                != location.offset
            {
                return Err(BatchError::Index(super::Error::Malformed(
                    "object offset disagrees with pack index",
                )));
            }
            owned[location.pack as usize] += 1;
        }
        let mut report = RepackBatch::default();
        let mut candidates = Vec::new();
        for ((path, pack), count) in named.into_iter().zip(owned) {
            check(cancel)?;
            let reason = if present(&path.with_extension("keep"))? {
                Some(BatchSkipReason::Kept)
            } else if present(&path.with_extension("promisor"))? {
                Some(BatchSkipReason::Promisor)
            } else if present(&path.with_extension("mtimes"))? {
                Some(BatchSkipReason::Cruft)
            } else if count == 0 {
                Some(BatchSkipReason::NoOwnedObjects)
            } else {
                None
            };
            let estimated_bytes = if pack.object_count() == 0 {
                0
            } else {
                (u128::from(pack.artifact_bytes().0) * count as u128 / pack.object_count() as u128)
                    as u64
            };
            let reason = reason.or_else(|| {
                (batch_bytes != 0 && estimated_bytes >= batch_bytes)
                    .then_some(BatchSkipReason::TooLarge)
            });
            if let Some(reason) = reason {
                report.skipped.push(BatchSkipped { path, reason });
                continue;
            }
            let metadata = fs::symlink_metadata(&path).map_err(|source| BatchError::Path {
                path: path.clone(),
                source,
            })?;
            if !metadata.is_file() {
                return Err(BatchError::Changed);
            }
            let modified = metadata.modified().map_err(|source| BatchError::Path {
                path: path.clone(),
                source,
            })?;
            candidates.push((
                BatchPack {
                    path,
                    objects: count,
                    estimated_bytes,
                },
                modification_seconds(modified),
            ));
        }
        select(candidates, batch_bytes, &mut report)?;
        objects
            .check_packs_unchanged(cancel)
            .map_err(|error| BatchError::Read(Box::new(error)))?;
        check(cancel)?;
        Ok(report)
    }
}

fn select(
    mut candidates: Vec<(BatchPack, i128)>,
    target: u64,
    report: &mut RepackBatch,
) -> Result<(), BatchError> {
    candidates.sort_by(|(left, left_time), (right, right_time)| {
        left_time.cmp(right_time).then(left.path.cmp(&right.path))
    });
    if candidates.len() < 2 {
        report
            .skipped
            .extend(candidates.into_iter().map(|(pack, _)| BatchSkipped {
                path: pack.path,
                reason: BatchSkipReason::InsufficientPacks,
            }));
        return Ok(());
    }
    for (pack, _) in candidates {
        if target != 0 && report.estimated_bytes >= target {
            report.skipped.push(BatchSkipped {
                path: pack.path,
                reason: BatchSkipReason::OutsideBatch,
            });
        } else {
            report.estimated_bytes = report
                .estimated_bytes
                .checked_add(pack.estimated_bytes)
                .ok_or(BatchError::Index(super::Error::Limit(
                    "estimated batch bytes",
                )))?;
            report.packs.push(pack);
        }
    }
    Ok(())
}

// Git's batch age ordering uses whole stat seconds, so subsecond changes are ties.
fn modification_seconds(time: SystemTime) -> i128 {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => i128::from(duration.as_secs()),
        Err(error) => {
            let duration = error.duration();
            -i128::from(duration.as_secs()) - i128::from(duration.subsec_nanos() != 0)
        }
    }
}

fn present(path: &Path) -> Result<bool, BatchError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(BatchError::Path {
            path: path.to_owned(),
            source,
        }),
    }
}

fn check(cancel: &AtomicBool) -> Result<(), BatchError> {
    if cancel.load(Ordering::Relaxed) {
        Err(BatchError::Cancelled)
    } else {
        Ok(())
    }
}
