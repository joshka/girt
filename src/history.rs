//! Bounded ancestry queries over verified loose and packed commit objects.
use std::collections::{HashMap, VecDeque};

use crate::{Commit, CommitError, ObjectId, ObjectKind, ObjectReadError, Objects, ReadLimits};

/// Resource bounds for one complete history operation.
///
/// Bounds cover distinct commits (including queued roots/parents), parent occurrences (including
/// duplicates), and each object read. Memory is O(commits + parents), plus one decoded commit and
/// the reader's pack snapshot. These are input bounds, not a total heap or wall-clock limit.
#[derive(Clone, Copy, Debug)]
pub struct HistoryLimits {
    /// Maximum distinct commits; zero permits only an empty walk (default 100,000).
    pub max_commits: usize,
    /// Maximum parent occurrences (default 1,000,000).
    pub max_parents: usize,
    /// Bounds applied separately to every object read.
    pub read: ReadLimits,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            max_commits: 100_000,
            max_parents: 1_000_000,
            read: ReadLimits::default(),
        }
    }
}

impl Objects {
    /// Collects all commits reachable from explicit roots, including the roots themselves.
    ///
    /// Order is breadth-first: roots in supplied order, then parents in stored order. First
    /// discovery wins; duplicate roots and parents never duplicate output. This is **not
    /// topological order**: an ancestor supplied as a root can precede its descendant. Timestamps
    /// are ignored. An empty root slice returns an empty vector without reading storage.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError`] on any reachable missing, non-commit, malformed or corrupt object,
    /// cycle, or exhausted bound. No partial result is returned and no files are changed. Commit
    /// syntax uses [`Commit::parse`] without additional field validation; trees are not read.
    pub fn walk(
        &self,
        roots: &[ObjectId],
        limits: HistoryLimits,
    ) -> Result<Vec<ObjectId>, HistoryError> {
        Ok(Graph::read(self, roots, limits)?.ids)
    }

    /// Tests whether `ancestor` is reachable from `descendant`, including equality.
    ///
    /// # Errors
    ///
    /// Validates the complete ancestry of both endpoints before answering, even for identical
    /// endpoints or an early match. Returns the same failures as [`Self::walk`].
    pub fn is_ancestor(
        &self,
        ancestor: ObjectId,
        descendant: ObjectId,
        limits: HistoryLimits,
    ) -> Result<bool, HistoryError> {
        let graph = Graph::read(self, &[descendant, ancestor], limits)?;
        let marks = graph.reachable(0);
        Ok(marks[graph.positions[&ancestor]])
    }

    /// Returns all best common ancestors of two commits, sorted by raw object ID.
    ///
    /// A best common ancestor has no distinct common descendant. Criss-cross histories can return
    /// several bases; disconnected histories return none and identical endpoints return themselves.
    /// Runs in O(V + E) graph work plus O(B log B) sorting for B bases, without timestamp
    /// assumptions.
    ///
    /// # Errors
    ///
    /// Validates both complete ancestries; returns the same failures as [`Self::walk`].
    pub fn merge_bases(
        &self,
        left: ObjectId,
        right: ObjectId,
        limits: HistoryLimits,
    ) -> Result<Vec<ObjectId>, HistoryError> {
        let graph = Graph::read(self, &[left, right], limits)?;
        let left_marks = graph.reachable(0);
        let right_marks = graph.reachable(graph.positions[&right]);
        let common: Vec<_> = left_marks
            .iter()
            .zip(&right_marks)
            .map(|(a, b)| *a && *b)
            .collect();
        let mut dominated = vec![false; graph.ids.len()];
        // Common ancestry is closed under following parents. Every non-best common ancestor
        // is therefore a direct parent of another common ancestor somewhere in that ancestry.
        for (index, parents) in graph.parents.iter().enumerate() {
            if common[index] {
                for &parent in parents {
                    dominated[parent] = true;
                }
            }
        }
        let mut bases: Vec<_> = graph
            .ids
            .iter()
            .enumerate()
            .filter_map(|(i, id)| (common[i] && !dominated[i]).then_some(*id))
            .collect();
        bases.sort();
        Ok(bases)
    }
}

struct Graph {
    ids: Vec<ObjectId>,
    positions: HashMap<ObjectId, usize>,
    parents: Vec<Vec<usize>>,
}

