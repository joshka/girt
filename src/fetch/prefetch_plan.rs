//! Read-only maintenance planning; the generic fetch planner retains its original policy.
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{FetchPlanError, FetchRequest, FetchUpdate, FetchUpdateKind};
use crate::fetch::Advertisement;
use crate::refs::{RefName, Target};
use crate::remote::{MappingError, MappingLimits, MappingOptions};

/// Independent forward/reverse mapping budgets plus destination and result admission.
///
/// Defaults admit one million captured names and updates, 128 MiB of captured names,
/// 256 MiB of result names and 1 GiB of name-processing work. Mapping uses its documented
/// defaults independently for forward mapping and reverse pruning. These admit Linux's refs;
/// acquisition, retained caller allocations and exact allocator overhead are separate bounds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PrefetchPlanLimits {
    pub mapping: MappingLimits,
    pub max_snapshot_entries: usize,
    pub max_snapshot_bytes: usize,
    pub max_updates: usize,
    pub max_output_bytes: usize,
    pub max_work: usize,
}

impl Default for PrefetchPlanLimits {
    fn default() -> Self {
        Self {
            mapping: MappingLimits::default(),
            max_snapshot_entries: 1_000_000,
            max_snapshot_bytes: 128 * 1024 * 1024,
            max_updates: 1_000_000,
            max_output_bytes: 256 * 1024 * 1024,
            max_work: 1024 * 1024 * 1024,
        }
    }
}

impl FetchRequest {
    /// Plans only maintenance-owned prefetch destinations, without I/O or publication.
    ///
    /// The caller admits preparation and supplies maintenance's fixed reflog/depth policy.
    /// Limits precede output clones. Cancellation is checked on entry, every 64 loop steps
    /// and before returning. A bounded hash/lookup may finish between checks. Forward mapping
    /// and reverse pruning have independent mapping budgets; no partial plan escapes failure.
    /// An absent exact source refuses the plan, matching Git maintenance prefetch.
    pub(crate) fn plan_prefetch_controlled(
        &self,
        advertisement: &Advertisement,
        limits: PrefetchPlanLimits,
        cancel: &AtomicBool,
    ) -> Result<Vec<FetchUpdate>, FetchPlanError> {
        let mut budget = Budget::new(limits, cancel)?;
        self.admit_prefetch_snapshot(limits, &mut budget)?;
        let mappings = MappingOptions::new(&self.specs)
            .with_limits(limits.mapping)
            .map_advertisement(advertisement, cancel)?;
        if mappings.len() > limits.max_updates {
            return Err(MappingError::Limit("prefetch updates").into());
        }
        let mut updates = Vec::with_capacity(mappings.len());
        for mapping in mappings {
            budget.step()?;
            if let Some(source) = &mapping.source {
                budget.work(source.name.as_bytes())?;
            }
            let (previous, kind) = match &mapping.destination {
                None => (None, FetchUpdateKind::SourceOnly),
                Some(name) => {
                    budget.work(name.as_bytes())?;
                    if !is_prefetch(name) {
                        return Err(FetchPlanError::Destination(name.clone()));
                    }
                    let previous = match self.snapshot.get(name) {
                        Some(Target::Symbolic(_)) => {
                            return Err(FetchPlanError::Symbolic(name.clone()));
                        }
                        Some(Target::Direct(id)) => Some(*id),
                        None => None,
                    };
                    let new = mapping.source.as_ref().expect("fetch mapping source").id;
                    let kind = match previous {
                        None => FetchUpdateKind::Create,
                        Some(old) if old == new => FetchUpdateKind::Unchanged,
                        Some(_) => FetchUpdateKind::PrefetchReplace,
                    };
                    (previous, kind)
                }
            };
            budget.output(
                mapping.source.as_ref().map(|source| source.name.as_bytes()),
                mapping.destination.as_ref().map(RefName::as_bytes),
            )?;
            updates.push(FetchUpdate {
                mapping,
                previous,
                kind,
            });
        }
        if self.prune {
            // Borrow keys for these lookup tables; do not copy the admitted names again.
            let pruned = {
                let mut selected = HashSet::new();
                for update in &updates {
                    budget.step()?;
                    if let Some(name) = &update.mapping.destination {
                        budget.work(name.as_bytes())?;
                        selected.insert(name.as_bytes());
                    }
                }
                let mut candidates = Vec::new();
                for (name, target) in &self.snapshot {
                    budget.step()?;
                    budget.work(name.as_bytes())?;
                    if is_prefetch(name)
                        && matches!(target, Target::Direct(_))
                        && !selected.contains(name.as_bytes())
                    {
                        candidates.push(name);
                    }
                }
                MappingOptions::new(&self.specs)
                    .with_limits(limits.mapping)
                    .prune_candidates(
                        &candidates,
                        advertisement,
                        &self.retained_missing_sources,
                        cancel,
                    )?
            };
            for name in pruned {
                budget.step()?;
                budget.output(None, Some(name.as_bytes()))?;
                let Target::Direct(previous) = self.snapshot[name] else {
                    unreachable!("prune candidates are direct");
                };
                updates.push(FetchUpdate {
                    mapping: crate::remote::Mapping {
                        source: None,
                        destination: Some(name.clone()),
                        force: crate::remote::ForceIntent::Normal,
                    },
                    previous: Some(previous),
                    kind: FetchUpdateKind::Prune,
                });
            }
        }
        budget.check()?;
        Ok(updates)
    }

