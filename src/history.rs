//! Bounded ancestry queries over verified loose and packed commit objects.
//!
//! Use [`Objects::walk`] to collect reachable commit IDs, [`Objects::is_ancestor`] to test
//! ancestry, and [`Objects::merge_bases`] to find every best common ancestor. Roots are explicit
//! commit IDs: resolve references and peel annotated tags first. Queries read graph records, not
//! trees or blobs.
//!
//! # Inspect a branch point
//!
//! ```
//! use girt::{
//!     Commit, CommitFields, HistoryLimits, InitKind, ObjectFormat, Repository, Signature, Tree,
//! };
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r.git"),
//!     InitKind::Bare,
//! )?;
//!
//! let loose = repo.loose_objects();
//! let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
//! let person = Signature {
//!     name: b"A. Writer".to_vec(),
//!     email: b"writer@example.com".to_vec(),
//!     seconds: 1_700_000_000,
//!     offset_minutes: 0,
//! };
//! let commit = |parents, message: &[u8]| {
//!     loose.write_commit(&Commit::new(CommitFields {
//!         tree,
//!         parents,
//!         author: person.clone(),
//!         committer: person.clone(),
//!         extra_headers: vec![],
//!         message: message.to_vec(),
//!     })?)
//! };
//!
//! let base = commit(vec![], b"base\n")?;
//! let left = commit(vec![base], b"left\n")?;
//! let right = commit(vec![base], b"right\n")?;
//!
//! let objects = repo.objects()?;
//! assert_eq!(objects.walk(&[left])?, [left, base]);
//! assert!(objects.is_ancestor(base, right)?);
//! assert_eq!(objects.merge_bases(left, right)?, [base]);
//!
//! // The `_with` forms take the bounds the short forms fill in.
//! let limits = HistoryLimits::default().with_max_commits(10);
//! assert_eq!(objects.walk_with(&[right], limits)?, [right, base]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Ordering and shallow history
//!
//! Walking is breadth-first in supplied root and stored parent order, with duplicates removed.
//! It is not timestamp or topological order. Merge bases may contain several IDs for criss-cross
//! history; callers must not silently choose one as the only base. Disconnected histories have
//! none.
//!
//! Shallow commits are included but their parent edges are not followed, even if parent objects
//! exist. The reader retains boundaries from its repository handle. After deepening, refresh
//! repository shallow metadata with [`crate::Repository::refresh_shallow`] and create a new reader.
//! Raw commit parsing still exposes stored parent IDs.
//!
//! # Work and failures
//!
//! [`HistoryLimits`] bounds distinct commits, parent occurrences, and each object read. Each query
//! uses [`HistoryLimits::default`] and has a `_with` form, such as [`Objects::walk_with`], that
//! takes explicit limits.
//! The current queries collect a complete graph in memory; `walk` is not a lazy iterator. Ancestry
//! tests validate both complete ancestries even for equal endpoints or an early match. Size budgets
//! therefore need to cover the graph, not just the distance between two commits.
//!
//! [`HistoryError`] distinguishes missing or wrong-kind commits, malformed payloads, cycles, I/O,
//! and exhausted bounds. Failure returns no partial result and changes no files. A limit failure
//! requires an explicit budget decision; a missing ancestor may mean incomplete storage. Declared
//! shallow boundaries are the supported way to stop traversal, not a blanket permission to ignore
//! missing parents. History parsing does not impose author/date construction rules.
mod path;
pub use path::PathHistoryError;
mod range;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{
    Commit, CommitError, IdMap, ObjectId, ObjectKind, ObjectReadError, Objects, ReadLimits,
};

