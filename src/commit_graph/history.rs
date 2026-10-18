//! Attachment policy and generation ordering used by bounded history queries.
use std::sync::Arc;

use super::{CommitGraph, GraphError, bad};
use crate::Objects;

impl Objects {
    /// Attaches structurally checked auxiliary metadata for subsequent history queries.
    ///
    /// This explicitly trusts the index's object claims without decoding the named commits. Use
    /// [`CommitGraph::verify`] first when object verification is required. Missing index records
    /// use ordinary verified reads. Commit/parent query bounds and shallow boundaries still apply;
    /// per-object byte limits apply only to fallback reads. Typed object reads always use objects.
    /// Clones share the immutable index; storage refresh retains this index until replaced.
    ///
    /// # Errors
    ///
    /// Rejects a different hash format, missing parent records or cyclic indexed ancestry.
    pub fn with_commit_graph(mut self, graph: CommitGraph) -> Result<Self, GraphError> {
        self.attach_commit_graph(graph)?;
        Ok(self)
    }

    /// Removes the auxiliary index so history queries read commit objects directly.
    ///
    /// Useful when object claims must be checked during each query rather than trusted from an
    /// index. This does not alter repository configuration or other readers.
    pub fn without_commit_graph(mut self) -> Self {
        self.commit_graph = None;
        self
    }

    pub(crate) fn attach_commit_graph(&mut self, mut graph: CommitGraph) -> Result<(), GraphError> {
        if graph.format != self.object_format() {
            return Err(GraphError::HashFormat);
        }
        graph.check_ancestry()?;
        graph.generation_order = graph.generation_order();
        self.commit_graph = Some(Arc::new(graph));
        Ok(())
    }
}

impl CommitGraph {
    fn check_ancestry(&self) -> Result<(), GraphError> {
        let mut state = vec![0u8; self.commits.len()];
        for root in 0..self.commits.len() {
            let mut stack = vec![(root, false)];
            while let Some((index, exiting)) = stack.pop() {
                if exiting {
                    state[index] = 2;
                    continue;
                }
                if state[index] == 2 {
                    continue;
                }
                if state[index] == 1 {
                    return Err(bad("cyclic indexed ancestry"));
                }
                state[index] = 1;
                stack.push((index, true));
                for parent in self.commits[index].parents.iter().rev() {
                    let position = self
                        .commits
                        .binary_search_by_key(parent, |c| c.id)
                        .map_err(|_| bad("missing indexed parent"))?;
                    stack.push((position, false));
                }
            }
        }
        Ok(())
    }

    fn generation_order(&self) -> GenerationOrder {
        if self.commits.iter().all(|commit| {
            commit.corrected_commit_date.is_some_and(|date| {
                commit.parents.iter().all(|parent| {
                    self.get(*parent)
                        .and_then(|c| c.corrected_commit_date)
                        .is_some_and(|parent| parent < date)
                })
            })
        }) {
            return GenerationOrder::CorrectedDates;
        }
        if self.commits.iter().all(|commit| {
            commit.generation != 0
                && commit.generation < 0x3fffffff
                && commit.parents.iter().all(|parent| {
                    self.get(*parent)
                        .is_some_and(|parent| parent.generation < commit.generation)
                })
        }) {
            return GenerationOrder::Topological;
        }
        GenerationOrder::Unavailable
    }

    pub(crate) fn generation(&self, id: crate::ObjectId) -> Option<u64> {
        let record = self.get(id)?;
        match self.generation_order {
            GenerationOrder::Unavailable => None,
            GenerationOrder::Topological => Some(u64::from(record.generation)),
            GenerationOrder::CorrectedDates => record.corrected_commit_date,
        }
    }
}

/// A locally checked strict ordering; exact generation equations remain object-verifier work.
#[derive(Clone, Copy, Debug)]
pub(crate) enum GenerationOrder {
    Unavailable,
    Topological,
    CorrectedDates,
}