    fn admit_prefetch_snapshot(
        &self,
        limits: PrefetchPlanLimits,
        budget: &mut Budget<'_>,
    ) -> Result<(), MappingError> {
        let entries = self
            .snapshot
            .len()
            .checked_add(self.retained_missing_sources.len());
        if entries.is_none_or(|entries| entries > limits.max_snapshot_entries) {
            return Err(MappingError::Limit("prefetch snapshot entries"));
        }
        let mut bytes = limits.max_snapshot_bytes;
        for (name, target) in &self.snapshot {
            budget.step()?;
            budget.work(name.as_bytes())?;
            debit(&mut bytes, name.as_bytes().len(), "prefetch snapshot bytes")?;
            if let Target::Symbolic(target) = target {
                budget.work(target.as_bytes())?;
                debit(
                    &mut bytes,
                    target.as_bytes().len(),
                    "prefetch snapshot bytes",
                )?;
            }
        }
        for source in &self.retained_missing_sources {
            budget.step()?;
            budget.work(source)?;
            debit(&mut bytes, source.len(), "prefetch snapshot bytes")?;
        }
        Ok(())
    }
}

fn is_prefetch(name: &RefName) -> bool {
    name.as_bytes().starts_with(b"refs/prefetch/")
}

struct Budget<'a> {
    cancel: &'a AtomicBool,
    remaining: usize,
    work: usize,
    updates: usize,
    bytes: usize,
}

impl<'a> Budget<'a> {
    fn new(limits: PrefetchPlanLimits, cancel: &'a AtomicBool) -> Result<Self, MappingError> {
        let budget = Self {
            cancel,
            remaining: 64,
            work: limits.max_work,
            updates: limits.max_updates,
            bytes: limits.max_output_bytes,
        };
        budget.check()?;
        Ok(budget)
    }

    fn check(&self) -> Result<(), MappingError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(MappingError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn step(&mut self) -> Result<(), MappingError> {
        debit(&mut self.work, 1, "prefetch planning work")?;
        self.remaining -= 1;
        if self.remaining == 0 {
            self.check()?;
            self.remaining = 64;
        }
        Ok(())
    }

    fn work(&mut self, name: &[u8]) -> Result<(), MappingError> {
        debit(&mut self.work, name.len(), "prefetch planning work")
    }

    fn output(
        &mut self,
        source: Option<&[u8]>,
        destination: Option<&[u8]>,
    ) -> Result<(), MappingError> {
        debit(&mut self.updates, 1, "prefetch updates")?;
        for name in [source, destination].into_iter().flatten() {
            debit(&mut self.bytes, name.len(), "prefetch output bytes")?;
        }
        Ok(())
    }
}

fn debit(remaining: &mut usize, amount: usize, name: &'static str) -> Result<(), MappingError> {
    *remaining = remaining
        .checked_sub(amount)
        .ok_or(MappingError::Limit(name))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_observed_at_the_batch_boundary() {
        let cancel = AtomicBool::new(false);
        let mut budget = Budget::new(PrefetchPlanLimits::default(), &cancel).unwrap();
        for _ in 0..63 {
            budget.step().unwrap();
        }
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(budget.step(), Err(MappingError::Cancelled)));
    }
}
