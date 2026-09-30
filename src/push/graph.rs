//! Push object selection: objects reachable from pushed tips but not from receiver roots.
//!
//! Selection walks commits from the tips and the receiver roots together in committer-date order,
//! painting the roots' ancestry as known to the receiver. The walk stops once every queued commit
//! is known, so its cost follows the new history rather than the whole repository. Trees of known
//! commits adjacent to sent commits are then marked known before the sent commits' trees are
//! walked.
//!
//! Soundness never depends on dates: an object is omitted only when a mark propagated from a
//! locally present root through parsed edges reaches it, so it lies in that root's closure. Dates
//! only order the walk. Skewed dates can make the walk read a commit before discovering that the
//! receiver has it; such a commit is still omitted when the mark arrives before the walk stops,
//! and is otherwise sent redundantly.

use std::cmp::Reverse;
use std::collections::hash_map::Entry;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;

use super::{ForcePolicy, PushCommand, PushFailure as Error, PushLimits};
use crate::packet::check_cancelled;
use crate::{Commit, Object, ObjectId, ObjectKind, Objects, ReadLimits};

/// Known commits still painted after the date bound says no sent commit can be reached.
///
/// The bound is exact for histories whose commits are never older than their parents. A few extra
/// steps absorb small clock skew, which otherwise sends commits the receiver already has.
const SKEW_SLOP: usize = 5;

/// Objects to send, and the receiver roots that omitted objects rely on.
pub(super) struct Selection {
    pub objects: HashMap<ObjectId, Object>,
    pub receiver_roots: Vec<ObjectId>,
}

/// Selects objects reachable from the non-deleting commands' tips that are not proven to be in the
/// closure of a locally present receiver root, then proves branch fast-forwards.
///
/// `observe` receives the cumulative number of objects read from storage.
pub(super) fn select(
    store: &Objects,
    commands: &[PushCommand],
    roots: &[ObjectId],
    limits: PushLimits,
    cancel: &AtomicBool,
    observe: impl FnMut(u64),
) -> Result<Selection, Error> {
    if roots.len() > limits.max_refs {
        return Err(Error::Limit("receiver roots"));
    }
    let mut walk = Walk::new(store, limits, cancel, observe);
    walk.mark_roots(roots)?;
    walk.resolve_tips(commands)?;
    walk.walk_commits()?;
    walk.check_fast_forwards(commands)?;
    walk.mark_boundary_trees()?;
    walk.select_trees()?;
    Ok(walk.finish())
}

/// Parsed commit graph fields shared by the selection walk and fast-forward proofs.
struct CommitNode {
    tree: ObjectId,
    parents: Vec<ObjectId>,
    date: i64,
}

/// An object in the closure of `root`, an index into the proven roots.
#[derive(Clone, Copy)]
struct Mark {
    kind: ObjectKind,
    root: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Visit {
    Queued,
    Done,
}

/// A date-ordered commit queue; ties pop in discovery order.
#[derive(Default)]
struct DateQueue {
    heap: BinaryHeap<(i64, Reverse<u64>, ObjectId)>,
    sequence: u64,
}
impl DateQueue {
    fn push(&mut self, date: i64, id: ObjectId) {
        self.heap.push((date, Reverse(self.sequence), id));
        self.sequence += 1;
    }
    fn peek_date(&self) -> Option<i64> {
        self.heap.peek().map(|&(date, ..)| date)
    }
    fn pop(&mut self) -> Option<ObjectId> {
        self.heap.pop().map(|(.., id)| id)
    }
}

struct Walk<'a, F> {
    store: &'a Objects,
    limits: PushLimits,
    cancel: &'a AtomicBool,
    observe: F,
    reads: u64,
    /// Locally present receiver roots, deduplicated in caller order, and whether an omitted
    /// object relies on each one.
    roots: Vec<(ObjectId, bool)>,
    known: HashMap<ObjectId, Mark>,
    commits: HashMap<ObjectId, CommitNode>,
    /// Objects read for sending; entries later found in `known` are dropped by `finish`.
    selected: HashMap<ObjectId, Object>,
    /// Kinds required by typed edges to discovered objects that may be sent.
    expected: HashMap<ObjectId, Option<ObjectKind>>,
    /// Non-commit objects from tips and tags, walked after tree marking.
    later: Vec<ObjectId>,
    visits: HashMap<ObjectId, Visit>,
    queue: DateQueue,
    /// Queued commits not yet known; the walk may stop when this reaches zero.
    unknown_queued: usize,
    /// Commits popped while not known, in walk order.
    processed: Vec<ObjectId>,
    oldest_processed: Option<i64>,
    bytes: u64,
    edges: usize,
    exclusion_edges: usize,
    ancestry_steps: usize,
}

