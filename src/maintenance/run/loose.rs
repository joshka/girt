//! Loose-object task policy; packed-survivor proofs remain in the existing primitives.
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::{fs, io};

use super::{MaintenanceRun, MaintenanceRunError, TaskOutcome, TaskSkip, checkpoint};
use crate::Repository;
use crate::maintenance::{AutoThreshold, MaintenanceConfig, MaintenanceTask};
use crate::pack::LoosePackReport;
use crate::retention::{MaintenanceIsolation, PrunePackedReport};

/// Completed phases of loose-object maintenance. The default records no completed phase.
///
/// Pruning runs before packing. Newly packed loose copies are retained until a later run.
/// If a primitive fails, its partial report remains in
/// [`crate::maintenance::MaintenanceFailure::source`]; these fields preserve completed earlier
/// phases. A completed pruning prefix can leave `more_candidates`, and packing can leave
/// `more_loose`; either flag invites another isolated pass.
/// See [`crate::maintenance::MaintenanceRun`] for a two-run example.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct LooseObjectsReport {
    /// Completed verified duplicate pruning, including already removed loose copies.
    pub prune: Option<PrunePackedReport>,
    /// Completed additive packing; no newly packed loose copy was removed.
    pub pack: Option<LoosePackReport>,
}

pub(super) fn run<G>(
    run: &MaintenanceRun<'_>,
    guard: &G,
    outcome: &mut TaskOutcome,
    cancel: &AtomicBool,
) -> Result<(), MaintenanceRunError> {
    let threshold = if run.auto {
        MaintenanceConfig::new(run.repo.config())
            .auto_threshold(MaintenanceTask::LooseObjects)
            .map_err(MaintenanceRunError::primitive)?
    } else {
        None
    };
    let due = match threshold {
        Some(AutoThreshold::Disabled) => false,
        Some(AutoThreshold::Count(count)) => {
            count_due(run.repo, count, run.limits.loose_pack.max_entries, cancel)?
        }
        Some(AutoThreshold::Always) | None => true,
    };
    if !due {
        *outcome = TaskOutcome::Skipped(TaskSkip::NotDue);
        return Ok(());
    }
    *outcome = TaskOutcome::LooseObjects(Box::default());
    let TaskOutcome::LooseObjects(report) = outcome else {
        unreachable!("outcome was just initialized");
    };
    let mut isolation = BorrowedIsolation { _guard: guard };
    report.prune = Some(
        run.repo
            .prune_packed(&mut isolation, run.limits.prune_packed, cancel)
            .map_err(MaintenanceRunError::primitive)?,
    );
    checkpoint(cancel)?;
    report.pack = Some(
        run.repo
            .pack_loose(&mut isolation, run.limits.loose_pack, cancel)
            .map_err(MaintenanceRunError::primitive)?,
    );
    Ok(())
}

// Constructed only with the batch's live guard, for primitives operating on the same repository.
// The caller's exclusion spans both phases; acquiring either primitive must not reacquire it.
struct BorrowedIsolation<'a, G> {
    _guard: &'a G,
}
impl<G> MaintenanceIsolation for BorrowedIsolation<'_, G> {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> io::Result<()> {
        Ok(())
    }
}

fn count_due(
    repo: &Repository,
    threshold: u32,
    max_entries: usize,
    cancel: &AtomicBool,
) -> Result<bool, MaintenanceRunError> {
    let mut remaining = max_entries;
    let mut count = 0u32;
    let mut poll = 0;
    let mut visit = || {
        if poll == 0 {
            checkpoint(cancel)?;
            poll = 64;
        }
        poll -= 1;
        remaining = remaining
            .checked_sub(1)
            .ok_or(MaintenanceRunError::Limit("loose inventory entries"))?;
        Ok::<(), MaintenanceRunError>(())
    };
    for prefix in 0..256 {
        checkpoint(cancel)?;
        let directory = repo.object_dir().join(format!("{prefix:02x}"));
        let metadata = match fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error(&directory, error)),
        };
        visit()?;
        if !metadata.is_dir() {
            return Err(MaintenanceRunError::NonRegularLoose(directory));
        }
        for entry in fs::read_dir(&directory).map_err(|source| io_error(&directory, source))? {
            visit()?;
            let entry = entry.map_err(|source| io_error(&directory, source))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.len() != repo.object_format().digest_len() * 2 - 2
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                continue;
            }
            if !entry
                .file_type()
                .map_err(|source| io_error(&entry.path(), source))?
                .is_file()
            {
                return Err(MaintenanceRunError::NonRegularLoose(entry.path()));
            }
            count += 1; // threshold is positive and fits i32, so reaching it cannot overflow.
            if count >= threshold {
                return Ok(true);
            }
        }
    }
    checkpoint(cancel)?;
    Ok(false)
}

fn io_error(path: &Path, source: io::Error) -> MaintenanceRunError {
    MaintenanceRunError::InputIo {
        path: path.to_owned(),
        source,
    }
}
