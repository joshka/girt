//! Shallow boundaries for `deepen`, `deepen-relative`, `deepen-since` and `deepen-not`.

use std::collections::VecDeque;
use std::collections::hash_map::Entry;
use std::sync::atomic::AtomicBool;

use super::ServeError;
use crate::packet::check_cancelled;
use crate::{IdMap, IdSet, ObjectId, Objects, ReadLimits};

/// How far the client asked to deepen its history.
#[derive(Debug, Default)]
pub(super) struct Deepen {
    /// `deepen <n>`: keep `n` commits along every path from the wants.
    pub depth: Option<u32>,
    /// `deepen-relative`: count `depth` from the client's current boundary instead.
    pub relative: bool,
    /// `deepen-since <seconds>`: keep commits made at or after this time.
    pub since: Option<i64>,
    /// `deepen-not <ref>` targets: leave out history reachable from these commits.
    pub not: Vec<ObjectId>,
}

impl Deepen {
    pub(super) fn requested(&self) -> bool {
        self.depth.is_some() || self.since.is_some() || !self.not.is_empty()
    }
}

/// The client's new history shape.
#[derive(Debug, Default)]
pub(super) struct Boundaries {
    /// New boundary commits, reported as `shallow` lines.
    pub shallow: Vec<ObjectId>,
    /// Former boundaries whose parents are now sent, reported as `unshallow` lines.
    pub unshallow: Vec<ObjectId>,
    /// Commits whose parents aren't sent.
    pub cut: IdSet,
    /// Parents of unshallowed commits, sent as if wanted.
    pub tips: Vec<ObjectId>,
}

/// A commit's parents and committer date, as the boundary walks need them.
struct Node {
    parents: Vec<ObjectId>,
    date: i64,
}

struct Reader<'a> {
    objects: &'a Objects,
    read: ReadLimits,
    cancel: &'a AtomicBool,
    nodes: IdMap<Node>,
    /// Commits still allowed to be read.
    remaining: usize,
    /// Parent occurrences still allowed to be followed.
    parents: usize,
}

impl Reader<'_> {
    fn node(&mut self, id: ObjectId) -> Result<&Node, ServeError> {
        check_cancelled(self.cancel)?;
        match self.nodes.entry(id) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                self.remaining = self
                    .remaining
                    .checked_sub(1)
                    .ok_or(ServeError::Limit("shallow commits"))?;
                let commit = self
                    .objects
                    .read_commit_with(id, self.read)
                    .map_err(|error| ServeError::Commit(Box::new(error)))?;
                let date = commit
                    .committer()
                    .ok()
                    .flatten()
                    .and_then(|person| person.date().ok())
                    .map_or(0, |date| date.seconds);
                self.parents = self
                    .parents
                    .checked_sub(commit.parents().len())
                    .ok_or(ServeError::Limit("shallow parents"))?;
                Ok(entry.insert(Node {
                    parents: commit.parents().to_vec(),
                    date,
                }))
            }
        }
    }
}

/// The depth Git clients send for `--unshallow`, which lifts every client boundary.
const INFINITE_DEPTH: u32 = i32::MAX as u32;