impl<'a, F: FnMut(u64)> Walk<'a, F> {
    fn new(store: &'a Objects, limits: PushLimits, cancel: &'a AtomicBool, observe: F) -> Self {
        Self {
            store,
            limits,
            cancel,
            observe,
            reads: 0,
            roots: Vec::new(),
            known: HashMap::new(),
            commits: HashMap::new(),
            selected: HashMap::new(),
            expected: HashMap::new(),
            later: Vec::new(),
            visits: HashMap::new(),
            queue: DateQueue::default(),
            unknown_queued: 0,
            processed: Vec::new(),
            oldest_processed: None,
            bytes: limits.pack.max_input_bytes,
            edges: limits.max_edges,
            exclusion_edges: limits.max_edges,
            ancestry_steps: limits.max_ancestry_steps,
        }
    }

    fn read(&mut self, id: ObjectId, limits: ReadLimits) -> Result<Option<Object>, Error> {
        check_cancelled(self.cancel)?;
        // Storage synthesizes Git's canonical empty tree when it is not stored.
        let object = self
            .store
            .read(id, limits)
            .map_err(|source| Error::Read { id, source })?;
        if object.is_some() {
            self.reads += 1;
            (self.observe)(self.reads);
        }
        check_cancelled(self.cancel)?;
        Ok(object)
    }

    fn charge_edge(&mut self) -> Result<(), Error> {
        check_cancelled(self.cancel)?;
        self.edges = self
            .edges
            .checked_sub(1)
            .ok_or(Error::Limit("reachable edges"))?;
        Ok(())
    }

    fn charge_exclusion(&mut self) -> Result<(), Error> {
        check_cancelled(self.cancel)?;
        self.exclusion_edges = self
            .exclusion_edges
            .checked_sub(1)
            .ok_or(Error::Limit("exclusion edges"))?;
        Ok(())
    }

    /// Records that `id` is in the closure of root `root`; returns whether the mark is new.
    fn mark(&mut self, id: ObjectId, kind: ObjectKind, root: usize) -> Result<bool, Error> {
        if let Some(mark) = self.known.get(&id) {
            if mark.kind != kind {
                return Err(Error::Kind(id));
            }
            return Ok(false);
        }
        if self.selected.get(&id).is_some_and(|o| o.kind() != kind) {
            return Err(Error::Kind(id));
        }
        self.known.insert(id, Mark { kind, root });
        Ok(true)
    }

    /// Notes that an omitted object relies on its mark's root.
    fn rely_on(&mut self, id: ObjectId) -> bool {
        match self.known.get(&id) {
            Some(mark) => {
                self.roots[mark.root].1 = true;
                true
            }
            None => false,
        }
    }

    // Receiver roots.

    fn mark_roots(&mut self, roots: &[ObjectId]) -> Result<(), Error> {
        let mut seen = HashSet::new();
        for &id in roots {
            check_cancelled(self.cancel)?;
            if !seen.insert(id) {
                continue;
            }
            // Only local possession lets preparation follow a root's edges; others are ignored.
            let Some(object) = self.read(id, self.limits.read)? else {
                continue;
            };
            let index = self.roots.len();
            self.roots.push((id, false));
            self.mark_root_object(id, object, index)?;
        }
        Ok(())
    }

