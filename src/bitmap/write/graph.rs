use std::ops::Range;
use std::sync::atomic::AtomicBool;

use super::{WriteError, WriteLimits, WriteOptions, check};
use crate::{Commit, EntryMode, Object, ObjectFormat, ObjectId, ObjectKind, ObjectReadError, Tag};

pub(super) struct Graph<'a> {
    pub ids: &'a [ObjectId],
    pub kinds: Vec<ObjectKind>,
    edges: Vec<Range<usize>>,
    targets: Vec<u32>,
    parents: Vec<bool>,
}
impl<'a> Graph<'a> {
    pub fn from_verified(
        ids: &'a [ObjectId],
        order: &[usize],
        verified: &crate::retention::VerifiedGraph,
        limits: WriteLimits,
        cancel: &AtomicBool,
        mut read: impl FnMut(usize) -> Result<Object, ObjectReadError>,
    ) -> Result<Self, WriteError> {
        if ids.len() > limits.max_objects || order.len() != ids.len() {
            return Err(WriteError::Limit("objects"));
        }
        let verified_positions: crate::PeerIdMap<_> = verified
            .nodes
            .iter()
            .enumerate()
            .map(|(position, node)| (node.id, position))
            .collect();
        let mut output_to_verified = Vec::with_capacity(ids.len());
        let mut verified_to_output = vec![None; verified.nodes.len()];
        for (position, &id) in ids.iter().enumerate() {
            check(cancel)?;
            let verified_position = verified_positions
                .get(&id)
                .copied()
                .ok_or(WriteError::Closure(id))?;
            output_to_verified.push(verified_position);
            verified_to_output[verified_position] =
                Some(u32::try_from(position).map_err(|_| WriteError::Limit("objects"))?);
        }
        let mut graph = Self {
            ids,
            kinds: vec![ObjectKind::Blob; ids.len()],
            edges: vec![0..0; ids.len()],
            targets: Vec::new(),
            parents: vec![false; ids.len()],
        };
        let mut input_bytes = 0_u64;
        for &position in order {
            check(cancel)?;
            let id = ids[position];
            let object = read(position).map_err(|error| match error {
                ObjectReadError::Cancelled => WriteError::Cancelled,
                error => WriteError::Read(error),
            })?;
            input_bytes = input_bytes
                .checked_add(object.data().len() as u64)
                .filter(|&bytes| bytes <= limits.max_input_bytes)
                .ok_or(WriteError::Limit("graph input bytes"))?;
            let verified_position = output_to_verified[position];
            let node = &verified.nodes[verified_position];
            if object.kind() != node.kind {
                return Err(super::super::Error::Kind(id).into());
            }
            graph.kinds[position] = object.kind();
            let first = graph.targets.len();
            let targets = verified
                .targets
                .get(node.edges.clone())
                .ok_or(WriteError::Limit("verified graph edge"))?;
            for &target in targets {
                check(cancel)?;
                if graph.targets.len() >= limits.max_edges {
                    return Err(WriteError::Limit("graph edges"));
                }
                let target_node = verified
                    .nodes
                    .get(target as usize)
                    .ok_or(WriteError::Limit("verified graph edge"))?;
                let target_position = verified_to_output
                    .get(target as usize)
                    .copied()
                    .flatten()
                    .ok_or(WriteError::Closure(target_node.id))?;
                if object.kind() == ObjectKind::Commit && target_node.kind == ObjectKind::Commit {
                    graph.parents[target_position as usize] = true;
                }
                graph.targets.push(target_position);
            }
            graph.edges[position] = first..graph.targets.len();
        }
        Ok(graph)
    }

