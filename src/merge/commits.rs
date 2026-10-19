use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

use super::memory::Memory;
use super::{RenameError, TreeError, TreeMerge, TreeMergeOptions};
use crate::{HistoryLimits, ObjectId, Objects, Tree};

/// Commit-history and recursive-base policy for [`Objects::merge_commits_with`].
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct CommitOptions {
    /// Rename-aware tree/content policy for each base and final merge.
    pub trees: TreeMergeOptions,
    /// Bounds for each ancestry query, independently applied.
    pub history: HistoryLimits,
    /// Permit unrelated tips by selecting an empty tree base (default false).
    pub allow_unrelated_histories: bool,
    /// Maximum recursive synthesis depth (default 32).
    pub max_base_depth: usize,
    /// Maximum number of tree merges used to synthesize bases (default 128).
    pub max_virtual_merges: usize,
    /// Maximum best-common-ancestor set size admitted at any step (default 64).
    pub max_bases: usize,
    /// Maximum ancestry queries across the entire composition (default 1,024).
    pub max_ancestry_queries: usize,
}

impl Default for CommitOptions {
    fn default() -> Self {
        Self {
            trees: TreeMergeOptions::default(),
            history: HistoryLimits::default(),
            allow_unrelated_histories: false,
            max_base_depth: 32,
            max_virtual_merges: 128,
            max_bases: 64,
            max_ancestry_queries: 1_024,
        }
    }
}

/// Commit-tip merge computation, retaining its ordered parents and base provenance.
///
/// Install all generated objects in [`Self::result`] before publishing its root identity.
/// A clean result can become a merge commit with the existing [`crate::Commit::builder`], using
/// [`Self::parents`] in order. No commit or reference is written by this computation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CommitMerge {
    /// Ours then theirs; a new merge commit should preserve this order.
    pub parents: [ObjectId; 2],
    /// All best common ancestors of the original tips, ordered by object identity.
    pub bases: Vec<ObjectId>,
    /// Original or synthesized root used as the final merge's base.
    pub base_tree: ObjectId,
    /// Candidate tree, conflicts and all generated base/normalization/content objects.
    pub result: TreeMerge,
}

