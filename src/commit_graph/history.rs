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
        graph.generation_order = graph.prepare_history()?;
        self.commit_graph = Some(Arc::new(graph));
        Ok(())
    }
}

impl CommitGraph {
    fn prepare_history(&self) -> Result<GenerationOrder, GraphError> {
        let positions = ParentLookup::new(&self.commits);
        let mut corrected = true;
        let mut topological = true;
        for commit in &self.commits {
            corrected &= commit.corrected_commit_date.is_some();
            topological &= commit.generation != 0 && commit.generation < 0x3fffffff;
            for parent in &commit.parents {
                let parent = positions
                    .get(*parent)
                    .map(|position| &self.commits[position])
                    .ok_or(bad("missing indexed parent"))?;
                corrected &= match (parent.corrected_commit_date, commit.corrected_commit_date) {
                    (Some(parent), Some(child)) => parent < child,
                    _ => false,
                };
                topological &= parent.generation < commit.generation;
            }
        }
        // A strictly decreasing value on every edge also proves acyclic ancestry.
        if corrected {
            return Ok(GenerationOrder::CorrectedDates);
        }
        if topological {
            return Ok(GenerationOrder::Topological);
        }
        self.check_ancestry(&positions)?;
        Ok(GenerationOrder::Unavailable)
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