/// Resource bounds for one complete history operation.
///
/// Bounds cover distinct commits (including queued roots/parents), parent occurrences (including
/// duplicates but excluding edges beyond shallow boundaries), and each object read. Memory is
/// O(commits + parents), plus one decoded commit and the reader's pack snapshot. These are input
/// bounds, not a total heap or wall-clock limit.
///
/// ```
/// use girt::{HistoryLimits, ReadLimits};
/// let limits = HistoryLimits::default()
///     .with_max_commits(10_000)
///     .with_max_parents(40_000)
///     .with_read(ReadLimits::default().with_max_object_bytes(1024 * 1024));
/// assert_eq!(limits.max_commits, 10_000);
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct HistoryLimits {
    /// Maximum distinct commits; zero permits only an empty walk (default 100,000).
    pub max_commits: usize,
    /// Maximum parent occurrences (default 1,000,000).
    pub max_parents: usize,
    /// Bounds applied separately to every object read.
    pub read: ReadLimits,
}

impl HistoryLimits {
    /// Sets [`Self::max_commits`].
    #[must_use]
    pub fn with_max_commits(mut self, max_commits: usize) -> Self {
        self.max_commits = max_commits;
        self
    }

    /// Sets [`Self::max_parents`].
    #[must_use]
    pub fn with_max_parents(mut self, max_parents: usize) -> Self {
        self.max_parents = max_parents;
        self
    }

    /// Sets [`Self::read`].
    #[must_use]
    pub fn with_read(mut self, read: ReadLimits) -> Self {
        self.read = read;
        self
    }
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

impl HistoryLimits {
    /// Returns generous finite bounds for caller-approved ancestry queries.
    ///
    /// Commit and parent bounds retain arithmetic headroom; object reads use
    /// [`ReadLimits::trusted`]. A complete ancestry is collected in memory, so these values can
    /// permit work beyond available resources. Prefer selected default-bound overrides when the
    /// application needs predictable graph size or decoding cost.
    pub const fn trusted() -> Self {
        Self {
            max_commits: usize::MAX / 4,
            max_parents: usize::MAX / 4,
            read: ReadLimits::trusted(),
        }
    }
}

impl Objects {
    /// Collects all commits reachable from explicit roots, including the roots themselves.
    ///
    /// Order is breadth-first: roots in supplied order, then parents in stored order. First
    /// discovery wins; duplicate roots and parents never duplicate output. This is **not
    /// topological order**: an ancestor supplied as a root can precede its descendant. Timestamps
    /// are ignored. Declared shallow roots are read and parsed but their parent edges are not
    /// followed, even when parent objects exist. Raw commit reads retain every parent.
    /// An empty root slice returns an empty vector without reading storage.
    ///
    /// ```
    /// use girt::{
    ///     Commit, CommitFields, HistoryLimits, InitKind, ObjectFormat, PackLimits, Repository,
    ///     Signature, Tree,
    /// };
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repository = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("repo.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let loose = repository.loose_objects();
    /// let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
    /// let person = Signature {
    ///     name: b"A. Writer".to_vec(),
    ///     email: b"writer@example.com".to_vec(),
    ///     seconds: 1_700_000_000,
    ///     offset_minutes: 0,
    /// };
    /// let commit = |parents, message: &[u8]| {
    ///     loose.write_commit(&Commit::new(CommitFields {
    ///         tree,
    ///         parents,
    ///         author: person.clone(),
    ///         committer: person.clone(),
    ///         extra_headers: vec![],
    ///         message: message.to_vec(),
    ///     })?)
    /// };
    ///
    /// let first = commit(vec![], b"first\n")?;
    /// let second = commit(vec![first], b"second\n")?;
    ///
    /// let objects = repository.objects_with(PackLimits::default())?;
    /// assert_eq!(
    ///     objects.walk_with(&[second], HistoryLimits::default())?,
    ///     [second, first]
    /// );
    ///
    /// // Supplied root order wins over ancestry: the walk is not topological.
    /// let limits = HistoryLimits::default();
    /// assert_eq!(
    ///     objects.walk_with(&[first, second], limits)?,
    ///     [first, second]
    /// );
    /// assert!(objects.is_ancestor_with(first, second, HistoryLimits::default())?);
    /// assert!(!objects.is_ancestor_with(second, first, HistoryLimits::default())?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError`] on any reachable missing, non-commit, malformed or corrupt object,
    /// cycle, or exhausted bound. No partial result is returned and no files are changed. Commit
    /// syntax uses [`Commit::parse`] without additional field validation; trees are not read.
    pub fn walk_with(
        &self,
        roots: &[ObjectId],
        limits: HistoryLimits,
    ) -> Result<Vec<ObjectId>, HistoryError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "history.walk",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || Ok(Graph::read(self, roots, limits)?.ids);
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::history);