    /// Marks a root, peeling tags; commits join the walk and trees are marked recursively.
    fn mark_root_object(
        &mut self,
        mut id: ObjectId,
        mut object: Object,
        root: usize,
    ) -> Result<(), Error> {
        loop {
            let kind = object.kind();
            match kind {
                ObjectKind::Commit => {
                    if let Entry::Vacant(entry) = self.commits.entry(id) {
                        entry.insert(parse_commit(id, &object)?);
                    }
                    return self.mark_commit(id, root);
                }
                ObjectKind::Tree => {
                    if self.mark(id, kind, root)? {
                        self.mark_tree_closure(id, root)?;
                    }
                    return Ok(());
                }
                ObjectKind::Blob => return self.mark(id, kind, root).map(drop),
                ObjectKind::Tag => {
                    if !self.mark(id, kind, root)? {
                        return Ok(());
                    }
                    let [(target, target_kind)] = edges(id, &object)?[..] else {
                        unreachable!("a tag has one target");
                    };
                    self.charge_exclusion()?;
                    match self.read(target, self.limits.read)? {
                        Some(next) if next.kind() != target_kind => {
                            return Err(Error::Kind(target));
                        }
                        Some(next) => (id, object) = (target, next),
                        // The receiver's copy is trusted; local absence only stops marking.
                        None => return self.mark(target, target_kind, root).map(drop),
                    }
                }
            }
        }
    }

    /// Marks trees and blobs below an already marked tree. Absent subtrees stay unexpanded.
    fn mark_tree_closure(&mut self, tree: ObjectId, root: usize) -> Result<(), Error> {
        let mut pending = vec![tree];
        while let Some(id) = pending.pop() {
            let object = match self.selected.get(&id) {
                Some(object) => edges(id, object)?,
                None => match self.read(id, self.limits.read)? {
                    Some(object) if object.kind() != ObjectKind::Tree => {
                        return Err(Error::Kind(id));
                    }
                    Some(object) => edges(id, &object)?,
                    None => continue,
                },
            };
            for (target, kind) in object {
                self.charge_exclusion()?;
                if self.mark(target, kind, root)? && kind == ObjectKind::Tree {
                    pending.push(target);
                }
            }
        }
        Ok(())
    }

    // Tips.

    /// Records an interesting typed edge; returns whether the target is newly discovered.
    /// A known target is omitted, subject to its marked kind.
    fn discover(&mut self, id: ObjectId, kind: Option<ObjectKind>) -> Result<bool, Error> {
        if let Some(mark) = self.known.get(&id) {
            if kind.is_some_and(|kind| kind != mark.kind) {
                return Err(Error::Kind(id));
            }
            self.rely_on(id);
            return Ok(false);
        }
        // An unconstrained root may already have been read before a typed edge reaches it.
        if self
            .selected
            .get(&id)
            .is_some_and(|o| kind.is_some_and(|kind| kind != o.kind()))
        {
            return Err(Error::Kind(id));
        }
        if let Some(previous) = self.expected.get_mut(&id) {
            if previous.is_some() && kind.is_some() && *previous != kind {
                return Err(Error::Kind(id));
            }
            if kind.is_some() {
                *previous = kind;
            }
            return Ok(false);
        }
        if self.expected.len() as u64 >= u64::from(self.limits.pack.max_objects) {
            return Err(Error::Limit("selected objects"));
        }
        self.expected.insert(id, kind);
        Ok(true)
    }

    /// Reads a candidate for sending, charging the aggregate payload budget once.
    /// Returns `None` when the object is absent.
    fn read_candidate(&mut self, id: ObjectId) -> Result<Option<ObjectKind>, Error> {
        if let Some(object) = self.selected.get(&id) {
            return Ok(Some(object.kind()));
        }
        let mut read = self.limits.read;
        read.max_object_bytes = read.max_object_bytes.min(
            usize::try_from(self.bytes.min(self.limits.pack.max_object_bytes))
                .unwrap_or(usize::MAX),
        );
        let Some(object) = self.read(id, read)? else {
            return Ok(None);
        };
        if self.expected[&id].is_some_and(|kind| kind != object.kind()) {
            return Err(Error::Kind(id));
        }
        self.bytes = self
            .bytes
            .checked_sub(object.data().len() as u64)
            .ok_or(Error::Limit("selected bytes"))?;
        let kind = object.kind();
        self.selected.insert(id, object);
        Ok(Some(kind))
    }

