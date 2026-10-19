//! Explicitly budgeted mapping; trusted entry points retain their original algorithm.
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Direction, ForceIntent, Mapping, MappingError, Plan, RefSource, Refspec, Refspecs};
use crate::fetch::{Advertisement, AdvertisementKind};

/// Admission and work budgets for pure reference mapping.
///
/// Defaults admit one million sources, 4,096 specs, 128 MiB of input names/specs,
/// one million output attempts, 256 MiB of output names and 1 GiB of comparison work.
/// Names and specs are already caller-owned. These limits bound additional mapping work,
/// not their construction or exact allocator overhead.
// New limit fields must remain Copy.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct MappingLimits {
    /// Input sources or advertisement entries, including peeled hints.
    pub max_sources: usize,
    /// Parsed refspec occurrences, including exclusions and duplicates.
    pub max_specs: usize,
    /// Aggregate raw refspec and input name bytes; capabilities are not inspected.
    pub max_input_bytes: usize,
    /// Attempted mappings plus missing-source reports, including duplicate attempts.
    pub max_mappings: usize,
    /// Logical source/destination name bytes per attempt, or missing-source report bytes.
    ///
    /// Deduplication does not refund these charges. Internal sets retain a constant number
    /// of copies of admitted names; this is not exact heap accounting.
    pub max_output_bytes: usize,
    /// Loop steps plus source/pattern bytes examined by comparisons, including exclusions.
    pub max_work: usize,
}

impl Default for MappingLimits {
    fn default() -> Self {
        Self {
            max_sources: 1_000_000,
            max_specs: 4096,
            max_input_bytes: 128 * 1024 * 1024,
            max_mappings: 1_000_000,
            max_output_bytes: 256 * 1024 * 1024,
            max_work: 1024 * 1024 * 1024,
        }
    }
}

impl MappingLimits {
    /// Sets [`Self::max_sources`].
    #[must_use]
    pub fn with_max_sources(mut self, value: usize) -> Self {
        self.max_sources = value;
        self
    }
    /// Sets [`Self::max_specs`].
    #[must_use]
    pub fn with_max_specs(mut self, value: usize) -> Self {
        self.max_specs = value;
        self
    }
    /// Sets [`Self::max_input_bytes`].
    #[must_use]
    pub fn with_max_input_bytes(mut self, value: usize) -> Self {
        self.max_input_bytes = value;
        self
    }
    /// Sets [`Self::max_mappings`].
    #[must_use]
    pub fn with_max_mappings(mut self, value: usize) -> Self {
        self.max_mappings = value;
        self
    }
    /// Sets [`Self::max_output_bytes`].
    #[must_use]
    pub fn with_max_output_bytes(mut self, value: usize) -> Self {
        self.max_output_bytes = value;
        self
    }
    /// Sets [`Self::max_work`].
    #[must_use]
    pub fn with_max_work(mut self, value: usize) -> Self {
        self.max_work = value;
        self
    }
}

/// Explicit controlled mapping of an already-parsed refspec list.
///
/// No I/O or mutation occurs. Limits precede name clones, and cancellation is observed
/// on entry and at most every 64 loop steps. One bounded name comparison, hashing or
/// validation may finish before the next poll. Failure returns no partial plan.
/// Existing [`Refspecs`] mapping methods remain unbounded trusted alternatives.
///
/// # Examples
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::remote::{Direction, MappingLimits, MappingOptions, Refspecs};
///
/// let specs = Refspecs::parse(Direction::Fetch, ["refs/heads/*:refs/remotes/origin/*"])?;
/// let mappings = MappingOptions::new(&specs)
///     .with_limits(MappingLimits::default().with_max_sources(100))
///     .map(&[], &AtomicBool::new(false))?;
/// assert!(mappings.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct MappingOptions<'a> {
    specs: &'a Refspecs,
    limits: MappingLimits,
}

impl<'a> MappingOptions<'a> {
    /// Selects refspecs and default budgets, without performing work.
    pub fn new(specs: &'a Refspecs) -> Self {
        Self {
            specs,
            limits: MappingLimits::default(),
        }
    }