        result
    }

    /// Tests whether `ancestor` is reachable from `descendant`, including equality.
    ///
    /// # Errors
    ///
    /// Without an attached commit-graph, validates both complete ancestries before answering.
    /// With explicitly trusted indexed metadata and checked generation order, stops at the target
    /// generation or an early match. Returns the same failures as [`Self::walk`] for explored data.
    ///
    /// ```
    /// # use girt::{Commit, CommitFields, HistoryLimits, InitKind, ObjectFormat, PackLimits,
    /// #            Repository, Signature, Tree};
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
    /// # let base = commit(vec![], b"base\n")?;
    /// # let left = commit(vec![base], b"left\n")?;
    /// # let right = commit(vec![base], b"right\n")?;
    /// # let objects = repo.objects_with(PackLimits::default())?;
    /// assert!(objects.is_ancestor_with(base, left, HistoryLimits::default())?);
    /// assert!(objects.is_ancestor_with(left, left, HistoryLimits::default())?);
    /// assert!(!objects.is_ancestor_with(left, right, HistoryLimits::default())?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn is_ancestor_with(
        &self,
        ancestor: ObjectId,
        descendant: ObjectId,
        limits: HistoryLimits,
    ) -> Result<bool, HistoryError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "history.is_ancestor",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            if let Some(answer) = self.indexed_ancestor(ancestor, descendant, limits) {
                return answer;
            }
            let graph = Graph::read(self, &[descendant, ancestor], limits)?;
            let marks = graph.reachable(0);
            Ok(marks[graph.positions[&ancestor]])
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::history);

        result
    }

    /// Fetch stops after verifying the reached ancestor, without following its parents.
    /// Byte-bound receive evidence and fallback reads both verify identity. Budgets cover
    /// discovered commits and followed edges; cycles in the explored graph fail. No timestamp
    /// assumptions.
    pub(crate) fn is_ancestor_verified(
        &self,
        ancestor: ObjectId,
        descendant: ObjectId,
        limits: HistoryLimits,
        cancel: &AtomicBool,
        parents: impl FnMut(ObjectId, ReadLimits) -> Result<Option<Vec<ObjectId>>, HistoryError>,
    ) -> Result<bool, HistoryError> {
        let graph =
            Graph::read_until(self, &[descendant], limits, parents, Some(ancestor), cancel)?;
        Ok(graph.positions.contains_key(&ancestor))
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
    ///
    /// ```
    /// # use girt::{Commit, CommitFields, HistoryLimits, InitKind, ObjectFormat, PackLimits,
    /// #            Repository, Signature, Tree};
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
    /// # let base = commit(vec![], b"base\n")?;
    /// # let left = commit(vec![base], b"left\n")?;
    /// # let right = commit(vec![base], b"right\n")?;
    /// # let objects = repo.objects_with(PackLimits::default())?;
    /// assert_eq!(objects.merge_bases_with(left, right, HistoryLimits::default())?, [base]);
    /// assert_eq!(objects.merge_bases_with(left, left, HistoryLimits::default())?, [left]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn merge_bases_with(
        &self,
        left: ObjectId,
        right: ObjectId,
        limits: HistoryLimits,
    ) -> Result<Vec<ObjectId>, HistoryError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "history.merge_bases",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
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
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::history);

        result
    }

    /// Collects the commits reachable from `roots` with [`HistoryLimits::default`].
    ///
    /// Equivalent to `self.walk_with(roots, HistoryLimits::default())`; see [`Self::walk_with`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::walk_with`].
    pub fn walk(&self, roots: &[ObjectId]) -> Result<Vec<ObjectId>, HistoryError> {
        self.walk_with(roots, HistoryLimits::default())
    }

    /// Tests ancestry with [`HistoryLimits::default`].
    ///
    /// Equivalent to `self.is_ancestor_with(ancestor, descendant, HistoryLimits::default())`; see
    /// [`Self::is_ancestor_with`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::is_ancestor_with`].
    pub fn is_ancestor(
        &self,
        ancestor: ObjectId,
        descendant: ObjectId,
    ) -> Result<bool, HistoryError> {
        self.is_ancestor_with(ancestor, descendant, HistoryLimits::default())
    }

    /// Finds every best common ancestor with [`HistoryLimits::default`].
    ///
    /// Equivalent to `self.merge_bases_with(left, right, HistoryLimits::default())`; see
    /// [`Self::merge_bases_with`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::merge_bases_with`].
    pub fn merge_bases(
        &self,
        left: ObjectId,
        right: ObjectId,
    ) -> Result<Vec<ObjectId>, HistoryError> {
        self.merge_bases_with(left, right, HistoryLimits::default())
    }
}