    /// Reads tips and pushed tags. Commits join the walk; trees and blobs wait for tree marking.
    fn resolve_tips(&mut self, commands: &[PushCommand]) -> Result<(), Error> {
        let mut pending = VecDeque::new();
        for command in commands {
            check_cancelled(self.cancel)?;
            if command.deletes() {
                continue;
            }
            let kind = command
                .name
                .as_bytes()
                .starts_with(b"refs/heads/")
                .then_some(ObjectKind::Commit);
            if self.discover(command.new, kind)? {
                pending.push_back(command.new);
            }
        }
        while let Some(id) = pending.pop_front() {
            check_cancelled(self.cancel)?;
            if matches!(
                self.expected[&id],
                Some(ObjectKind::Tree | ObjectKind::Blob)
            ) {
                self.later.push(id);
                continue;
            }
            match self.read_candidate(id)?.ok_or(Error::Missing(id))? {
                ObjectKind::Commit => self.enqueue_candidate_commit(id)?,
                ObjectKind::Tag => {
                    for (target, kind) in edges(id, &self.selected[&id])? {
                        self.charge_edge()?;
                        if self.discover(target, Some(kind))? {
                            pending.push_back(target);
                        }
                    }
                }
                ObjectKind::Tree | ObjectKind::Blob => self.later.push(id),
            }
        }
        Ok(())
    }

    // Commit walk.

    /// Queues a discovered commit that is not known. Its object is kept for sending. An absent
    /// commit is queued ahead of every date so the walk reports it promptly, unless it is marked
    /// known before it pops.
    fn enqueue_candidate_commit(&mut self, id: ObjectId) -> Result<(), Error> {
        let date = match self.read_candidate(id)? {
            Some(ObjectKind::Commit) => {
                if !self.commits.contains_key(&id) {
                    let node = parse_commit(id, &self.selected[&id])?;
                    self.commits.insert(id, node);
                }
                self.commits[&id].date
            }
            Some(_) => return Err(Error::Kind(id)),
            None => i64::MAX,
        };
        self.visits.insert(id, Visit::Queued);
        self.unknown_queued += 1;
        self.queue.push(date, id);
        Ok(())
    }

    /// Reads a commit the receiver is known to have, without keeping its payload.
    /// Returns `None` when it is absent locally.
    fn load_known_commit(&mut self, id: ObjectId) -> Result<Option<i64>, Error> {
        if let Some(node) = self.commits.get(&id) {
            return Ok(Some(node.date));
        }
        let node = match self.selected.get(&id) {
            Some(object) => parse_commit(id, object)?,
            None => match self.read(id, self.limits.read)? {
                Some(object) if object.kind() != ObjectKind::Commit => {
                    return Err(Error::Kind(id));
                }
                Some(object) => parse_commit(id, &object)?,
                None => return Ok(None),
            },
        };
        let date = node.date;
        self.commits.insert(id, node);
        Ok(Some(date))
    }