    /// Replaces the mapping budgets.
    #[must_use]
    pub fn with_limits(mut self, limits: MappingLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Maps explicit sources, refusing an absent exact selector.
    ///
    /// # Errors
    ///
    /// Returns ordinary mapping errors, [`MappingError::Limit`] or [`MappingError::Cancelled`].
    pub fn map(
        self,
        sources: &[RefSource],
        cancel: &AtomicBool,
    ) -> Result<Vec<Mapping>, MappingError> {
        complete(self.map_with_missing(sources, cancel)?)
    }

    /// Maps explicit sources and separately reports absent exact selectors.
    ///
    /// # Errors
    ///
    /// Returns ordinary validation/collision errors, limits or cancellation without a partial plan.
    pub fn map_with_missing(
        self,
        sources: &[RefSource],
        cancel: &AtomicBool,
    ) -> Result<(Vec<Mapping>, Vec<Vec<u8>>), MappingError> {
        let mut budget = Budget::new(self.limits, cancel)?;
        self.admit(
            sources.len(),
            sources.iter().map(|source| source.name.as_bytes()),
            &mut budget,
        )?;
        self.map_admitted(sources, &mut budget)
    }

    /// Maps advertisement tips; peeled hints count toward input admission but cannot be selected.
    ///
    /// # Errors
    ///
    /// Returns direction, mapping, limit or cancellation errors without a partial plan.
    pub fn map_advertisement(
        self,
        advertisement: &Advertisement,
        cancel: &AtomicBool,
    ) -> Result<Vec<Mapping>, MappingError> {
        complete(self.map_advertisement_with_missing(advertisement, cancel)?)
    }

    /// Maps advertisement tips and separately reports absent exact selectors.
    ///
    /// Capabilities are not inspected or copied. Input budgets cover all entries before any
    /// tip names are cloned. The caller bounds advertisement acquisition separately.
    ///
    /// # Errors
    ///
    /// Returns direction, validation/collision, limit or cancellation errors without a partial
    /// plan.
    pub fn map_advertisement_with_missing(
        self,
        advertisement: &Advertisement,
        cancel: &AtomicBool,
    ) -> Result<(Vec<Mapping>, Vec<Vec<u8>>), MappingError> {
        let mut budget = Budget::new(self.limits, cancel)?;
        if self.specs.direction != Direction::Fetch {
            return Err(MappingError::Direction);
        }
        self.admit(
            advertisement.refs.len(),
            advertisement
                .refs
                .iter()
                .map(|reference| reference.name.as_bytes()),
            &mut budget,
        )?;
        let mut sources = Vec::new();
        for reference in &advertisement.refs {
            budget.step()?;
            if reference.kind == AdvertisementKind::Tip {
                sources.push(RefSource {
                    name: reference.name.clone(),
                    id: reference.id,
                });
            }
        }
        self.map_admitted(&sources, &mut budget)
    }

    /// Reverse matching for maintenance pruning, under one budget for the complete phase.
    /// Borrow output names; the planner admits its own result clones separately.
    pub(crate) fn prune_candidates<'n>(
        self,
        destinations: &[&'n crate::refs::RefName],
        advertisement: &Advertisement,
        retained: &std::collections::BTreeSet<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> Result<Vec<&'n crate::refs::RefName>, MappingError> {
        let mut budget = Budget::new(self.limits, cancel)?;
        if self.specs.direction != Direction::Fetch {
            return Err(MappingError::Direction);
        }
        let count = advertisement
            .refs
            .len()
            .checked_add(destinations.len())
            .and_then(|count| count.checked_add(retained.len()))
            .ok_or(MappingError::Limit("sources"))?;
        self.admit(
            count,
            advertisement
                .refs
                .iter()
                .map(|reference| reference.name.as_bytes())
                .chain(destinations.iter().map(|name| name.as_bytes()))
                .chain(retained.iter().map(Vec::as_slice)),
            &mut budget,
        )?;
        let mut advertised = std::collections::HashSet::new();
        for reference in &advertisement.refs {
            budget.step()?;
            if reference.kind == AdvertisementKind::Tip {
                budget.comparison(b"", reference.name.as_bytes())?;
                advertised.insert(reference.name.as_bytes());
            }
        }
        let mut pruned = Vec::new();
        for &destination in destinations {
            budget.step()?;
            let mut owned = false;
            let mut protected = false;
            for spec in &self.specs.specs {
                budget.step()?;
                let Some(source) = reverse_source(spec, destination, &mut budget)? else {
                    continue;
                };
                let mut excluded = false;
                for negative in &self.specs.specs {
                    budget.step()?;
                    if !negative.negative {
                        continue;
                    }
                    budget.comparison(&negative.source, &source)?;
                    if negative.capture(&source).is_some() {
                        excluded = true;
                        break;
                    }
                }
                if excluded {
                    continue;
                }
                owned = true;
                budget.comparison(b"", &source)?;
                if advertised.contains(source.as_slice()) || retained.contains(source.as_slice()) {
                    protected = true;
                    break;
                }
            }
            if owned && !protected {
                pruned.push(destination);
            }
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(MappingError::Cancelled);
        }
        Ok(pruned)
    }

    fn admit<'b>(
        self,
        count: usize,
        names: impl Iterator<Item = &'b [u8]>,
        budget: &mut Budget<'_>,
    ) -> Result<(), MappingError> {
        if count > self.limits.max_sources {
            return Err(MappingError::Limit("sources"));
        }
        if self.specs.specs.len() > self.limits.max_specs {
            return Err(MappingError::Limit("refspecs"));
        }
        let mut bytes = self.limits.max_input_bytes;
        for spec in &self.specs.specs {
            budget.step()?;
            debit(&mut bytes, spec.raw.len(), "input bytes")?;
        }
        for name in names {
            budget.step()?;
            debit(&mut bytes, name.len(), "input bytes")?;
        }
        Ok(())
    }