struct Graph {
    ids: Vec<ObjectId>,
    positions: IdMap<usize>,
    parents: Vec<Vec<usize>>,
}

impl Graph {
    fn read(
        objects: &Objects,
        roots: &[ObjectId],
        limits: HistoryLimits,
    ) -> Result<Self, HistoryError> {
        Self::read_with(objects, roots, limits, |id, _| {
            Ok(objects
                .commit_graph
                .as_ref()
                .and_then(|graph| graph.get(id))
                .map(|commit| commit.parents.clone()))
        })
    }

    fn read_with(
        objects: &Objects,
        roots: &[ObjectId],
        limits: HistoryLimits,
        verified: impl FnMut(ObjectId, ReadLimits) -> Result<Option<Vec<ObjectId>>, HistoryError>,
    ) -> Result<Self, HistoryError> {
        Self::read_until(
            objects,
            roots,
            limits,
            verified,
            None,
            &AtomicBool::new(false),
        )
    }

    fn read_until(
        objects: &Objects,
        roots: &[ObjectId],
        limits: HistoryLimits,
        mut verified: impl FnMut(ObjectId, ReadLimits) -> Result<Option<Vec<ObjectId>>, HistoryError>,
        stop: Option<ObjectId>,
        cancel: &AtomicBool,
    ) -> Result<Self, HistoryError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "history.graph",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
            roots = roots.len(),
            commits = tracing::field::Empty,
        );

        let operation = || {
            let mut graph = Self {
                ids: vec![],
                positions: IdMap::default(),
                parents: vec![],
            };
            for &root in roots {
                graph.discover(root, limits.max_commits)?;
            }
            let mut remaining = limits.max_parents;
            let mut index = 0;
            while index < graph.ids.len() {
                let id = graph.ids[index];
                check_cancelled(cancel, id)?;
                let parents = match verified(id, limits.read)? {
                    Some(parents) => parents,
                    None => {
                        let object = objects
                            .read_controlled(id, limits.read, cancel)
                            .map_err(|source| HistoryError::Read { id, source })?
                            .ok_or(HistoryError::Missing(id))?;
                        if object.kind() != ObjectKind::Commit {
                            return Err(HistoryError::NotCommit(id));
                        }
                        let commit = Commit::parse(object.object_format(), object.data())
                            .map_err(|source| HistoryError::Parse { id, source })?;
                        commit.parents().to_vec()
                    }
                };
                if stop == Some(id) {
                    // Queued commits have no explored outgoing edges. The matching commit was
                    // identity-checked and parsed above; its parents are outside this query.
                    graph.parents.resize_with(graph.ids.len(), Vec::new);
                    break;
                }
                let parents = if objects.shallow_roots().contains(id) {
                    &[]
                } else {
                    parents.as_slice()
                };
                remaining = remaining
                    .checked_sub(parents.len())
                    .ok_or(HistoryError::Limit("parent occurrences"))?;
                let mut edges = Vec::with_capacity(parents.len());
                for &parent in parents {
                    check_cancelled(cancel, id)?;
                    edges.push(graph.discover(parent, limits.max_commits)?);
                }
                graph.parents.push(edges);
                index += 1;
            }
            graph.check_acyclic_controlled(cancel)?;
            Ok(graph)
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::history);
        #[cfg(feature = "tracing")]
        if let Ok(graph) = &result {
            span.record("commits", graph.ids.len());
        }
        result
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

    #[cfg(test)]
    fn check_acyclic(&self) -> Result<(), HistoryError> {
        self.check_acyclic_controlled(&AtomicBool::new(false))
    }

    fn check_acyclic_controlled(&self, cancel: &AtomicBool) -> Result<(), HistoryError> {
        let mut children = vec![0usize; self.ids.len()];
        for (index, parents) in self.parents.iter().enumerate() {
            check_cancelled(cancel, self.ids[index])?;
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
            check_cancelled(cancel, self.ids[index])?;
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

fn check_cancelled(cancel: &AtomicBool, id: ObjectId) -> Result<(), HistoryError> {
    if cancel.load(Ordering::Relaxed) {
        Err(HistoryError::Read {
            id,
            source: ObjectReadError::Cancelled,
        })
    } else {
        Ok(())
    }
}

/// A complete ancestry operation failed; no partial answer is returned.
///
/// [`Self::Missing`] can indicate incomplete storage; fetch the required history or use a
/// repository with correctly declared shallow roots. [`Self::NotCommit`] requires resolving the
/// input to a commit, possibly by peeling a tag. [`Self::Limit`] calls for an explicit budget
/// decision. Parsing and storage failures retain the offending ID and source. No variant authorizes
/// ignoring an edge and treating an incomplete traversal as a complete answer.
///
/// ```
/// use girt::{
///     HistoryError, HistoryLimits, InitKind, ObjectFormat, ObjectId, PackLimits,
///     Repository,
/// };
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r.git"),
///     InitKind::Bare,
/// )?;
///
/// let missing = ObjectId::Sha1([1; 20]);
/// let error = repo
///     .objects_with(PackLimits::default())?
///     .walk_with(&[missing], HistoryLimits::default())
///     .expect_err("missing root");
/// assert!(matches!(error, HistoryError::Missing(id) if id == missing));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HistoryError {
    /// A root or reachable parent does not exist.
    /// Obtain missing history or correct declared shallow boundaries; do not drop an edge from a
    /// supposedly complete answer.
    #[error("missing commit {0}")]
    Missing(ObjectId),
    /// An object exists but is not a commit; tags are not peeled implicitly.
    /// Resolve or peel the input to a verified commit before querying ancestry.
    #[error("object {0} is not a commit")]
    NotCommit(ObjectId),
    /// Storage verification or a per-read resource bound failed.
    /// Inspect the identified object and storage source; budget refusal and corruption need
    /// different responses.
    #[error("reading commit {id}")]
    Read {
        /// Object being read.
        id: ObjectId,
        /// Underlying storage failure.
        #[source]
        source: ObjectReadError,
    },
    /// Verified bytes do not have supported commit syntax.
    /// Report the commit ID and parser cause; graph traversal cannot invent missing graph records.
    #[error("parsing commit {id}")]
    Parse {
        /// Object being parsed.
        id: ObjectId,
        /// Underlying syntax failure.
        #[source]
        source: CommitError,
    },
    /// A graph bound was exhausted.
    /// Choose a justified graph budget covering complete ancestries, not only expected endpoint
    /// distance.
    #[error("history limit exceeded: {0}")]
    Limit(&'static str),
    /// Parent edges contain a cycle.
    /// Report the cyclic graph; a larger budget cannot supply a valid ancestry ordering.
    #[error("cyclic commit ancestry")]
    Cycle,
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn graph(parents: Vec<Vec<usize>>) -> Graph {
        let ids: Vec<_> = (0..parents.len())
            .map(|i| ObjectId::for_blob(crate::ObjectFormat::Sha1, &i.to_le_bytes()))
            .collect();
        let positions = ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        Graph {
            ids,
            positions,
            parents,
        }
    }

    fn verified_query(
        parents: Vec<Vec<usize>>,
        old: usize,
        new: usize,
        limits: HistoryLimits,
    ) -> Result<bool, HistoryError> {
        let graph = graph(parents);
        let root = tempfile::tempdir().unwrap();
        let repo = crate::Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("repo"),
            crate::InitKind::Bare,
        )
        .unwrap();
        let objects = repo.objects_with(crate::PackLimits::default()).unwrap();
        objects.is_ancestor_verified(
            graph.ids[old],
            graph.ids[new],
            limits,
            &AtomicBool::new(false),
            |id, _| {
                Ok(Some(
                    graph.parents[graph.positions[&id]]
                        .iter()
                        .map(|&i| graph.ids[i])
                        .collect(),
                ))
            },
        )
    }

    fn stored_history() -> (tempfile::TempDir, crate::Repository, Vec<ObjectId>) {
        let root = tempfile::tempdir().unwrap();
        let repo = crate::Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("repo"),
            crate::InitKind::Bare,
        )
        .unwrap();
        let loose = repo.loose_objects();
        let tree = loose
            .write_tree(&crate::Tree::new(crate::ObjectFormat::Sha1, vec![]).unwrap())
            .unwrap();
        let signature = crate::Signature {
            name: b"History".to_vec(),
            email: b"history@example.test".to_vec(),
            seconds: 1700000000,
            offset_minutes: 0,
        };
        let mut ids = vec![];
        for i in 0..3 {
            let commit = Commit::new(crate::CommitFields {
                tree,
                parents: ids.last().copied().into_iter().collect(),
                author: signature.clone(),
                committer: signature.clone(),
                extra_headers: vec![],
                message: format!("node {i}").into_bytes(),
            })
            .unwrap();
            ids.push(loose.write_commit(&commit).unwrap());
        }
        (root, repo, ids)
    }

    fn loose_path(repo: &crate::Repository, id: ObjectId) -> std::path::PathBuf {
        let hex = id.to_string();
        repo.git_dir()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..])
    }

    #[test]
    fn verified_query_hashes_the_matching_commit() {
        let (_root, repo, ids) = stored_history();
        std::fs::copy(loose_path(&repo, ids[0]), loose_path(&repo, ids[1])).unwrap();
        let objects = repo.objects_with(crate::PackLimits::default()).unwrap();
        assert!(
            matches!(objects.is_ancestor_verified(ids[1], ids[2], HistoryLimits::default(), &AtomicBool::new(false), |_, _| Ok(None)), Err(HistoryError::Read { id, .. }) if id == ids[1])
        );
    }

    #[test]
    fn verified_query_respects_git_shallow_boundary() {
        let (_root, repo, ids) = stored_history();
        std::fs::write(repo.git_dir().join("shallow"), format!("{}\n", ids[1])).unwrap();
        let repo = crate::Repository::open(repo.git_dir()).unwrap();
        let objects = repo.objects_with(crate::PackLimits::default()).unwrap();
        let oracle = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.git_dir())
            .args([
                "merge-base",
                "--is-ancestor",
                &ids[0].to_string(),
                &ids[2].to_string(),
            ])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert_eq!(oracle.code(), Some(1));
        assert!(
            !objects
                .is_ancestor_verified(
                    ids[0],
                    ids[2],
                    HistoryLimits::default(),
                    &AtomicBool::new(false),
                    |_, _| Ok(None)
                )
                .unwrap()
        );
        assert!(
            objects
                .is_ancestor_verified(
                    ids[1],
                    ids[2],
                    HistoryLimits::default(),
                    &AtomicBool::new(false),
                    |_, _| Ok(None)
                )
                .unwrap()
        );
    }

    #[test]
    fn cancellation_during_verified_walk_prevents_an_answer() {
        let (_root, repo, ids) = stored_history();
        let objects = repo.objects_with(crate::PackLimits::default()).unwrap();
        let cancel = AtomicBool::new(false);
        let result = objects.is_ancestor_verified(
            ids[0],
            ids[2],
            HistoryLimits::default(),
            &cancel,
            |_, _| {
                cancel.store(true, Ordering::Relaxed);
                Ok(None)
            },
        );
        assert!(matches!(
            result,
            Err(HistoryError::Read {
                source: ObjectReadError::Cancelled,
                ..
            })
        ));
    }

    #[rstest]
    #[case::commits(1, 10, "commits")]
    #[case::parents(10, 0, "parent occurrences")]
    fn verified_query_keeps_graph_budgets(
        #[case] max_commits: usize,
        #[case] max_parents: usize,
        #[case] expected: &str,
    ) {
        assert!(
            matches!(verified_query(vec![vec![], vec![0]], 0, 1, HistoryLimits { max_commits, max_parents, ..Default::default() }), Err(HistoryError::Limit(name)) if name == expected)
        );
    }

    #[test]
    fn verified_query_keeps_per_read_budget() {
        let (_root, repo, ids) = stored_history();
        let objects = repo.objects_with(crate::PackLimits::default()).unwrap();
        let mut limits = HistoryLimits::default();
        limits.read.max_object_bytes = 1;
        assert!(matches!(
            objects.is_ancestor_verified(
                ids[1],
                ids[2],
                limits,
                &AtomicBool::new(false),
                |_, _| Ok(None)
            ),
            Err(HistoryError::Read { .. })
        ));
    }

    #[test]
    fn early_match_does_not_hide_an_explored_cycle() {
        assert!(matches!(
            verified_query(vec![vec![0, 1], vec![]], 1, 0, HistoryLimits::default()),
            Err(HistoryError::Cycle)
        ));
    }

    #[test]
    fn verified_query_does_not_read_beyond_matching_commit() {
        let (_root, repo, ids) = stored_history();
        std::fs::remove_file(loose_path(&repo, ids[0])).unwrap();
        let objects = repo.objects_with(crate::PackLimits::default()).unwrap();
        assert!(
            objects
                .is_ancestor_verified(
                    ids[1],
                    ids[2],
                    HistoryLimits::default(),
                    &AtomicBool::new(false),
                    |_, _| Ok(None)
                )
                .unwrap()
        );
    }

    #[test]
    fn verified_match_stops_before_old_parents_are_queued() {
        assert!(
            verified_query(
                vec![vec![], vec![0], vec![1]],
                1,
                2,
                HistoryLimits {
                    max_commits: 2,
                    max_parents: 1,
                    ..Default::default()
                }
            )
            .unwrap()
        );
    }

    #[rstest]
    #[case::unrelated(vec![vec![], vec![0], vec![]], 1, 2, false)]
    #[case::rewind(vec![vec![], vec![0]], 1, 0, false)]
    #[case::merge_second_parent(vec![vec![], vec![0], vec![1], vec![2], vec![0], vec![3,4]], 4, 5, true)]
    #[case::duplicate_edges(vec![vec![], vec![0,0]], 0, 1, true)]
    #[case::equal(vec![vec![], vec![0]], 1, 1, true)]
    fn verified_reachability(
        #[case] parents: Vec<Vec<usize>>,
        #[case] old: usize,
        #[case] new: usize,
        #[case] expected: bool,
    ) {
        assert_eq!(
            verified_query(parents, old, new, HistoryLimits::default()).unwrap(),
            expected
        );
    }

    #[rstest]
    #[case::self_cycle(vec![vec![0], vec![]])]
    #[case::two_node_cycle(vec![vec![1], vec![0], vec![]])]
    fn verified_search_rejects_explored_cycles(#[case] parents: Vec<Vec<usize>>) {
        let old = parents.len() - 1;
        assert!(matches!(
            verified_query(parents, old, 0, HistoryLimits::default()),
            Err(HistoryError::Cycle)
        ));
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
        let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"root");
        assert!(matches!(
            graph.discover(id, 0),
            Err(HistoryError::Limit("commits"))
        ));
        assert!(graph.ids.is_empty());
    }
}

#[cfg(doctest)]
#[doc = include_str!("../site/src/content/docs/guides/objects-and-history.md")]
mod website_objects_guide {}