impl Graph {
    fn read(
        objects: &Objects,
        roots: &[ObjectId],
        limits: HistoryLimits,
    ) -> Result<Self, HistoryError> {
        let mut graph = Self {
            ids: vec![],
            positions: HashMap::new(),
            parents: vec![],
        };
        for &root in roots {
            graph.discover(root, limits.max_commits)?;
        }
        let mut remaining = limits.max_parents;
        let mut index = 0;
        while index < graph.ids.len() {
            let id = graph.ids[index];
            let object = objects
                .read(id, limits.read)
                .map_err(|source| HistoryError::Read { id, source })?
                .ok_or(HistoryError::Missing(id))?;
            if object.kind() != ObjectKind::Commit {
                return Err(HistoryError::NotCommit(id));
            }
            let commit = Commit::parse(object.data())
                .map_err(|source| HistoryError::Parse { id, source })?;
            let parents = &commit.fields().parents;
            remaining = remaining
                .checked_sub(parents.len())
                .ok_or(HistoryError::Limit("parent occurrences"))?;
            let mut edges = Vec::with_capacity(parents.len());
            for &parent in parents {
                edges.push(graph.discover(parent, limits.max_commits)?);
            }
            graph.parents.push(edges);
            index += 1;
        }
        graph.check_acyclic()?;
        Ok(graph)
    }

    fn discover(&mut self, id: ObjectId, limit: usize) -> Result<usize, HistoryError> {
        if let Some(&index) = self.positions.get(&id) {
            return Ok(index);
        }
        if self.ids.len() == limit {
            return Err(HistoryError::Limit("commits"));
        }
        let index = self.ids.len();
        self.ids.push(id);
        self.positions.insert(id, index);
        Ok(index)
    }

    fn check_acyclic(&self) -> Result<(), HistoryError> {
        let mut children = vec![0usize; self.ids.len()];
        for parents in &self.parents {
            for &parent in parents {
                children[parent] += 1;
            }
        }
        let mut ready: VecDeque<_> = children
            .iter()
            .enumerate()
            .filter_map(|(i, &n)| (n == 0).then_some(i))
            .collect();
        let mut visited = 0;
        while let Some(index) = ready.pop_front() {
            visited += 1;
            for &parent in &self.parents[index] {
                children[parent] -= 1;
                if children[parent] == 0 {
                    ready.push_back(parent);
                }
            }
        }
        if visited != self.ids.len() {
            return Err(HistoryError::Cycle);
        }
        Ok(())
    }

    fn reachable(&self, root: usize) -> Vec<bool> {
        let mut marks = vec![false; self.ids.len()];
        marks[root] = true;
        let mut pending = vec![root];
        while let Some(index) = pending.pop() {
            for &parent in &self.parents[index] {
                if !marks[parent] {
                    marks[parent] = true;
                    pending.push(parent);
                }
            }
        }
        marks
    }
}

/// A complete ancestry operation failed; no partial answer is returned.
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    /// A root or reachable parent does not exist.
    #[error("missing commit {0}")]
    Missing(ObjectId),
    /// An object exists but is not a commit; tags are not peeled implicitly.
    #[error("object {0} is not a commit")]
    NotCommit(ObjectId),
    /// Storage verification or a per-read resource bound failed.
    #[error("reading commit {id}: {source}")]
    Read {
        /// Object being read.
        id: ObjectId,
        /// Underlying storage failure.
        #[source]
        source: ObjectReadError,
    },
    /// Verified bytes do not have supported commit syntax.
    #[error("parsing commit {id}: {source}")]
    Parse {
        /// Object being parsed.
        id: ObjectId,
        /// Underlying syntax failure.
        #[source]
        source: CommitError,
    },
    /// A graph bound was exhausted.
    #[error("history limit exceeded: {0}")]
    Limit(&'static str),
    /// Parent edges contain a cycle.
    #[error("cyclic commit ancestry")]
    Cycle,
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn graph(parents: Vec<Vec<usize>>) -> Graph {
        let ids: Vec<_> = (0..parents.len())
            .map(|i| ObjectId::for_blob(&i.to_le_bytes()))
            .collect();
        let positions = ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        Graph {
            ids,
            positions,
            parents,
        }
    }

    #[rstest]
    #[case::self_cycle(vec![vec![0]])]
    #[case::two_node_cycle(vec![vec![1],vec![0]])]
    fn rejects_cycles(#[case] parents: Vec<Vec<usize>>) {
        assert!(matches!(
            graph(parents).check_acyclic(),
            Err(HistoryError::Cycle)
        ));
    }

    #[test]
    fn duplicate_edges_are_acyclic() {
        assert!(graph(vec![vec![1, 1], vec![]]).check_acyclic().is_ok());
    }

    #[test]
    fn deep_history_uses_no_recursion() {
        let mut parents: Vec<_> = (1..100_000).map(|i| vec![i]).collect();
        parents.push(vec![]);
        let graph = graph(parents);
        assert!(graph.check_acyclic().is_ok());
        assert!(graph.reachable(0).into_iter().all(|marked| marked));
    }

    #[test]
    fn discovery_enforces_limit_before_queue_growth() {
        let mut graph = graph(vec![]);
        let id = ObjectId::for_blob(b"root");
        assert!(matches!(
            graph.discover(id, 0),
            Err(HistoryError::Limit("commits"))
        ));
        assert!(graph.ids.is_empty());
    }
}