    pub fn read(
        format: ObjectFormat,
        ids: &'a [ObjectId],
        order: &[usize],
        limits: WriteLimits,
        cancel: &AtomicBool,
        mut read: impl FnMut(usize) -> Result<Object, ObjectReadError>,
    ) -> Result<Self, WriteError> {
        // Callers supply the validated pack order or an explicitly constructed MIDX permutation.
        debug_assert_eq!(order.len(), ids.len());
        let mut graph = Self {
            ids,
            kinds: vec![ObjectKind::Blob; ids.len()],
            edges: vec![0..0; ids.len()],
            targets: Vec::new(),
            parents: vec![false; ids.len()],
        };
        // Inventory IDs are claims until all reads finish. Use the collision-resistant map
        // even though successful construction independently verifies every claimed identity.
        let positions: crate::PeerIdMap<_> = ids
            .iter()
            .enumerate()
            .map(|(position, &id)| (id, position))
            .collect();
        let mut expected = Vec::new();
        let mut input = 0_u64;
        // Read offset order so delta chains reuse recently decoded bases. All graph positions
        // remain in object-ID order, independently of this physical traversal order.
        for &position in order {
            let id = ids[position];
            check(cancel)?;
            let object = read(position).map_err(|error| match error {
                ObjectReadError::Cancelled => WriteError::Cancelled,
                error => WriteError::Read(error),
            })?;
            input = input
                .checked_add(object.data().len() as u64)
                .filter(|&bytes| bytes <= limits.max_input_bytes)
                .ok_or(WriteError::Limit("graph input bytes"))?;
            graph.kinds[position] = object.kind();
            let first = graph.targets.len();
            let mut push = |target: ObjectId, kind| -> Result<(), WriteError> {
                check(cancel)?;
                if graph.targets.len() >= limits.max_edges {
                    return Err(WriteError::Limit("graph edges"));
                }
                let target_position = positions
                    .get(&target)
                    .copied()
                    .ok_or(WriteError::Closure(target))?;
                let target_position =
                    u32::try_from(target_position).map_err(|_| WriteError::Limit("objects"))?;
                if object.kind() == ObjectKind::Commit && kind == ObjectKind::Commit {
                    graph.parents[target_position as usize] = true;
                }
                graph.targets.push(target_position);
                expected.push(kind);
                Ok(())
            };
            match object.kind() {
                ObjectKind::Blob => {}
                ObjectKind::Commit => {
                    let commit = Commit::parse(format, object.data())
                        .map_err(|error| malformed(id, error))?;
                    push(commit.tree(), ObjectKind::Tree)?;
                    for &parent in commit.parents() {
                        push(parent, ObjectKind::Commit)?;
                    }
                }
                ObjectKind::Tree => {
                    for entry in crate::tree::RawEntries::new(format, object.data()) {
                        let (mode, _, target) = entry.map_err(|error| malformed(id, error))?;
                        if mode != EntryMode::Gitlink {
                            push(target, mode.object_kind())?;
                        }
                    }
                }
                ObjectKind::Tag => {
                    let tag =
                        Tag::parse(format, object.data()).map_err(|error| malformed(id, error))?;
                    push(tag.target(), tag.target_kind())?;
                }
            }
            graph.edges[position] = first..graph.targets.len();
        }
        for (&target, &kind) in graph.targets.iter().zip(&expected) {
            check(cancel)?;
            if graph.kinds[target as usize] != kind {
                return Err(super::super::Error::Kind(ids[target as usize]).into());
            }
        }
        Ok(graph)
    }

    pub fn select(
        &self,
        options: &WriteOptions,
        limits: WriteLimits,
        cancel: &AtomicBool,
    ) -> Result<Vec<usize>, WriteError> {
        let ids = self.ids;
        if options.commits.len() > limits.max_visits {
            return Err(WriteError::Limit("selection input"));
        }
        if !options.commits.is_empty() {
            let mut selected = Vec::new();
            for &id in &options.commits {
                check(cancel)?;
                let index = ids.binary_search(&id).map_err(|_| WriteError::Commit(id))?;
                if self.kinds[index] != ObjectKind::Commit {
                    return Err(WriteError::Commit(id));
                }
                selected.push(index);
            }
            selected.sort_unstable();
            selected.dedup();
            if selected.len() > options.max_commits {
                return Err(WriteError::Limit("selected commits"));
            }
            return Ok(selected);
        }
        let mut tips = Vec::new();
        let mut others = Vec::new();
        for (index, &kind) in self.kinds.iter().enumerate() {
            check(cancel)?;
            if kind == ObjectKind::Commit {
                if self.parents[index] {
                    others.push(index);
                } else {
                    tips.push(index);
                }
            }
        }
        let mut selected = sample(&tips, options.max_commits);
        selected.extend(sample(
            &others,
            options.max_commits.saturating_sub(selected.len()),
        ));
        selected.sort_unstable();
        Ok(selected)
    }

    pub fn targets(&self, position: usize) -> &[u32] {
        &self.targets[self.edges[position].clone()]
    }
}

fn sample(input: &[usize], count: usize) -> Vec<usize> {
    let count = count.min(input.len());
    (0..count)
        .map(|index| input[(index as u64 * input.len() as u64 / count as u64) as usize])
        .collect()
}
fn malformed(id: ObjectId, source: impl std::error::Error + Send + Sync + 'static) -> WriteError {
    super::super::Error::Object {
        id,
        source: Box::new(source),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitmap::write::reachable::Closures;

    #[test]
    fn retained_closures_obey_word_capacity_and_charge_multiword_unions() {
        let ids: Vec<_> = (0..70)
            .map(|number| ObjectFormat::Sha1.hash_object(ObjectKind::Commit, &[number]))
            .collect();
        let mut edges = vec![0..0; ids.len()];
        edges[0] = 0..1;
        edges[1] = 1..2;
        edges[69] = 2..3;
        let graph = Graph {
            ids: &ids,
            kinds: vec![ObjectKind::Commit; ids.len()],
            edges,
            targets: vec![1, 2, 0],
            parents: vec![false; ids.len()],
        };
        let inverse: Vec<_> = (0..ids.len()).rev().collect();
        let cancel = AtomicBool::new(false);
        let mut limits = WriteLimits {
            max_visits: 10,
            max_bytes: 16, // Exactly one two-word cached closure.
            ..WriteLimits::default()
        };
        let mut closures = Closures::new(&graph, &inverse, limits);
        assert_eq!(closures.reachable(0, &cancel).unwrap(), vec![0, 56]);
        assert_eq!(closures.reachable(69, &cancel).unwrap(), vec![1, 56]);
        for (bytes, visits) in [(8, 10), (16, 9)] {
            limits.max_bytes = bytes;
            limits.max_visits = visits;
            let mut closures = Closures::new(&graph, &inverse, limits);
            closures.reachable(0, &cancel).unwrap();
            assert!(matches!(
                closures.reachable(69, &cancel),
                Err(WriteError::Limit("reachability visits"))
            ));
        }
    }
}