    /// Marks a commit known and propagates the mark through already walked ancestry.
    fn mark_commit(&mut self, id: ObjectId, root: usize) -> Result<(), Error> {
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            check_cancelled(self.cancel)?;
            if !self.mark(id, ObjectKind::Commit, root)? {
                continue;
            }
            match self.visits.get(&id) {
                None => {
                    // Absent known commits have no edges to follow; queue them without a date.
                    let date = self.load_known_commit(id)?.unwrap_or(i64::MAX);
                    self.visits.insert(id, Visit::Queued);
                    self.queue.push(date, id);
                }
                Some(Visit::Queued) => self.unknown_queued -= 1,
                // Parents of a walked candidate were all discovered when it was popped.
                Some(Visit::Done) => pending.extend(&self.commits[&id].parents),
            }
        }
        Ok(())
    }

    /// Pops commits newest first until no queued commit is a candidate. Known commits dated at or
    /// after the oldest walked candidate are still painted, since without skew only they can
    /// reach one; then [`SKEW_SLOP`] older ones are. Like Git, a candidate with a very old date
    /// therefore makes the walk paint known history back to that date.
    fn walk_commits(&mut self) -> Result<(), Error> {
        let mut slop = SKEW_SLOP;
        while let Some(date) = self.queue.peek_date() {
            if self.unknown_queued == 0 {
                // Without skew, a known commit older than every walked candidate cannot reach one.
                let Some(oldest) = self.oldest_processed else {
                    break;
                };
                if date < oldest {
                    if slop == 0 {
                        break;
                    }
                    slop -= 1;
                }
            }
            let id = self.queue.pop().expect("peeked");
            check_cancelled(self.cancel)?;
            self.visits.insert(id, Visit::Done);
            if let Some(mark) = self.known.get(&id).copied() {
                let Some(node) = self.commits.get(&id) else {
                    continue;
                };
                for parent in node.parents.clone() {
                    self.charge_exclusion()?;
                    self.mark_commit(parent, mark.root)?;
                }
                continue;
            }
            self.unknown_queued -= 1;
            let parents = match self.commits.get(&id) {
                Some(node) => node.parents.clone(),
                None => return Err(Error::Missing(id)),
            };
            self.processed.push(id);
            self.oldest_processed = Some(self.oldest_processed.map_or(date, |d| d.min(date)));
            self.charge_edge()?;
            for parent in parents {
                self.charge_edge()?;
                if let Some(mark) = self.known.get(&parent) {
                    if mark.kind != ObjectKind::Commit {
                        return Err(Error::Kind(parent));
                    }
                } else if !self.visits.contains_key(&parent) {
                    // Every unvisited parent is queued; `discover` only applies kinds and limits.
                    self.discover(parent, Some(ObjectKind::Commit))?;
                    self.enqueue_candidate_commit(parent)?;
                }
            }
        }
        Ok(())
    }

    // Trees.

    /// Marks trees of known commits whose children will be sent, so shared content is omitted.
    fn mark_boundary_trees(&mut self) -> Result<(), Error> {
        let mut boundary = Vec::new();
        let mut seen = HashSet::new();
        for index in 0..self.processed.len() {
            let id = self.processed[index];
            if self.rely_on(id) {
                continue;
            }
            for &parent in &self.commits[&id].parents {
                if self.known.contains_key(&parent) && seen.insert(parent) {
                    boundary.push(parent);
                }
            }
        }
        for parent in boundary {
            check_cancelled(self.cancel)?;
            self.rely_on(parent);
            let root = self.known[&parent].root;
            let Some(node) = self.commits.get(&parent) else {
                continue;
            };
            let tree = node.tree;
            self.charge_exclusion()?;
            if self.mark(tree, ObjectKind::Tree, root)? {
                self.mark_tree_closure(tree, root)?;
            }
        }
        Ok(())
    }

    /// Walks trees of sent commits and tip trees/blobs, skipping known objects.
    fn select_trees(&mut self) -> Result<(), Error> {
        let mut pending: VecDeque<_> = std::mem::take(&mut self.later).into();
        for index in 0..self.processed.len() {
            let id = self.processed[index];
            if self.known.contains_key(&id) {
                continue;
            }
            let tree = self.commits[&id].tree;
            if self.discover(tree, Some(ObjectKind::Tree))? {
                pending.push_back(tree);
            }
        }
        while let Some(id) = pending.pop_front() {
            check_cancelled(self.cancel)?;
            if self.rely_on(id) {
                continue;
            }
            if self.read_candidate(id)?.ok_or(Error::Missing(id))? != ObjectKind::Tree {
                continue;
            }
            for (target, kind) in edges(id, &self.selected[&id])? {
                self.charge_edge()?;
                if self.discover(target, Some(kind))? {
                    pending.push_back(target);
                }
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Selection {
        let omitted: Vec<_> = self
            .selected
            .keys()
            .copied()
            .filter(|id| self.known.contains_key(id))
            .collect();
        for id in omitted {
            self.rely_on(id);
            self.selected.remove(&id);
        }
        Selection {
            objects: self.selected,
            receiver_roots: self
                .roots
                .into_iter()
                .filter_map(|(id, used)| used.then_some(id))
                .collect(),
        }
    }

    // Fast-forward proofs.

    fn check_fast_forwards(&mut self, commands: &[PushCommand]) -> Result<(), Error> {
        for command in commands {
            check_cancelled(self.cancel)?;
            if command.deletes() || command.force == ForcePolicy::Allow {
                continue;
            }
            let Some(old) = command.expected else {
                continue;
            };
            if old == command.new {
                continue;
            }
            let name = command.name.as_bytes();
            if name.starts_with(b"refs/heads/") && !self.is_ancestor(old, command.new)? {
                return Err(Error::WouldForce(command.name.clone()));
            }
            if name.starts_with(b"refs/tags/") {
                return Err(Error::WouldForce(command.name.clone()));
            }
        }
        Ok(())
    }

    fn ancestry_step(&mut self) -> Result<(), Error> {
        check_cancelled(self.cancel)?;
        self.ancestry_steps = self
            .ancestry_steps
            .checked_sub(1)
            .ok_or(Error::Limit("ancestry steps"))?;
        Ok(())
    }

    /// Exactly tests whether commit `old` is reachable from commit `new`, independent of dates.
    ///
    /// Paints `new`'s ancestry and `old`'s ancestry in date order. A path from `new` to `old`
    /// cannot pass through a strict ancestor of `old` (history is acyclic), so doubly painted
    /// commits other than `old` need not be followed for `new`. The answer is false once no
    /// commit painted only from `new` (or `old` itself painted from `new`) remains queued. Dates
    /// only decide how early `old`'s side prunes. An absent or non-commit `old` is never reached.
    fn is_ancestor(&mut self, old: ObjectId, new: ObjectId) -> Result<bool, Error> {
        const NEW: u8 = 1;
        const OLD: u8 = 2;
        let pending = |id: ObjectId, paint: u8| paint & NEW != 0 && (paint & OLD == 0 || id == old);
        let mut paints: HashMap<ObjectId, u8> = HashMap::new();
        let mut queued = HashSet::new();
        let mut queue = DateQueue::default();
        let mut open = 0usize;
        let new_date = self.ancestry_commit(new, true)?.expect("required");
        paints.insert(new, NEW);
        queued.insert(new);
        queue.push(new_date, new);
        open += 1;
        if let Some(date) = self.ancestry_commit(old, false)? {
            paints.insert(old, OLD);
            queued.insert(old);
            queue.push(date, old);
        }
        while open > 0 {
            let id = queue.pop().expect("open commits are queued");
            queued.remove(&id);
            let paint = paints[&id];
            if pending(id, paint) {
                open -= 1;
            }
            self.ancestry_step()?;
            if id == old && paint & NEW != 0 {
                return Ok(true);
            }
            let Some(node) = self.commits.get(&id) else {
                continue;
            };
            for parent in node.parents.clone() {
                self.ancestry_step()?;
                let before = paints.get(&parent).copied().unwrap_or(0);
                let after = before | paint;
                if after == before {
                    continue;
                }
                paints.insert(parent, after);
                if queued.contains(&parent) {
                    if pending(parent, before) && !pending(parent, after) {
                        open -= 1;
                    } else if !pending(parent, before) && pending(parent, after) {
                        open += 1;
                    }
                    continue;
                }
                // New paint must reach ancestors, including through an already popped commit.
                let required = pending(parent, after);
                let Some(date) = self.ancestry_commit(parent, required)? else {
                    continue;
                };
                queued.insert(parent);
                queue.push(date, parent);
                if required {
                    open += 1;
                }
            }
        }
        Ok(false)
    }

    /// Ensures a commit is parsed for an ancestry proof. `required` commits must exist; others
    /// that are absent or not commits are skipped.
    fn ancestry_commit(&mut self, id: ObjectId, required: bool) -> Result<Option<i64>, Error> {
        if let Some(node) = self.commits.get(&id) {
            return Ok(Some(node.date));
        }
        let read;
        let object = match self.selected.get(&id) {
            Some(object) => object,
            None => match self.read(id, self.limits.read)? {
                Some(object) => {
                    read = object;
                    &read
                }
                None if required => return Err(Error::Missing(id)),
                None => return Ok(None),
            },
        };
        if object.kind() != ObjectKind::Commit {
            return if required {
                Err(Error::Kind(id))
            } else {
                Ok(None)
            };
        }
        let node = parse_commit(id, object)?;
        let date = node.date;
        self.commits.insert(id, node);
        Ok(Some(date))
    }
}

/// Parses graph fields. An absent or uninterpretable committer date orders as the epoch.
fn parse_commit(id: ObjectId, object: &Object) -> Result<CommitNode, Error> {
    let commit = Commit::parse(object.object_format(), object.data())
        .map_err(|source| Error::Commit { id, source })?;
    let date = commit
        .committer()
        .ok()
        .flatten()
        .and_then(|person| person.date().ok())
        .map_or(0, |date| date.seconds);
    Ok(CommitNode {
        tree: commit.tree(),
        parents: commit.parents().to_vec(),
        date,
    })
}

/// Typed edges of a tree or tag in payload order, skipping gitlinks.
fn edges(id: ObjectId, object: &Object) -> Result<Vec<(ObjectId, ObjectKind)>, Error> {
    let mut edges = Vec::new();
    crate::edges::visit(id, object, |target, kind| {
        edges.push((target, kind));
        Ok::<_, Error>(())
    })?;
    Ok(edges)
}
