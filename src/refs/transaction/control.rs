//! Stack-local preparation accounting, compiled out for trusted transactions.
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::refs::store::{check_path, conflicts, io_error, parse_loose, read_optional};
use crate::refs::{
    Expected, FilesTransactionLimits, RefEdit, RefName, ReferenceError, References, Target, packed,
};

pub(super) struct Control<'a, const CONTROLLED: bool> {
    limits: FilesTransactionLimits,
    cancel: &'a AtomicBool,
    input: usize,
    prepared: usize,
    remaining: usize,
}

impl<'a, const CONTROLLED: bool> Control<'a, CONTROLLED> {
    pub fn new(limits: FilesTransactionLimits, cancel: &'a AtomicBool) -> Self {
        Self {
            limits,
            cancel,
            input: 0,
            prepared: 0,
            remaining: 0,
        }
    }

    pub fn phase(&mut self) -> Result<(), ReferenceError> {
        if CONTROLLED {
            super::check_cancelled(self.cancel)?;
            self.remaining = 64;
        }
        Ok(())
    }

    pub fn step(&mut self) -> Result<(), ReferenceError> {
        if CONTROLLED {
            if self.remaining == 0 {
                self.phase()?;
            }
            self.remaining -= 1;
        }
        Ok(())
    }

    pub fn edits(&mut self, edits: &[RefEdit]) -> Result<(), ReferenceError> {
        if CONTROLLED {
            if edits.len() > self.limits.max_edits {
                return Err(ReferenceError::Limit("transaction edits"));
            }
            // Admit caller payloads before validation scans them or preparation clones them.
            for edit in edits {
                self.step()?;
                self.name(&edit.name)?;
                self.target(edit.target.as_ref())?;
                self.expected(&edit.expected)?;
                if let Some((committer, message)) = edit.reflog.append_fields() {
                    self.prepare(committer.name.len())?;
                    self.prepare(committer.email.len())?;
                    self.prepare(message.len())?;
                }
            }
        }
        Ok(())
    }

    pub fn prepare(&mut self, bytes: usize) -> Result<(), ReferenceError> {
        if CONTROLLED {
            charge(
                &mut self.prepared,
                bytes,
                self.limits.max_prepared_bytes,
                "transaction preparation bytes",
            )?;
        }
        Ok(())
    }

    pub fn name(&mut self, name: &RefName) -> Result<(), ReferenceError> {
        if CONTROLLED {
            self.prepare(name.as_bytes().len())?;
        }
        Ok(())
    }

    pub fn target(&mut self, target: Option<&Target>) -> Result<(), ReferenceError> {
        if CONTROLLED {
            match target {
                Some(Target::Direct(id)) => self.prepare(id.as_bytes().len() * 2 + 1)?,
                Some(Target::Symbolic(name)) => {
                    self.name(name)?;
                    self.prepare(6)?;
                }
                None => (),
            }
        }
        Ok(())
    }

    pub fn expected(&mut self, expected: &Expected) -> Result<(), ReferenceError> {
        if CONTROLLED {
            match expected {
                Expected::Value(target) | Expected::AbsentOr(target) => {
                    self.target(Some(target))?
                }
                Expected::Any | Expected::Absent | Expected::Exists => (),
            }
        }
        Ok(())
    }

    pub fn reflog(
        &mut self,
        committer: &crate::Signature,
        message: &[u8],
    ) -> Result<(), ReferenceError> {
        if CONTROLLED {
            // Raw signature/message clones plus encoded copies. The fixed allowance covers both
            // SHA-256 IDs, timestamp, timezone, framing and separators before encoding allocates.
            for bytes in [committer.name.len(), committer.email.len(), message.len()] {
                self.prepare(bytes)?;
                self.prepare(bytes)?;
            }
            self.prepare(256)?;
        }
        Ok(())
    }

    pub fn packed(
        &mut self,
        refs: &References<'_>,
        path: &Path,
    ) -> Result<Arc<packed::Snapshot>, ReferenceError> {
        if CONTROLLED {
            self.phase()?;
            let fallback = packed::PackedCache::default();
            let cache = refs.packed_cache.unwrap_or(&fallback);
            let snapshot = cache.load_controlled(
                refs.object_format,
                path,
                self.limits.max_input_bytes - self.input,
                self.limits.max_packed_refs,
                self.cancel,
            )?;
            self.input += snapshot.bytes.len();
            Ok(snapshot)
        } else {
            refs.packed_at(path)
        }
    }

    fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>, ReferenceError> {
        if !CONTROLLED {
            return read_optional(path);
        }
        self.phase()?;
        check_path(path)?;
        let Some(file) = crate::refs::enumerate::open_walked(path)? else {
            return Ok(None);
        };
        let remaining = self.limits.max_input_bytes - self.input;
        if file
            .metadata()
            .map_err(|source| io_error(path, source))?
            .len()
            > remaining as u64
        {
            return Err(ReferenceError::Limit("reference bytes"));
        }
        let mut bytes = Vec::new();
        file.take(remaining.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|source| io_error(path, source))?;
        charge(
            &mut self.input,
            bytes.len(),
            self.limits.max_input_bytes,
            "reference bytes",
        )?;
        self.phase()?;
        Ok(Some(bytes))
    }

    pub fn read_locked(
        &mut self,
        refs: &References<'_>,
        name: &RefName,
        packed: &packed::Packed,
    ) -> Result<Option<Target>, ReferenceError> {
        if !CONTROLLED {
            return refs.read_locked(name, packed);
        }
        let path = refs.path(name)?;
        if let Some(bytes) = self.read(&path)? {
            return parse_loose(refs.object_format, &bytes, &path).map(Some);
        }
        Ok(packed
            .get(name)
            .filter(|_| !name.per_worktree())
            .map(|record| Target::Direct(record.target)))
    }

    pub fn namespace(
        &mut self,
        refs: &References<'_>,
        name: &RefName,
        packed: &packed::Packed,
    ) -> Result<(), ReferenceError> {
        if !CONTROLLED {
            return refs.check_packed_namespace(name, packed, || Ok(()));
        }
        // One step per ordered lookup: an ancestor probe per `/` plus the child range probe.
        refs.check_packed_namespace(name, packed, || self.step())
    }

    pub fn overlap(
        &mut self,
        names: &BTreeSet<RefName>,
        name: &RefName,
    ) -> Result<bool, ReferenceError> {
        if !CONTROLLED {
            return Ok(names
                .iter()
                .any(|other| conflicts(name.as_bytes(), other.as_bytes())));
        }
        for other in names {
            self.step()?;
            if conflicts(name.as_bytes(), other.as_bytes()) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn append_tail(&mut self, path: &Path) -> Result<(), ReferenceError> {
        if CONTROLLED {
            self.phase()?;
            let bytes = crate::refs::reflog::check_append_tail_controlled(
                path,
                self.limits.max_input_bytes - self.input,
            )?;
            self.input += bytes;
            self.phase()?;
            Ok(())
        } else {
            crate::refs::reflog::check_append_tail(path)
        }
    }
}

fn charge(
    used: &mut usize,
    bytes: usize,
    maximum: usize,
    resource: &'static str,
) -> Result<(), ReferenceError> {
    let total = used
        .checked_add(bytes)
        .ok_or(ReferenceError::Limit(resource))?;
    if total > maximum {
        return Err(ReferenceError::Limit(resource));
    }
    *used = total;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;

    #[test]
    fn preparation_scan_observes_cancellation_at_the_next_batch_boundary() {
        let cancel = AtomicBool::new(false);
        let mut control = Control::<true>::new(FilesTransactionLimits::default(), &cancel);
        control.phase().unwrap();
        cancel.store(true, Ordering::Relaxed);
        for _ in 0..64 {
            control.step().unwrap();
        }
        assert!(matches!(control.step(), Err(ReferenceError::Cancelled)));
    }

    #[test]
    fn preparation_accounting_refuses_overflow() {
        let cancel = AtomicBool::new(false);
        let limits = FilesTransactionLimits::default().with_max_prepared_bytes(usize::MAX);
        let mut control = Control::<true>::new(limits, &cancel);
        control.prepare(usize::MAX).unwrap();
        assert!(matches!(control.prepare(1), Err(ReferenceError::Limit(_))));
    }
}