    fn map_admitted(
        self,
        sources: &[RefSource],
        budget: &mut Budget<'_>,
    ) -> Result<(Vec<Mapping>, Vec<Vec<u8>>), MappingError> {
        let mut names = std::collections::HashSet::new();
        for source in sources {
            budget.step()?;
            if !names.insert(&source.name) {
                return Err(MappingError::DuplicateSource(source.name.clone()));
            }
            if source.id.is_null() {
                return Err(MappingError::ZeroId(source.name.clone()));
            }
        }
        let mut plan = Plan::default();
        let mut missing = Vec::new();
        for spec in &self.specs.specs {
            budget.step()?;
            if spec.negative {
                continue;
            }
            if spec.source.is_empty() {
                budget.output(spec, b"", None)?;
                plan.add(mapping(spec, b"", None)?)?;
                continue;
            }
            let mut matched = false;
            for source in sources {
                budget.comparison(&spec.source, source.name.as_bytes())?;
                let Some(capture) = spec.capture(source.name.as_bytes()) else {
                    continue;
                };
                matched = true;
                let mut excluded = false;
                for negative in &self.specs.specs {
                    budget.step()?;
                    if negative.negative {
                        budget.comparison(&negative.source, source.name.as_bytes())?;
                        if negative.capture(source.name.as_bytes()).is_some() {
                            excluded = true;
                            break;
                        }
                    }
                }
                if !excluded {
                    budget.output(spec, capture, Some(source))?;
                    plan.add(mapping(spec, capture, Some(source))?)?;
                }
            }
            if !matched && !spec.wildcard {
                budget.report(spec.source.len())?;
                missing.push(spec.source.clone());
            }
        }
        Ok((plan.mappings, missing))
    }
}

fn complete(
    (mappings, missing): (Vec<Mapping>, Vec<Vec<u8>>),
) -> Result<Vec<Mapping>, MappingError> {
    match missing.into_iter().next() {
        Some(source) => Err(MappingError::UnmatchedSource(source)),
        None => Ok(mappings),
    }
}

fn mapping(
    spec: &Refspec,
    capture: &[u8],
    source: Option<&RefSource>,
) -> Result<Mapping, MappingError> {
    Ok(Mapping {
        source: source.cloned(),
        destination: spec.destination(capture)?,
        force: if spec.force {
            ForceIntent::Requested
        } else {
            ForceIntent::Normal
        },
    })
}

