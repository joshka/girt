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
        graph.history = graph.prepare_history()?;
        self.commit_graph = Some(Arc::new(graph));
        Ok(())
    }
}

impl CommitGraph {
    fn prepare_history(&self) -> Result<HistoryIndex, GraphError> {
        let lookup = ParentLookup::new(&self.commits);
        let mut corrected = true;
        let mut topological = true;
        let mut parent_starts = Vec::with_capacity(self.commits.len() + 1);
        let mut parents = Vec::new();
        parent_starts.push(0);
        for commit in &self.commits {
            corrected &= commit.corrected_commit_date.is_some();
            topological &= commit.generation != 0 && commit.generation < 0x3fffffff;
            for parent in &commit.parents {
                let position = lookup.get(*parent).ok_or(bad("missing indexed parent"))?;
                let parent = &self.commits[position];
                corrected &= match (parent.corrected_commit_date, commit.corrected_commit_date) {
                    (Some(parent), Some(child)) => parent < child,
                    _ => false,
                };
                topological &= parent.generation < commit.generation;
                parents.push(u32::try_from(position).map_err(|_| bad("commit count"))?);
            }
            parent_starts.push(u32::try_from(parents.len()).map_err(|_| bad("parent count"))?);
        }
        // A strictly decreasing value on every edge also proves acyclic ancestry.
        let (order, generations) = if corrected {
            let dates = self
                .commits
                .iter()
                .map(|c| c.corrected_commit_date.unwrap_or(0));
            (GenerationOrder::CorrectedDates, dates.collect())
        } else if topological {
            let levels = self.commits.iter().map(|c| u64::from(c.generation));
            (GenerationOrder::Topological, levels.collect())
        } else {
            self.check_ancestry(&lookup)?;
            return Ok(HistoryIndex::default());
        };
        Ok(HistoryIndex {
            order,
            generations,
            parent_starts,
            parents,
        })
    }

    fn check_ancestry(&self, positions: &ParentLookup<'_>) -> Result<(), GraphError> {
        let mut state = vec![0u8; self.commits.len()];
        let mut stack = Vec::new();
        for root in 0..self.commits.len() {
            if state[root] == 2 {
                continue;
            }
            stack.push((root, false));
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
                    let position = positions
                        .get(*parent)
                        .ok_or(bad("missing indexed parent"))?;
                    stack.push((position, false));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn generation(&self, id: crate::ObjectId) -> Option<u64> {
        let record = self.get(id)?;
        match self.history.order {
            GenerationOrder::Unavailable => None,
            GenerationOrder::Topological => Some(u64::from(record.generation)),
            GenerationOrder::CorrectedDates => record.corrected_commit_date,
        }
    }

    /// Finds a record's position for position-indexed walks, when a generation order is checked.
    pub(crate) fn position(&self, id: crate::ObjectId) -> Option<u32> {
        if matches!(self.history.order, GenerationOrder::Unavailable) {
            return None;
        }
        let position = self.commits.binary_search_by_key(&id, |c| c.id).ok()?;
        u32::try_from(position).ok()
    }

    /// Returns the number of record positions.
    pub(crate) fn positions(&self) -> usize {
        self.commits.len()
    }

    /// Returns the commit at a position from [`Self::position`] or [`Self::parent_positions`].
    pub(crate) fn id_at(&self, position: u32) -> crate::ObjectId {
        self.commits[position as usize].id
    }

    /// Returns the checked generation at a position from [`Self::position`].
    pub(crate) fn generation_at(&self, position: u32) -> u64 {
        self.history.generations[position as usize]
    }

    /// Returns the parent positions, in stored order, of a position from [`Self::position`].
    pub(crate) fn parent_positions(&self, position: u32) -> &[u32] {
        let position = position as usize;
        let starts = &self.history.parent_starts;
        &self.history.parents[starts[position] as usize..starts[position + 1] as usize]
    }
}

/// A locally checked strict ordering; exact generation equations remain object-verifier work.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum GenerationOrder {
    #[default]
    Unavailable,
    Topological,
    CorrectedDates,
}

/// Generations and parent positions indexed by record position, filled only for a checked order.
///
/// Walks index dense arrays with these positions instead of searching identities per edge.
#[derive(Clone, Debug, Default)]
pub(crate) struct HistoryIndex {
    order: GenerationOrder,
    generations: Vec<u64>,
    /// `parents[parent_starts[i]..parent_starts[i + 1]]` are the parents of position `i`.
    parent_starts: Vec<u32>,
    parents: Vec<u32>,
}

// Sorted cryptographic identities usually leave only one or two records per 16-bit bucket.
// Narrowing binary searches is bounded even for adversarial identities, without hashing or
// storing another copy of every object ID.
struct ParentLookup<'a> {
    commits: &'a [super::GraphCommit],
    offsets: Vec<usize>,
}

impl<'a> ParentLookup<'a> {
    fn new(commits: &'a [super::GraphCommit]) -> Self {
        let mut offsets = Vec::with_capacity(65537);
        let mut position = 0;
        for bucket in 0..=65536 {
            while position < commits.len() && prefix(commits[position].id) < bucket {
                position += 1;
            }
            offsets.push(position);
        }
        Self { commits, offsets }
    }

    fn get(&self, id: crate::ObjectId) -> Option<usize> {
        let bucket = prefix(id);
        let start = self.offsets[bucket];
        self.commits[start..self.offsets[bucket + 1]]
            .binary_search_by_key(&id, |commit| commit.id)
            .ok()
            .map(|position| start + position)
    }
}

fn prefix(id: crate::ObjectId) -> usize {
    let bytes = id.as_bytes();
    usize::from(u16::from_be_bytes([bytes[0], bytes[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ObjectFormat, ObjectId};

    #[test]
    fn parent_lookup_handles_empty_buckets_and_colliding_prefixes() {
        let ids: Vec<_> = ["000000", "000001", "123400", "123401", "ffffff"]
            .into_iter()
            .map(|start| {
                ObjectId::from_hex(ObjectFormat::Sha1, &format!("{start}{}", "0".repeat(34)))
                    .unwrap()
            })
            .collect();
        let commits: Vec<_> = ids
            .iter()
            .map(|id| super::super::GraphCommit {
                id: *id,
                tree: *id,
                parents: Vec::new(),
                commit_time: 0,
                generation: 0,
                corrected_commit_date: None,
                bloom: None,
            })
            .collect();
        let lookup = ParentLookup::new(&commits);
        for (position, id) in ids.iter().enumerate() {
            assert_eq!(lookup.get(*id), Some(position));
        }
        for start in ["000002", "123402", "800000", "fffffe"] {
            let id = ObjectId::from_hex(ObjectFormat::Sha1, &format!("{start}{}", "0".repeat(34)))
                .unwrap();
            assert_eq!(lookup.get(id), None);
        }
        assert_eq!(ParentLookup::new(&[]).get(ids[0]), None);
    }
}