/// Commit/base computation failed without publishing any generated object.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CommitError {
    /// An ancestry query could not complete within its bounds.
    #[error("finding merge ancestors")]
    History(#[from] crate::HistoryError),
    /// Reading a tip or base commit failed.
    #[error("reading merge commit")]
    Read(#[from] crate::ReadCommitError),
    /// Rename-aware tree computation failed.
    #[error("merging commit trees")]
    Trees(#[from] RenameError),
    /// Creating/reading an in-memory base failed.
    #[error("constructing virtual merge base")]
    Base(#[from] TreeError),
    /// Unrelated tips require explicit policy opt-in.
    #[error("merge tips have no common ancestor")]
    Unrelated,
    /// Recursive synthesis reached a configured bound.
    #[error("virtual merge base limit: {0}")]
    Limit(&'static str),
    /// A virtual base has unresolved conflicts; no provisional side is treated as resolved.
    #[error("virtual merge base contains unresolved conflicts")]
    ConflictedBase(Box<TreeMerge>),
    /// Caller cancelled between ancestry/tree operations.
    #[error("commit merge cancelled")]
    Cancelled,
    /// In a partial clone, blobs a base or final tree merge would read are not stored. Each tree
    /// merge lists every blob it needs before merging content, so a merge that synthesizes
    /// virtual bases may report missing blobs again after a fetch. Fetch them, for example with
    /// [`workflow::fetch_needed`](crate::workflow::fetch_needed), and merge again.
    #[error("merge needs objects the repository does not store")]
    MissingObjects(#[source] crate::MissingObjectsError),
}

/// Lifts missing objects out of a tree merge failure, so callers match one variant.
fn trees(error: RenameError) -> CommitError {
    match error {
        RenameError::MissingObjects(missing) => CommitError::MissingObjects(missing),
        error => CommitError::Trees(error),
    }
}

impl Objects {
    /// Merges commit tips using every best common ancestor and default bounds.
    ///
    /// See [`Self::merge_commits_with`] for recursive-base and publication contracts.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError`] on unrelated history, unreadable inputs, incomplete tree merging,
    /// unresolved virtual bases, exhausted bounds or cancellation.
    ///
    /// ```
    /// use girt::{Commit, InitKind, ObjectFormat, Repository, Signature, Tree};
    /// let temporary = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     temporary.path().join("repo"),
    ///     InitKind::Bare,
    /// )?;
    /// let loose = repo.loose_objects();
    /// let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
    /// let person = Signature {
    ///     name: b"Merge Author".to_vec(),
    ///     email: b"merge@example.com".to_vec(),
    ///     seconds: 1_700_000_000,
    ///     offset_minutes: 0,
    /// };
    /// let base = loose.write_commit(&Commit::builder(tree, person.clone(), b"base\n").build()?)?;
    /// let ours = loose.write_commit(
    ///     &Commit::builder(tree, person.clone(), b"ours\n")
    ///         .parent(base)
    ///         .build()?,
    /// )?;
    /// let theirs = loose.write_commit(
    ///     &Commit::builder(tree, person.clone(), b"theirs\n")
    ///         .parent(base)
    ///         .build()?,
    /// )?;
    /// let merged = repo.objects()?.merge_commits(ours, theirs)?;
    /// assert!(merged.result.is_clean());
    /// for (_, bytes) in &merged.result.blobs {
    ///     loose.write_blob(bytes)?;
    /// }
    /// for tree in &merged.result.trees {
    ///     loose.write_tree(tree)?;
    /// }
    /// let commit = Commit::builder(merged.result.tree.id(), person, b"merge\n")
    ///     .parents(merged.parents)
    ///     .build()?;
    /// let id = loose.write_commit(&commit)?;
    /// assert_eq!(repo.objects()?.read_commit(id)?.parents(), &[ours, theirs]);
    /// // The object is stored; no reference has moved.
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn merge_commits(
        &self,
        ours: ObjectId,
        theirs: ObjectId,
    ) -> Result<CommitMerge, CommitError> {
        self.merge_commits_with(
            ours,
            theirs,
            CommitOptions::default(),
            &AtomicBool::new(false),
        )
    }

    /// Computes a rename-aware commit merge without a working tree or object-store mutation.
    ///
    /// Finds all best common ancestors using the existing history traversal. Multiple bases are
    /// folded in object-ID order. Each fold recursively synthesizes the best common ancestors of
    /// the accumulated original base tips and the next tip. Generated trees and blobs remain in
    /// memory and are available to later folds, including rename scoring. A conflicted virtual
    /// base returns [`CommitError::ConflictedBase`] rather than choosing a provisional ancestor.
    ///
    /// Unrelated original tips require explicit opt-in. An empty base is always permitted inside
    /// recursive synthesis. Bounds apply independently to ancestry queries and tree comparisons;
    /// depth, base-set and virtual-merge limits bound composition. Individual history/storage
    /// reads and sorting are not interruptible; cancellation is checked between operations.
    /// No commit, index, working file or reference is written. Final conflicts remain a result.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError`] with original causes and unresolved virtual-base data. No partial
    /// result is installed, so retrying computation requires no repository rollback.
    pub fn merge_commits_with(
        &self,
        ours: ObjectId,
        theirs: ObjectId,
        options: CommitOptions,
        cancel: &AtomicBool,
    ) -> Result<CommitMerge, CommitError> {
        merge_with_driver(self, ours, theirs, options, cancel, None)
    }

    /// Merges commit tips using one path-driver instance for recursive bases and final content.
    ///
    /// Uses [`Self::merge_commits_with`]'s bounds, history policy and in-memory result contract.
    /// Callback side effects and attribute snapshot choice belong to the caller; no implicit
    /// command runs. See [`super::TreeDriver`] for attribute and custom-driver composition.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError`], retaining any driver failure's path and original source.
    pub fn merge_commits_with_driver(
        &self,
        ours: ObjectId,
        theirs: ObjectId,
        options: CommitOptions,
        cancel: &AtomicBool,
        driver: &mut dyn super::TreeDriver,
    ) -> Result<CommitMerge, CommitError> {
        merge_with_driver(self, ours, theirs, options, cancel, Some(driver))
    }
}

fn merge_with_driver<'a, 'd>(
    objects: &'a Objects,
    ours: ObjectId,
    theirs: ObjectId,
    options: CommitOptions,
    cancel: &'a AtomicBool,
    driver: Option<&'a mut (dyn super::TreeDriver + 'd)>,
) -> Result<CommitMerge, CommitError> {
    let mut merger = Bases {
        objects,
        options,
        cancel,
        memory: Memory::default(),
        driver,
        remaining: options.max_virtual_merges,
        queries: options.max_ancestry_queries,
    };
    merger.check()?;
    let mut bases = merger.query_bases(ours, theirs)?;
    bases.sort();
    if bases.is_empty() && !options.allow_unrelated_histories {
        return Err(CommitError::Unrelated);
    }
    let base_tree = merger.synthesize(&bases, 0)?;
    let a = objects.read_commit(ours)?.tree();
    let b = objects.read_commit(theirs)?.tree();
    merger.check()?;
    let mut result = super::renames::merge_with_memory_and_driver(
        objects,
        [base_tree, a, b],
        options.trees,
        cancel,
        &mut merger.memory,
        merger.driver.as_deref_mut(),
    )
    .map_err(trees)?;
    result.blobs.extend(merger.memory.blobs);
    Ok(CommitMerge {
        parents: [ours, theirs],
        bases,
        base_tree,
        result,
    })
}

struct Bases<'a, 'd> {
    objects: &'a Objects,
    options: CommitOptions,
    cancel: &'a AtomicBool,
    memory: Memory,
    driver: Option<&'a mut (dyn super::TreeDriver + 'd)>,
    remaining: usize,
    queries: usize,
}

impl Bases<'_, '_> {
    fn check(&self) -> Result<(), CommitError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(CommitError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn charge_query(&mut self) -> Result<(), CommitError> {
        self.check()?;
        self.queries = self
            .queries
            .checked_sub(1)
            .ok_or(CommitError::Limit("ancestry queries"))?;
        Ok(())
    }

    fn query_bases(
        &mut self,
        left: ObjectId,
        right: ObjectId,
    ) -> Result<Vec<ObjectId>, CommitError> {
        self.charge_query()?;
        Ok(self
            .objects
            .merge_bases_with(left, right, self.options.history)?)
    }

    fn synthesize(&mut self, bases: &[ObjectId], depth: usize) -> Result<ObjectId, CommitError> {
        self.check()?;
        if depth > self.options.max_base_depth {
            return Err(CommitError::Limit("depth"));
        }
        if bases.len() > self.options.max_bases {
            return Err(CommitError::Limit("bases"));
        }
        let Some(first) = bases.first() else {
            let tree = Tree::new(self.objects.object_format(), vec![]).map_err(TreeError::from)?;
            let id = tree.id();
            if !self.memory.trees.contains_key(&id) {
                self.memory.order.push(id);
            }
            self.memory.trees.insert(id, tree);
            return Ok(id);
        };
        let mut tree = self.objects.read_commit(*first)?.tree();
        let mut accumulated = vec![*first];
        for next in &bases[1..] {
            self.check()?;
            self.remaining = self
                .remaining
                .checked_sub(1)
                .ok_or(CommitError::Limit("merges"))?;
            let common = self.common(&accumulated, *next)?;
            let base = self.synthesize(&common, depth + 1)?;
            let next_tree = self.objects.read_commit(*next)?.tree();
            let mut result = super::renames::merge_with_memory_and_driver(
                self.objects,
                [base, tree, next_tree],
                self.options.trees,
                self.cancel,
                &mut self.memory,
                self.driver.as_deref_mut(),
            )
            .map_err(trees)?;
            if !result.is_clean() {
                result.blobs.extend(
                    self.memory
                        .blobs
                        .iter()
                        .map(|(id, bytes)| (*id, bytes.clone())),
                );
                return Err(CommitError::ConflictedBase(Box::new(result)));
            }
            tree = result.tree.id();
            for object in result.trees {
                let id = object.id();
                if !self.memory.trees.contains_key(&id) {
                    self.memory.order.push(id);
                }
                self.memory.trees.insert(id, object);
            }
            self.memory.blobs.extend(result.blobs);
            accumulated.push(*next);
        }
        Ok(tree)
    }

    fn common(&mut self, left: &[ObjectId], right: ObjectId) -> Result<Vec<ObjectId>, CommitError> {
        let mut common = BTreeSet::new();
        for tip in left {
            self.check()?;
            common.extend(self.query_bases(*tip, right)?);
            if common.len() > self.options.max_bases {
                return Err(CommitError::Limit("bases"));
            }
        }
        let mut best = Vec::new();
        for candidate in &common {
            let mut dominated = false;
            for other in &common {
                self.check()?;
                if candidate == other {
                    continue;
                }
                self.charge_query()?;
                if self
                    .objects
                    .is_ancestor_with(*candidate, *other, self.options.history)?
                {
                    dominated = true;
                    break;
                }
            }
            if !dominated {
                best.push(*candidate);
            }
        }
        Ok(best)
    }
}
