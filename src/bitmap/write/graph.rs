use std::ops::Range;
use std::sync::atomic::AtomicBool;

use super::{WriteError, WriteLimits, WriteOptions, check};
use crate::{Commit, EntryMode, Object, ObjectFormat, ObjectId, ObjectKind, ObjectReadError, Tag};

pub(super) struct Graph<'a> {
    pub ids: &'a [ObjectId],
    pub kinds: Vec<ObjectKind>,
    edges: Vec<Range<usize>>,
    targets: Vec<usize>,
    parents: Vec<bool>,
}
impl<'a> Graph<'a> {
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
                let target_position = ids
                    .binary_search(&target)
                    .map_err(|_| WriteError::Closure(target))?;
                if object.kind() == ObjectKind::Commit && kind == ObjectKind::Commit {
                    graph.parents[target_position] = true;
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
            if graph.kinds[target] != kind {
                return Err(super::super::Error::Kind(ids[target]).into());
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

    pub fn reachable(
        &self,
        root: usize,
        inverse: &[usize],
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<u64>, WriteError> {
        let mut bitmap = vec![0_u64; self.kinds.len().div_ceil(64)];
        let mut stack = vec![root];
        while let Some(index) = stack.pop() {
            check(cancel)?;
            charge(budget)?;
            let bit = inverse[index];
            let word = &mut bitmap[bit / 64];
            let mask = 1 << (bit % 64);
            if *word & mask != 0 {
                continue;
            }
            *word |= mask;
            for &target in &self.targets[self.edges[index].clone()] {
                check(cancel)?;
                charge(budget)?;
                stack.push(target);
            }
        }
        Ok(bitmap)
    }
}

fn sample(input: &[usize], count: usize) -> Vec<usize> {
    let count = count.min(input.len());
    (0..count)
        .map(|index| input[(index as u64 * input.len() as u64 / count as u64) as usize])
        .collect()
}
fn charge(budget: &mut usize) -> Result<(), WriteError> {
    *budget = budget
        .checked_sub(1)
        .ok_or(WriteError::Limit("reachability visits"))?;
    Ok(())
}
fn malformed(id: ObjectId, source: impl std::error::Error + Send + Sync + 'static) -> WriteError {
    super::super::Error::Object {
        id,
        source: Box::new(source),
    }
    .into()
}
