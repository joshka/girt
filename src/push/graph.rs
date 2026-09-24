use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;

use super::{ForcePolicy, PushCommand, PushFailure as Error, PushLimits};
use crate::packet::check_cancelled;
use crate::{Object, ObjectId, ObjectKind, Objects};

/// Own selected objects once; preserve parent edges for per-command ancestry proofs.
pub(super) struct Graph {
    pub objects: HashMap<ObjectId, Object>,
    parents: HashMap<ObjectId, Vec<ObjectId>>,
}
impl Graph {
    pub fn select(
        store: &Objects,
        commands: &[PushCommand],
        limits: PushLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let mut graph = Self {
            objects: HashMap::new(),
            parents: HashMap::new(),
        };
        let mut pending = VecDeque::new();
        let mut expected = HashMap::new();
        for command in commands {
            let kind = command
                .name
                .as_bytes()
                .starts_with(b"refs/heads/")
                .then_some(ObjectKind::Commit);
            enqueue(command.new, kind, &mut expected, &mut pending, limits)?;
        }
        let mut bytes = limits.pack.max_input_bytes;
        let mut edges = limits.max_edges;
        while let Some(id) = pending.pop_front() {
            check_cancelled(cancel)?;
            let mut read = limits.read;
            read.max_object_bytes = read.max_object_bytes.min(
                usize::try_from(bytes.min(limits.pack.max_object_bytes)).unwrap_or(usize::MAX),
            );
            let object = store
                .read(id, read)
                .map_err(|source| Error::Read { id, source })?
                .ok_or(Error::Missing(id))?;
            if expected[&id].is_some_and(|kind| kind != object.kind()) {
                return Err(Error::Kind(id));
            }
            bytes = bytes
                .checked_sub(object.data().len() as u64)
                .ok_or(Error::Limit("selected bytes"))?;
            let mut parents = Vec::new();
            let edge = |target, kind| {
                if object.kind() == ObjectKind::Commit && kind == ObjectKind::Commit {
                    parents.push(target);
                }
                check_cancelled(cancel)?;
                edges = edges
                    .checked_sub(1)
                    .ok_or(Error::Limit("reachable edges"))?;
                // An unconstrained root may already have been read before a typed edge reaches it.
                if graph.objects.get(&target).is_some_and(|o| o.kind() != kind) {
                    return Err(Error::Kind(target));
                }
                enqueue(target, Some(kind), &mut expected, &mut pending, limits)
            };
            crate::edges::visit(id, &object, edge)?;
            if object.kind() == ObjectKind::Commit {
                graph.parents.insert(id, parents);
            }
            if expected[&id].is_some_and(|kind| kind != object.kind()) {
                return Err(Error::Kind(id));
            }
            graph.objects.insert(id, object);
        }
        let mut remaining = limits.max_ancestry_steps;
        for command in commands {
            check_cancelled(cancel)?;
            if command.force == ForcePolicy::Allow {
                continue;
            }
            if let Some(old) = command.expected {
                if old == command.new {
                    continue;
                }
                if !command.name.as_bytes().starts_with(b"refs/heads/")
                    || !graph.is_ancestor(old, command.new, &mut remaining, cancel)?
                {
                    return Err(Error::WouldForce(command.name.clone()));
                }
            }
        }
        Ok(graph)
    }

    /// Only roots inside the validated selected graph can prove a complete exclusion closure.
    /// Unavailable/disconnected roots are ignored rather than read from arbitrary local history.
    pub fn exclude(
        &mut self,
        roots: &[ObjectId],
        limits: PushLimits,
        cancel: &AtomicBool,
    ) -> Result<Vec<ObjectId>, Error> {
        if roots.len() > limits.max_refs {
            return Err(Error::Limit("receiver roots"));
        }
        let mut proven = Vec::new();
        let mut seen = HashSet::new();
        let mut pending = VecDeque::new();
        for &id in roots {
            check_cancelled(cancel)?;
            if self.objects.contains_key(&id) && seen.insert(id) {
                proven.push(id);
                pending.push_back(id);
            }
        }
        let mut edges = limits.max_edges;
        while let Some(id) = pending.pop_front() {
            check_cancelled(cancel)?;
            let object = &self.objects[&id];
            let edge = |target, _kind| {
                check_cancelled(cancel)?;
                edges = edges
                    .checked_sub(1)
                    .ok_or(Error::Limit("exclusion edges"))?;
                if seen.insert(target) {
                    pending.push_back(target);
                }
                Ok::<_, Error>(())
            };
            crate::edges::visit(id, object, edge)?;
        }
        for id in seen {
            check_cancelled(cancel)?;
            self.objects.remove(&id);
        }
        Ok(proven)
    }

    fn is_ancestor(
        &self,
        old: ObjectId,
        new: ObjectId,
        remaining: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<bool, Error> {
        let mut pending = vec![new];
        let mut seen = HashSet::from([new]);
        while let Some(id) = pending.pop() {
            check_cancelled(cancel)?;
            *remaining = remaining
                .checked_sub(1)
                .ok_or(Error::Limit("ancestry steps"))?;
            if id == old {
                return Ok(true);
            }
            for &parent in &self.parents[&id] {
                check_cancelled(cancel)?;
                *remaining = remaining
                    .checked_sub(1)
                    .ok_or(Error::Limit("ancestry steps"))?;
                if seen.insert(parent) {
                    pending.push(parent);
                }
            }
        }
        Ok(false)
    }
}

fn enqueue(
    id: ObjectId,
    kind: Option<ObjectKind>,
    expected: &mut HashMap<ObjectId, Option<ObjectKind>>,
    pending: &mut VecDeque<ObjectId>,
    limits: PushLimits,
) -> Result<(), Error> {
    if let Some(previous) = expected.get_mut(&id) {
        if previous.is_some() && kind.is_some() && *previous != kind {
            return Err(Error::Kind(id));
        }
        if kind.is_some() {
            *previous = kind;
        }
    } else {
        if expected.len() as u64 >= u64::from(limits.pack.max_objects) {
            return Err(Error::Limit("selected objects"));
        }
        expected.insert(id, kind);
        pending.push_back(id);
    }
    Ok(())
}