/// The controlled counterpart of `source_for_destination`: charge the constructed source
/// before allocating it. Keep the trusted implementation unchanged.
fn reverse_source(
    spec: &Refspec,
    destination: &crate::refs::RefName,
    budget: &mut Budget<'_>,
) -> Result<Option<Vec<u8>>, MappingError> {
    if spec.negative {
        return Ok(None);
    }
    let Some(pattern) = &spec.destination else {
        return Ok(None);
    };
    budget.comparison(pattern, destination.as_bytes())?;
    let capture = if let Some(star) = pattern.iter().position(|byte| *byte == b'*') {
        let Some(capture) = destination
            .as_bytes()
            .strip_prefix(&pattern[..star])
            .and_then(|capture| capture.strip_suffix(&pattern[star + 1..]))
        else {
            return Ok(None);
        };
        capture
    } else if pattern == destination.as_bytes() {
        b""
    } else {
        return Ok(None);
    };
    budget.comparison(&spec.source, capture)?;
    if let Some(star) = spec.source.iter().position(|byte| *byte == b'*') {
        let bytes = spec
            .source
            .len()
            .checked_sub(1)
            .and_then(|bytes| bytes.checked_add(capture.len()))
            .ok_or(MappingError::Limit("output bytes"))?;
        budget.report(bytes)?;
        Ok(Some(
            [&spec.source[..star], capture, &spec.source[star + 1..]].concat(),
        ))
    } else {
        budget.report(spec.source.len())?;
        Ok(Some(spec.source.clone()))
    }
}

struct Budget<'a> {
    cancel: &'a AtomicBool,
    remaining: usize,
    mappings: usize,
    bytes: usize,
    work: usize,
}

impl<'a> Budget<'a> {
    fn new(limits: MappingLimits, cancel: &'a AtomicBool) -> Result<Self, MappingError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(MappingError::Cancelled);
        }
        Ok(Self {
            cancel,
            remaining: 64,
            mappings: limits.max_mappings,
            bytes: limits.max_output_bytes,
            work: limits.max_work,
        })
    }

    fn step(&mut self) -> Result<(), MappingError> {
        debit(&mut self.work, 1, "comparison work")?;
        self.remaining -= 1;
        if self.remaining == 0 {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(MappingError::Cancelled);
            }
            self.remaining = 64;
        }
        Ok(())
    }

    fn comparison(&mut self, pattern: &[u8], name: &[u8]) -> Result<(), MappingError> {
        self.step()?;
        debit(&mut self.work, pattern.len(), "comparison work")?;
        debit(&mut self.work, name.len(), "comparison work")
    }

    fn report(&mut self, bytes: usize) -> Result<(), MappingError> {
        debit(&mut self.mappings, 1, "mapping attempts")?;
        debit(&mut self.bytes, bytes, "output bytes")
    }

    fn output(
        &mut self,
        spec: &Refspec,
        capture: &[u8],
        source: Option<&RefSource>,
    ) -> Result<(), MappingError> {
        self.report(source.map_or(0, |source| source.name.as_bytes().len()))?;
        if let Some(pattern) = &spec.destination {
            let bytes = if spec.wildcard {
                pattern.len() - 1
            } else {
                pattern.len()
            };
            debit(&mut self.bytes, bytes, "output bytes")?;
            if spec.wildcard {
                debit(&mut self.bytes, capture.len(), "output bytes")?;
            }
        }
        Ok(())
    }
}

fn debit(remaining: &mut usize, bytes: usize, name: &'static str) -> Result<(), MappingError> {
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or(MappingError::Limit(name))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_polled_at_the_batch_boundary() {
        let cancel = AtomicBool::new(false);
        let mut budget = Budget::new(MappingLimits::default(), &cancel).unwrap();
        for _ in 0..63 {
            budget.step().unwrap();
        }
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(budget.step(), Err(MappingError::Cancelled)));
    }

    #[test]
    fn skipped_exclusion_entries_still_charge_work() {
        let cancel = AtomicBool::new(false);
        let mut budget = Budget::new(MappingLimits::default().with_max_work(1), &cancel).unwrap();
        budget.step().unwrap();
        assert!(matches!(
            budget.step(),
            Err(MappingError::Limit("comparison work"))
        ));
    }
}
