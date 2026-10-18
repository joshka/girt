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
            return Ok(paint(self, graph, include, exclude, Sides::Include, limits)?.0);
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

/// How many commits each side of a symmetric range has that the other lacks.
///
/// Returned by [`Objects::ahead_behind`]. A merge commit counts once, like any other commit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct AheadBehind {
    /// Commits reachable from the left tip but not from the right tip.
    pub ahead: usize,
    /// Commits reachable from the right tip but not from the left tip.
    pub behind: usize,
}

impl Objects {
    /// Counts the commits only `left` reaches and the commits only `right` reaches.
    ///
    /// This is `git rev-list --left-right --count left...right`: with `left` a branch and `right`
    /// its upstream, [`AheadBehind::ahead`] is how many commits the branch has that the upstream
    /// lacks. Equal tips give zero on both sides; disconnected histories count every commit.
    ///
    /// With a commit-graph covering both tips, the walk visits commits in descending generation
    /// order and stops once every queued commit is reachable from both tips, so shared history
    /// below the merge base is not read. Without one, both complete ancestries are read. Both give
    /// exact counts; no timestamp heuristics are used.
    ///
    /// ```
    /// # use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};
    /// # let directory = tempfile::tempdir()?;
    /// # let repo = Repository::init(ObjectFormat::Sha1, directory.path().join("r.git"), InitKind::Bare)?;
    /// # let loose = repo.loose_objects();
    /// # let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
    /// # let person = Signature {
    /// #     name: b"A. Writer".to_vec(), email: b"writer@example.com".to_vec(),
    /// #     seconds: 1_700_000_000, offset_minutes: 0,
    /// # };
    /// # let commit = |parents, message: &[u8]| loose.write_commit(&Commit::new(CommitFields {
    /// #     tree, parents, author: person.clone(), committer: person.clone(),
    /// #     extra_headers: vec![], message: message.to_vec(),
    /// # })?);
    /// let base = commit(vec![], b"base\n")?;
    /// let local = commit(vec![base], b"local\n")?;
    /// let upstream = commit(vec![base], b"upstream 1\n")?;
    /// let upstream = commit(vec![upstream], b"upstream 2\n")?;
    ///
    /// let counts = repo.objects()?.ahead_behind(local, upstream)?;
    /// assert_eq!((counts.ahead, counts.behind), (1, 2));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::walk_range`].
    pub fn ahead_behind(
        &self,
        left: ObjectId,
        right: ObjectId,
    ) -> Result<AheadBehind, HistoryError> {
        self.ahead_behind_with(left, right, HistoryLimits::default())
    }

    /// Counts both sides of a symmetric range under explicit bounds.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::walk_range_with`]; bounds count commits explored on both
    /// sides.
    pub fn ahead_behind_with(
        &self,
        left: ObjectId,
        right: ObjectId,
        limits: HistoryLimits,
    ) -> Result<AheadBehind, HistoryError> {
        if let Some(graph) = self
            .commit_graph
            .as_ref()
            .filter(|graph| graph.generation(left).is_some() && graph.generation(right).is_some())
        {
            let (ahead, behind) = paint(self, graph, &[left], &[right], Sides::Both, limits)?;
            return Ok(AheadBehind {
                ahead: ahead.len(),
                behind: behind.len(),
            });
        }
        let graph = Graph::read(self, &[left, right], limits)?;
        let left = graph.reachable(graph.positions[&left]);
        let right = graph.reachable(graph.positions[&right]);
        let only = |a: &[bool], b: &[bool]| a.iter().zip(b).filter(|(a, b)| **a && !**b).count();
        Ok(AheadBehind {
            ahead: only(&left, &right),
            behind: only(&right, &left),
        })
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

/// Which single-color commits a paint collects and waits for.
#[derive(Clone, Copy, PartialEq)]
enum Sides {
    /// Commits reachable only from the include roots.
    Include,
    /// Commits reachable from only one side, either side.
    Both,
}

struct Paint {
    marks: IdMap<u8>,
    queue: BinaryHeap<(u64, ObjectId)>,
    sides: Sides,
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
        self.interesting += usize::from(self.counts(new));
        self.interesting -= usize::from(self.counts(old));
        self.marks.insert(id, new);
        Ok(())
    }

    /// Whether a commit with `color` is still interesting: one side reaches it, not both.
    fn counts(&self, color: u8) -> bool {
        match self.sides {
            Sides::Include => color == 1,
            Sides::Both => color == 1 || color == 2,
        }
    }
}

fn paint(
    objects: &Objects,
    graph: &crate::commit_graph::CommitGraph,
    include: &[ObjectId],
    exclude: &[ObjectId],
    sides: Sides,
    limits: HistoryLimits,
) -> Result<(Vec<ObjectId>, Vec<ObjectId>), HistoryError> {
    let mut state = Paint {
        marks: IdMap::default(),
        queue: BinaryHeap::new(),
        sides,
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
    let mut included = Vec::new();
    let mut excluded = Vec::new();
    while state.interesting != 0 {
        let Some((_, id)) = state.queue.pop() else {
            break;
        };
        // Every child of `id` has a higher generation and was popped first, so its color is final.
        let color = state.marks[&id];
        if state.counts(color) {
            state.interesting -= 1;
            if color == 1 {
                included.push(id)
            } else {
                excluded.push(id)
            }
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
    Ok((included, excluded))
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
