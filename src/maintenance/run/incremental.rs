//! Incremental task policy; verification and publication stay in the pack primitives.
use std::fs::File;
use std::io::{self, Read};
use std::sync::atomic::AtomicBool;

use super::{
    BorrowedIsolation, MaintenanceRun, MaintenanceRunError, TaskOutcome, TaskSkip, checkpoint,
};
use crate::maintenance::{AutoThreshold, MaintenanceConfig, MaintenanceTask};
use crate::multi_pack_index::{
    BatchRepackLimits, BatchRepackReport, ExpireLimits, ExpireReport, IndexWritten, MultiPackIndex,
    WriteLimits, WriteOptions,
};
use crate::{AlternateLimits, PackLimits};

/// Bounds for one incremental-repack task.
///
/// Defaults admit a Linux-kernel-sized store: 512 MiB of pack indexes and MIDX input/output,
/// 20 million input object occurrences, and 4096 packs. Expiry retains its one-million directory
/// observation and 1 GiB decoded-survivor budgets; batch payload and repository admission use
/// their primitive defaults. Each phase has an independent budget. Larger duplicate inventories
/// can require explicit increases. This type is `Copy`, so later fields must also be `Copy`.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct IncrementalRepackLimits {
    /// Initial MIDX encoding, including duplicate occurrences.
    pub write: WriteLimits,
    /// Initial pack snapshot and automatic eligibility observation.
    /// Automatic MIDX parsing uses `batch.batch.index`.
    pub packs: PackLimits,
    /// Verified zero-owned pack expiry.
    pub expire: ExpireLimits,
    /// Additive selected-pack publication; no selected source pack is retired here.
    pub batch: BatchRepackLimits,
}

impl Default for IncrementalRepackLimits {
    fn default() -> Self {
        let packs = PackLimits::default().with_max_index_bytes(512 * 1024 * 1024);
        let write = WriteLimits {
            max_bytes: 512 * 1024 * 1024,
            max_objects: 20_000_000,
            ..WriteLimits::default()
        };
        let index = crate::multi_pack_index::Limits {
            max_bytes: write.max_bytes,
            max_objects: write.max_objects,
            ..crate::multi_pack_index::Limits::default()
        };
        let expire = ExpireLimits {
            packs,
            index,
            write,
            ..ExpireLimits::default()
        };
        let batch = BatchRepackLimits {
            batch: crate::multi_pack_index::BatchLimits { packs, index },
            index: write,
            ..BatchRepackLimits::default()
        };
        Self {
            write,
            packs,
            expire,
            batch,
        }
    }
}

impl IncrementalRepackLimits {
    /// Replaces initial MIDX encoding bounds.
    #[must_use]
    pub fn with_write(mut self, value: WriteLimits) -> Self {
        self.write = value;
        self
    }
    /// Replaces initial snapshot bounds.
    #[must_use]
    pub fn with_packs(mut self, value: PackLimits) -> Self {
        self.packs = value;
        self
    }
    /// Replaces verified expiry bounds.
    #[must_use]
    pub fn with_expire(mut self, value: ExpireLimits) -> Self {
        self.expire = value;
        self
    }
    /// Replaces additive batch bounds.
    #[must_use]
    pub fn with_batch(mut self, value: BatchRepackLimits) -> Self {
        self.batch = value;
        self
    }
}

/// Completed incremental phases. The default records no completed phase.
///
/// The order is MIDX write, verified expiry, then additive repacking. Expiry deliberately preserves
/// packs at least as new as the MIDX, unlike Git's observed expiry of future-dated zero-owned
/// packs. Keeping extra disk data is preferable to deleting another writer's recently published
/// pack. Preserved paths are recorded in [`ExpireReport::preserved`]. A failed primitive's partial
/// report stays in [`crate::maintenance::MaintenanceFailure::source`].
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct IncrementalRepackReport {
    /// Completed initial MIDX publication.
    pub index: Option<IndexWritten>,
    /// Completed expiry, including preserved primary packs.
    pub expire: Option<ExpireReport>,
    /// Target computed from surviving pack sizes, after expiry.
    pub batch_bytes: Option<u64>,
    /// Completed additive batch publication.
    pub repack: Option<BatchRepackReport>,
}