/// Computes the boundaries for `deepen`, given the commit wants and the client's current
/// boundary commits.
///
/// Reads at most `history.max_commits` commits and follows at most `history.max_parents` parent
/// occurrences. Cancellation is checked between commits; a single commit read can't be
/// interrupted.
pub(super) fn boundaries(
    objects: &Objects,
    wants: &[ObjectId],
    client_order: &[ObjectId],
    deepen: &Deepen,
    history: crate::HistoryLimits,
    cancel: &AtomicBool,
) -> Result<Boundaries, ServeError> {
    let client: IdSet = client_order.iter().copied().collect();
    let mut reader = Reader {
        objects,
        read: history.read,
        cancel,
        nodes: IdMap::default(),
        remaining: history.max_commits,
        parents: history.max_parents,
    };
    // `kept` holds every commit the client ends up with whose parents are cut.
    let mut kept = Vec::new();
    let mut lifted = Vec::new();
    match deepen.depth {
        Some(depth) => {
            // Relative deepening starts from the client boundaries the wants reach.
            let sources: Vec<(ObjectId, u32)> = if deepen.relative {
                let reached = reached_boundaries(&mut reader, wants, &client)?;
                client_order
                    .iter()
                    .filter(|id| reached.contains(id))
                    .map(|&id| (id, 0))
                    .collect()
            } else {
                wants.iter().map(|&id| (id, 1)).collect()
            };
            let mut seen = IdSet::default();
            let mut queue: VecDeque<(ObjectId, u32)> = VecDeque::new();
            for (id, level) in sources {
                if seen.insert(id) {
                    queue.push_back((id, level));
                }
            }
            // Like Git, a commit at the full depth is a boundary even without parents.
            while let Some((id, level)) = queue.pop_front() {
                if level >= depth {
                    kept.push(id);
                    continue;
                }
                let parents = reader.node(id)?.parents.clone();
                if client.contains(&id) {
                    lifted.push(id);
                }
                for parent in parents {
                    if seen.insert(parent) {
                        queue.push_back((parent, level + 1));
                    }
                }
            }
        }
        None => {
            let excluded = reachable(&mut reader, &deepen.not)?;
            let included = |reader: &mut Reader<'_>, id: ObjectId| -> Result<bool, ServeError> {
                if excluded.contains(&id) {
                    return Ok(false);
                }
                Ok(match deepen.since {
                    Some(since) => reader.node(id)?.date >= since,
                    None => true,
                })
            };
            let mut seen = IdSet::default();
            let mut pending: Vec<ObjectId> = Vec::new();
            for &id in wants {
                if included(&mut reader, id)? && seen.insert(id) {
                    pending.push(id);
                }
            }
            if pending.is_empty() {
                return Err(ServeError::Protocol(
                    "no commits selected for shallow requests",
                ));
            }
            while let Some(id) = pending.pop() {
                let parents = reader.node(id)?.parents.clone();
                let mut cut = false;
                for &parent in &parents {
                    if included(&mut reader, parent)? {
                        if seen.insert(parent) {
                            pending.push(parent);
                        }
                    } else {
                        cut = true;
                    }
                }
                if cut {
                    kept.push(id);
                } else if client.contains(&id) {
                    lifted.push(id);
                }
            }
        }
    }
    // Like Git, `--unshallow` lifts every client boundary, even those the wants don't reach.
    if deepen.depth == Some(INFINITE_DEPTH) {
        let lifted_set: IdSet = lifted.iter().copied().collect();
        for &id in client_order {
            if !lifted_set.contains(&id) {
                reader.node(id)?;
                lifted.push(id);
            }
        }
    }
    let mut boundaries = Boundaries::default();
    for id in kept {
        boundaries.cut.insert(id);
        if !client.contains(&id) {
            boundaries.shallow.push(id);
        }
    }
    for id in lifted {
        if let Some(node) = reader.nodes.get(&id) {
            boundaries.tips.extend(&node.parents);
        }
        boundaries.unshallow.push(id);
    }
    Ok(boundaries)
}

/// The client boundaries reachable from `wants`, walking no further than any boundary.
fn reached_boundaries(
    reader: &mut Reader<'_>,
    wants: &[ObjectId],
    client: &IdSet,
) -> Result<IdSet, ServeError> {
    let mut reached = IdSet::default();
    let mut seen: IdSet = wants.iter().copied().collect();
    let mut pending = wants.to_vec();
    while let Some(id) = pending.pop() {
        if client.contains(&id) {
            reached.insert(id);
            continue;
        }
        for parent in reader.node(id)?.parents.clone() {
            if seen.insert(parent) {
                pending.push(parent);
            }
        }
    }
    Ok(reached)
}

/// Every commit reachable from `roots`.
fn reachable(reader: &mut Reader<'_>, roots: &[ObjectId]) -> Result<IdSet, ServeError> {
    let mut seen: IdSet = roots.iter().copied().collect();
    let mut pending = roots.to_vec();
    while let Some(id) = pending.pop() {
        for parent in reader.node(id)?.parents.clone() {
            if seen.insert(parent) {
                pending.push(parent);
            }
        }
    }
    Ok(seen)
}
