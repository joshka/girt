//! Collecting bisect candidates in Git's `rev-list` walk order.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::atomic::AtomicBool;

use super::{Candidate, Candidates, Error};
use crate::pathspec::Pathspec;
use crate::{Commit, HistoryLimits, ObjectId, Objects, TreeCompareLimits};

/// Which history a bisection searches.
///
/// The default searches every commit between the new and old commits. Git records these choices
/// in `BISECT_FIRST_PARENT` and `BISECT_NAMES`.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Scope {
    /// Follow only first parents, as `git bisect start --first-parent` does.
    pub first_parent: bool,
    /// Only commits that change these paths can be chosen; `None` selects every path.
    pub paths: Option<Pathspec>,
}

impl Scope {
    /// Sets [`Self::first_parent`].
    #[must_use]
    pub fn with_first_parent(mut self, first_parent: bool) -> Self {
        self.first_parent = first_parent;
        self
    }

    /// Sets [`Self::paths`].
    #[must_use]
    pub fn with_paths(mut self, paths: Option<Pathspec>) -> Self {
        self.paths = paths;
        self
    }
}

impl Objects {
    /// Collects the commits a bisection between `new` and `olds` chooses from.
    ///
    /// The candidates are the commits `new` reaches that no commit in `olds` reaches, in the order
    /// `git rev-list` walks them: newest committer date first, ties in discovery order, starting
    /// from `new` and then `olds`. With [`Scope::paths`], merges that leave the paths unchanged
    /// relative to a candidate or old parent follow only that parent, and commits that leave them
    /// unchanged are kept but not counted (see [`Candidates`]). This is how `git bisect` walks;
    /// `git rev-list --bisect` can differ when it has not yet seen that a parent is excluded.
    ///
    /// Membership is exact reachability, so commits with committer dates far out of order are
    /// classified correctly where Git's walk may stop early and keep a few extra commits.
    ///
    /// # Errors
    ///
    /// Returns [`Error::History`] for missing, malformed or over-limit history,
    /// [`Error::ReadCommit`] when a commit cannot be read, and [`Error::TreeCompare`] when a path
    /// comparison fails.
    pub fn bisect_candidates(
        &self,
        new: ObjectId,
        olds: &[ObjectId],
        scope: &Scope,
    ) -> Result<Candidates, Error> {
        let limits = HistoryLimits::default();
        let included: HashSet<ObjectId> = self
            .walk_range_with(&[new], olds, limits)?
            .into_iter()
            .collect();

        let mut walk = Walk::default();
        walk.push(self, new)?;
        for &old in olds {
            walk.push(self, old)?;
        }

        let cancel = AtomicBool::new(false);
        let mut order = Vec::new();
        let mut followed: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
        let mut unchanged = HashSet::new();
        let mut interesting_queued = usize::from(included.contains(&new));
        while interesting_queued > 0 {
            let Some((id, commit)) = walk.pop() else {
                break;
            };
            let interesting = included.contains(&id);
            let mut parents = commit.parents().to_vec();
            if scope.first_parent {
                parents.truncate(1);
            }

            if interesting {
                interesting_queued -= 1;
                if let Some(paths) = &scope.paths {
                    let relevant =
                        |parent: &ObjectId| included.contains(parent) || olds.contains(parent);
                    let (shown, follow) =
                        self.simplify(&commit, parents, paths, relevant, &cancel)?;
                    if !shown {
                        unchanged.insert(id);
                    }
                    parents = follow;
                }
                order.push(id);
                followed.insert(id, parents.clone());
            }

            for parent in parents {
                if walk.push(self, parent)? && included.contains(&parent) {
                    interesting_queued += 1;
                }
            }
        }

        let positions: HashMap<ObjectId, usize> =
            order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let candidates = order.iter().map(|id| {
            let parents = followed[id]
                .iter()
                .filter_map(|parent| positions.get(parent).copied())
                .collect();
            let candidate = Candidate::new(*id, parents);
            if unchanged.contains(id) {
                candidate.unchanged()
            } else {
                candidate
            }
        });
        Ok(Candidates::new(candidates)?)
    }
}

impl Objects {
    /// Git's path simplification as `git bisect` applies it, with ancestry already known.
    ///
    /// A commit is shown when it changes the paths relative to its parent (a root, relative to
    /// the empty tree). A merge that leaves the paths unchanged relative to a *relevant* parent
    /// is hidden and follows only that parent; the relevant parents are candidates and the given
    /// old commits. Matches against other excluded parents are ignored, so a merge whose only
    /// unchanged side is excluded history is still shown and follows every parent.
    fn simplify(
        &self,
        commit: &Commit,
        parents: Vec<ObjectId>,
        paths: &Pathspec,
        relevant: impl Fn(&ObjectId) -> bool,
        cancel: &AtomicBool,
    ) -> Result<(bool, Vec<ObjectId>), Error> {
        let limits = TreeCompareLimits::default();
        let differs = |parent: Option<ObjectId>| -> Result<bool, Error> {
            let old = parent
                .map(|id| self.read_commit_with(id, limits.read).map(|c| c.tree()))
                .transpose()?;
            let changes =
                self.compare_trees_matching(old, Some(commit.tree()), paths, limits, cancel)?;
            Ok(!changes.is_empty())
        };

        match parents.as_slice() {
            [] => Ok((differs(None)?, parents)),
            [parent] => Ok((differs(Some(*parent))?, parents)),
            _ => {
                for parent in &parents {
                    if relevant(parent) && !differs(Some(*parent))? {
                        return Ok((false, vec![*parent]));
                    }
                }
                Ok((true, parents))
            }
        }
    }
}

/// A committer-date priority queue with ties in insertion order, as Git's revision walk uses.
#[derive(Default)]
struct Walk {
    queue: BinaryHeap<(i64, Reverse<usize>)>,
    entries: Vec<Option<(ObjectId, Commit)>>,
    seen: HashSet<ObjectId>,
}

impl Walk {
    /// Queues a commit the first time it is seen; returns whether it was new.
    fn push(&mut self, objects: &Objects, id: ObjectId) -> Result<bool, Error> {
        if !self.seen.insert(id) {
            return Ok(false);
        }
        let limit = HistoryLimits::default().max_commits;
        if self.seen.len() > limit {
            return Err(crate::HistoryError::Limit("commits").into());
        }

        let commit = objects.read_commit(id)?;
        // Git treats an unreadable committer date as zero, which sorts the commit last.
        let seconds = commit
            .committer()
            .ok()
            .flatten()
            .and_then(|identity| identity.date().ok())
            .map_or(0, |date| date.seconds);
        let sequence = self.entries.len();
        self.entries.push(Some((id, commit)));
        self.queue.push((seconds, Reverse(sequence)));
        Ok(true)
    }

    fn pop(&mut self) -> Option<(ObjectId, Commit)> {
        let (_, Reverse(sequence)) = self.queue.pop()?;
        self.entries[sequence].take()
    }
}
