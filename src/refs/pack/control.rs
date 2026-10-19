//! Stack-local accounting; the legacy specialization compiles out every new check.
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{PackRefsError, PackRefsReport};
use crate::PeelLimits;
use crate::refs::store::{check_path, io_error, read_optional};

/// Work bounds for [`super::super::References::pack_controlled`].
///
/// Input bytes include packed refs, loose refs and locked rechecks before pruning. Ref counts
/// include existing packed records and loose direct candidates, including duplicate names.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct PackRefsLimits {
    /// Directory entries examined, including ignored entries and directories.
    pub max_entries: usize,
    /// Packed records plus loose direct candidates considered.
    pub max_refs: usize,
    /// Aggregate bytes read from reference files.
    pub max_input_bytes: usize,
    /// Maximum encoded packed-refs size.
    pub max_output_bytes: usize,
    /// Verified object-read and tag-depth bounds for each reference's peel.
    /// Exhaustion returns [`PackRefsError::Peel`], preserving the underlying peel failure.
    pub peel: PeelLimits,
}

impl Default for PackRefsLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_refs: 100_000,
            max_input_bytes: 64 * 1024 * 1024,
            max_output_bytes: 64 * 1024 * 1024,
            peel: PeelLimits::default(),
        }
    }
}

pub(super) struct Control<'a, const CONTROLLED: bool> {
    pub limits: PackRefsLimits,
    pub cancel: &'a AtomicBool,
    entries: usize,
    refs: usize,
    input: usize,
    remaining: usize,
}

impl<'a, const CONTROLLED: bool> Control<'a, CONTROLLED> {
    pub fn new(limits: PackRefsLimits, cancel: &'a AtomicBool) -> Self {
        Self {
            limits,
            cancel,
            entries: 0,
            refs: 0,
            input: 0,
            remaining: 0,
        }
    }

    pub fn phase(&mut self) -> Result<(), PackRefsError> {
        if CONTROLLED {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(PackRefsError::Cancelled {
                    report: Box::default(),
                });
            }
            self.remaining = 64;
        }
        Ok(())
    }

    pub fn step(&mut self) -> Result<(), PackRefsError> {
        if CONTROLLED {
            if self.remaining == 0 {
                self.phase()?;
            }
            self.remaining -= 1;
        }
        Ok(())
    }

    pub fn entry(&mut self) -> Result<(), PackRefsError> {
        if CONTROLLED {
            self.step()?;
            charge(
                &mut self.entries,
                1,
                self.limits.max_entries,
                "directory entries",
            )?;
        }
        Ok(())
    }

    pub fn reference(&mut self) -> Result<(), PackRefsError> {
        if CONTROLLED {
            charge(&mut self.refs, 1, self.limits.max_refs, "references")?;
        }
        Ok(())
    }

    pub fn output(&self, size: usize) -> Result<(), PackRefsError> {
        if CONTROLLED && size > self.limits.max_output_bytes {
            return Err(limit("output bytes"));
        }
        Ok(())
    }

    pub fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>, PackRefsError> {
        if !CONTROLLED {
            return Ok(read_optional(path)?);
        }
        check_path(path)?;
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(io_error(path, source).into()),
        };
        let remaining = self.limits.max_input_bytes - self.input;
        let length = file
            .metadata()
            .map_err(|source| io_error(path, source))?
            .len();
        if length > remaining as u64 {
            return Err(limit("input bytes"));
        }
        let mut bytes = Vec::with_capacity(length as usize);
        file.take((remaining as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|source| io_error(path, source))?;
        charge(
            &mut self.input,
            bytes.len(),
            self.limits.max_input_bytes,
            "input bytes",
        )?;
        Ok(Some(bytes))
    }
}

fn charge(
    used: &mut usize,
    amount: usize,
    maximum: usize,
    resource: &'static str,
) -> Result<(), PackRefsError> {
    let total = used.checked_add(amount).ok_or_else(|| limit(resource))?;
    if total > maximum {
        return Err(limit(resource));
    }
    *used = total;
    Ok(())
}

fn limit(resource: &'static str) -> PackRefsError {
    PackRefsError::Limit {
        resource,
        report: Box::default(),
    }
}

pub(super) fn with_report(error: PackRefsError, effects: PackRefsReport) -> PackRefsError {
    match error {
        PackRefsError::Cancelled { .. } => PackRefsError::Cancelled {
            report: Box::new(effects),
        },
        PackRefsError::Limit { resource, .. } => PackRefsError::Limit {
            resource,
            report: Box::new(effects),
        },
        PackRefsError::Reference(source) => PackRefsError::Prune {
            report: Box::new(effects),
            source,
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_polled_at_the_next_batch_boundary() {
        let cancel = AtomicBool::new(false);
        let mut control = Control::<true>::new(PackRefsLimits::default(), &cancel);
        control.phase().unwrap();
        cancel.store(true, Ordering::Relaxed);
        for _ in 0..64 {
            control.step().unwrap();
        }
        assert!(matches!(
            control.step(),
            Err(PackRefsError::Cancelled { .. })
        ));
    }

    #[test]
    fn legacy_specialization_ignores_flag_and_budgets() {
        let cancel = AtomicBool::new(true);
        let limits = PackRefsLimits {
            max_entries: 0,
            max_refs: 0,
            max_output_bytes: 0,
            ..PackRefsLimits::default()
        };
        let mut control = Control::<false>::new(limits, &cancel);
        control.phase().unwrap();
        for _ in 0..130 {
            control.entry().unwrap();
            control.reference().unwrap();
        }
        control.output(usize::MAX).unwrap();
    }
}
