//! Two-color reachability with generation-ordered exclusion stopping.
use std::collections::BinaryHeap;

use super::Graph;
use crate::{HistoryError, HistoryLimits, IdMap, ObjectId, Objects};

impl Objects {
    /// Collects commits reachable from `include` but not from `exclude`, using default bounds.
    ///
    /// Every result appears once; result order is unspecified. With an attached index covering
    /// every root and a checked strict generation order, traversal stops when no interesting
    /// commit remains queued. Otherwise it collects both closures. Shallow boundaries stop edges.
    ///
    /// # Errors
    ///
    /// Returns history read, kind, parse, cycle or resource errors for explored commits. Indexed
    /// metadata uses the explicit trust contract of [`Self::with_commit_graph`]. Ancestors beyond
    /// a generation stopping boundary are not read or verified.
    pub fn walk_range(
        &self,
        include: &[ObjectId],
        exclude: &[ObjectId],
    ) -> Result<Vec<ObjectId>, HistoryError> {
        self.walk_range_with(include, exclude, HistoryLimits::default())
    }

    /// Collects a reachability difference under explicit commit, edge and fallback-read bounds.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::walk_range`]. Bounds count both sides, including excluded
    /// commits actually explored. Empty includes return immediately without storage access.
    pub fn walk_range_with(
        &self,
        include: &[ObjectId],
        exclude: &[ObjectId],
        limits: HistoryLimits,
    ) -> Result<Vec<ObjectId>, HistoryError> {
        if include.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(graph) = self.commit_graph.as_ref().filter(|graph| {
            include
                .iter()
                .chain(exclude)
                .all(|id| graph.generation(*id).is_some())
        }) {
            return paint(self, graph, include, exclude, limits);
        }
        let roots: Vec<_> = include.iter().chain(exclude).copied().collect();
        let graph = Graph::read(self, &roots, limits)?;
        let mut selected = vec![false; graph.ids.len()];
        let mut hidden = vec![false; graph.ids.len()];
        for root in include {
            mark(&graph, graph.positions[root], &mut selected);
        }
        for root in exclude {
            mark(&graph, graph.positions[root], &mut hidden);
        }
        Ok(graph
            .ids
            .iter()
            .enumerate()
            .filter_map(|(i, id)| (selected[i] && !hidden[i]).then_some(*id))
            .collect())
    }
}

fn mark(graph: &Graph, root: usize, marks: &mut [bool]) {
    let mut stack = vec![root];
    while let Some(index) = stack.pop() {
        if std::mem::replace(&mut marks[index], true) {
            continue;
        }
        stack.extend(&graph.parents[index]);
    }
}

struct Paint {
    marks: IdMap<u8>,
    queue: BinaryHeap<(u64, ObjectId)>,
    interesting: usize,
    remaining_edges: usize,
    max_commits: usize,
}

impl Paint {
    fn enqueue(&mut self, id: ObjectId, generation: u64, color: u8) -> Result<(), HistoryError> {
        let old = self.marks.get(&id).copied().unwrap_or(0);
        let new = old | color;
        if old == new {
            return Ok(());
        }
        if old == 0 {
            if self.marks.len() >= self.max_commits {
                return Err(HistoryError::Limit("commits"));
            }
            self.queue.push((generation, id));
        }
        self.interesting += usize::from(new == 1);
        self.interesting -= usize::from(old == 1);
        self.marks.insert(id, new);
        Ok(())
    }
}

fn paint(
    objects: &Objects,
    graph: &crate::commit_graph::CommitGraph,
    include: &[ObjectId],
    exclude: &[ObjectId],
    limits: HistoryLimits,
) -> Result<Vec<ObjectId>, HistoryError> {
    let mut state = Paint {
        marks: IdMap::default(),
        queue: BinaryHeap::new(),
        interesting: 0,
        remaining_edges: limits.max_parents,
        max_commits: limits.max_commits,
    };
    for (roots, color) in [(include, 1), (exclude, 2)] {
        for root in roots {
            let record = graph.get(*root).ok_or(HistoryError::Missing(*root))?;
            state.enqueue(
                *root,
                graph
                    .generation(record.id)
                    .ok_or(HistoryError::Missing(record.id))?,
                color,
            )?;
        }
    }
    let mut result = Vec::new();
    while state.interesting != 0 {
        let Some((_, id)) = state.queue.pop() else {
            break;
        };
        let color = state.marks[&id];
        if color == 1 {
            state.interesting -= 1;
            result.push(id);
        }
        if objects.shallow_roots().contains(id) {
            continue;
        }
        let record = graph.get(id).ok_or(HistoryError::Missing(id))?;
        state.remaining_edges = state
            .remaining_edges
            .checked_sub(record.parents.len())
            .ok_or(HistoryError::Limit("parent occurrences"))?;
        for parent in &record.parents {
            let record = graph.get(*parent).ok_or(HistoryError::Missing(*parent))?;
            state.enqueue(
                *parent,
                graph
                    .generation(record.id)
                    .ok_or(HistoryError::Missing(record.id))?,
                color,
            )?;
        }
    }
    Ok(result)
}

impl Objects {
    pub(super) fn indexed_ancestor(
        &self,
        ancestor: ObjectId,
        descendant: ObjectId,
        limits: HistoryLimits,
    ) -> Option<Result<bool, HistoryError>> {
        let graph = self.commit_graph.as_ref()?;
        let floor = graph.generation(ancestor)?;
        graph.generation(descendant)?;
        Some(ancestor_query(
            self, graph, ancestor, descendant, floor, limits,
        ))
    }
}

fn ancestor_query(
    objects: &Objects,
    graph: &crate::commit_graph::CommitGraph,
    ancestor: ObjectId,
    descendant: ObjectId,
    floor: u64,
    limits: HistoryLimits,
) -> Result<bool, HistoryError> {
    let mut seen = crate::IdSet::default();
    seen.insert(ancestor);
    seen.insert(descendant);
    if seen.len() > limits.max_commits {
        return Err(HistoryError::Limit("commits"));
    }
    let mut remaining = limits.max_parents;
    let mut stack = vec![descendant];
    while let Some(id) = stack.pop() {
        if id == ancestor {
            return Ok(true);
        }
        let record = graph.get(id).ok_or(HistoryError::Missing(id))?;
        if graph
            .generation(record.id)
            .is_some_and(|generation| generation <= floor)
            || objects.shallow_roots().contains(id)
        {
            continue;
        }
        remaining = remaining
            .checked_sub(record.parents.len())
            .ok_or(HistoryError::Limit("parent occurrences"))?;
        for parent in &record.parents {
            if *parent == ancestor {
                return Ok(true);
            }
            if seen.insert(*parent) {
                if seen.len() > limits.max_commits {
                    return Err(HistoryError::Limit("commits"));
                }
                stack.push(*parent);
            }
        }
    }
    Ok(false)
}