pub(super) fn run<G>(
    run: &MaintenanceRun<'_>,
    guard: &G,
    outcome: &mut TaskOutcome,
    cancel: &AtomicBool,
) -> Result<(), MaintenanceRunError> {
    let limits = run.limits.incremental_repack;
    let threshold = if run.auto {
        MaintenanceConfig::new(run.repo.config())
            .auto_threshold(MaintenanceTask::IncrementalRepack)
            .map_err(MaintenanceRunError::primitive)?
    } else {
        None
    };
    let due = match threshold {
        Some(AutoThreshold::Disabled) => false,
        Some(AutoThreshold::Count(count)) => count_due(run, count, cancel)?,
        Some(AutoThreshold::Always) | None => true,
    };
    if !due {
        *outcome = TaskOutcome::Skipped(TaskSkip::NotDue);
        return Ok(());
    }
    *outcome = TaskOutcome::IncrementalRepack(Box::default());
    let TaskOutcome::IncrementalRepack(report) = outcome else {
        unreachable!("initialized above")
    };
    let mut isolation = BorrowedIsolation { _guard: guard };
    report.index = Some(
        run.repo
            .write_multi_pack_index_controlled(
                &mut isolation,
                WriteOptions::default(),
                limits.write,
                limits.packs,
                cancel,
            )
            .map_err(MaintenanceRunError::primitive)?,
    );
    checkpoint(cancel)?;
    report.expire = Some(
        run.repo
            .expire_multi_pack_index(&mut isolation, limits.expire, cancel)
            .map_err(MaintenanceRunError::primitive)?,
    );
    checkpoint(cancel)?;
    let objects = run
        .repo
        .objects_controlled(limits.batch.batch.packs, AlternateLimits::default(), cancel)
        .map_err(MaintenanceRunError::primitive)?;
    let mut largest = 0;
    let mut second = 0;
    for pack in objects.stores().next().expect("primary store").packs() {
        checkpoint(cancel)?;
        let size = pack.pack_bytes();
        if size >= largest {
            second = largest;
            largest = size;
        } else if size > second {
            second = size;
        }
    }
    let target = second
        .checked_add(1)
        .ok_or(MaintenanceRunError::Limit("incremental batch bytes"))?;
    drop(objects);
    report.batch_bytes = Some(target);
    report.repack = Some(
        run.repo
            .repack_incremental_batch(&mut isolation, target, limits.batch, cancel)
            .map_err(MaintenanceRunError::primitive)?,
    );
    Ok(())
}

fn count_due(
    run: &MaintenanceRun<'_>,
    threshold: u32,
    cancel: &AtomicBool,
) -> Result<bool, MaintenanceRunError> {
    checkpoint(cancel)?;
    let limits = run.limits.incremental_repack;
    let path = run.repo.object_dir().join("pack/multi-pack-index");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(MaintenanceRunError::InputIo {
                path,
                source: io::Error::new(io::ErrorKind::InvalidInput, "MIDX is not a regular file"),
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(MaintenanceRunError::InputIo { path, source }),
    }
    let bytes = match File::open(&path) {
        Ok(file) => {
            let max = limits.batch.batch.index.max_bytes;
            let cap = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
            let mut bytes = Vec::new();
            file.take(cap).read_to_end(&mut bytes).map_err(|source| {
                MaintenanceRunError::InputIo {
                    path: path.clone(),
                    source,
                }
            })?;
            if bytes.len() > max {
                return Err(MaintenanceRunError::Limit("automatic MIDX bytes"));
            }
            Some(bytes)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(source) => return Err(MaintenanceRunError::InputIo { path, source }),
    };
    checkpoint(cancel)?;
    let index = bytes
        .as_deref()
        .map(|bytes| {
            MultiPackIndex::parse_with(bytes, run.repo.object_format(), limits.batch.batch.index)
        })
        .transpose()
        .map_err(MaintenanceRunError::primitive)?;
    let objects = run
        .repo
        .objects_controlled(limits.packs, AlternateLimits::default(), cancel)
        .map_err(MaintenanceRunError::primitive)?;
    let mut count = 0;
    for pack in objects.stores().next().expect("primary store").packs() {
        checkpoint(cancel)?;
        let indexed = index.as_ref().is_some_and(|index| {
            pack.index_path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| index.pack_names().contains(&name))
        });
        if !indexed {
            count += 1;
            if count >= threshold {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
