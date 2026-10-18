//! Reuse only complete closures computed from this one immutable verified graph and bit order.
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

use super::graph::Graph;
use super::{WriteError, WriteLimits, check};
use crate::ObjectKind;

pub(super) struct Closures<'graph, 'ids> {
    graph: &'graph Graph<'ids>,
    inverse: &'graph [usize],
    remaining: usize,
    cached: HashMap<usize, Vec<u64>>,
    retained_bytes: usize,
    max_bytes: usize,
}

impl<'graph, 'ids> Closures<'graph, 'ids> {
    pub fn new(graph: &'graph Graph<'ids>, inverse: &'graph [usize], limits: WriteLimits) -> Self {
        Self {
            graph,
            inverse,
            remaining: limits.max_visits,
            cached: HashMap::new(),
            retained_bytes: 0,
            max_bytes: limits.max_bytes,
        }
    }

    /// Visit the commit subgraph once, emitting selected ancestors before descendants. Physical
    /// record order can differ from ID order; the writer sorts its lookup rows independently.
    pub fn ancestors_first(
        &mut self,
        selected: &[usize],
        cancel: &AtomicBool,
    ) -> Result<Vec<usize>, WriteError> {
        let mut seen = vec![false; self.graph.ids.len()];
        let mut ordered = Vec::with_capacity(selected.len());
        let mut stack = Vec::new();
        for &root in selected {
            check(cancel)?;
            if seen[root] {
                continue;
            }
            stack.push((root, false));
            while let Some((index, expanded)) = stack.pop() {
                check(cancel)?;
                charge(&mut self.remaining)?;
                if expanded {
                    if selected.binary_search(&index).is_ok() {
                        ordered.push(index);
                    }
                    continue;
                }
                if seen[index] {
                    continue;
                }
                seen[index] = true;
                stack.push((index, true));
                for &target in self.graph.targets(index) {
                    check(cancel)?;
                    charge(&mut self.remaining)?;
                    if self.graph.kinds[target] == ObjectKind::Commit && !seen[target] {
                        stack.push((target, false));
                    }
                }
            }
        }
        Ok(ordered)
    }

    pub fn reachable(&mut self, root: usize, cancel: &AtomicBool) -> Result<Vec<u64>, WriteError> {
        let before = self.remaining;
        let mut bitmap = vec![0_u64; self.graph.kinds.len().div_ceil(64)];
        let mut stack = vec![root];
        while let Some(index) = stack.pop() {
            check(cancel)?;
            charge(&mut self.remaining)?;
            let bit = self.inverse[index];
            let word = &mut bitmap[bit / 64];
            let mask = 1 << (bit % 64);
            if *word & mask != 0 {
                continue;
            }
            *word |= mask;
            if self.graph.kinds[index] == ObjectKind::Commit
                && let Some(cached) = self.cached.get(&index)
            {
                for (word, &cached) in bitmap.iter_mut().zip(cached) {
                    check(cancel)?;
                    charge(&mut self.remaining)?;
                    *word |= cached;
                }
                continue;
            }
            for &target in self.graph.targets(index) {
                check(cancel)?;
                charge(&mut self.remaining)?;
                stack.push(target);
            }
        }
        // A future hit already pays for its root pop. Do not retain a closure whose word union
        // costs more than the remaining traversal, or let a large sparse inventory fill the cache
        // with cheap leaf closures. Clones are bounded separately from the output artifact bytes.
        let work = before - self.remaining;
        if work.saturating_sub(1) > bitmap.len()
            && let Some(bytes) = bitmap
                .len()
                .checked_mul(size_of::<u64>())
                .and_then(|bytes| self.retained_bytes.checked_add(bytes))
                .filter(|&bytes| bytes <= self.max_bytes)
        {
            self.cached.insert(root, bitmap.clone());
            self.retained_bytes = bytes;
        }
        Ok(bitmap)
    }
}

fn charge(budget: &mut usize) -> Result<(), WriteError> {
    *budget = budget
        .checked_sub(1)
        .ok_or(WriteError::Limit("reachability visits"))?;
    Ok(())
}
