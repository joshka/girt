//! Git's choice of the next commit to test, as a pure function over the candidate graph.

use std::collections::HashSet;

use crate::ObjectId;

/// The commits a bisection step chooses from, in Git's walk order.
///
/// The first commit is the newest known-new (bad) commit. The others are the commits it reaches
/// that no known-old commit reaches, in the order `git rev-list <new> --not <olds>` walks them:
/// newest committer date first, ties in discovery order. Each commit's parents are restricted to
/// other candidates; with `--first-parent` only the first parent is kept.
///
/// With a pathspec, the walk follows Git's default history simplification and keeps the commits
/// that leave the paths unchanged, as `git rev-list --sparse` lists them. Only commits that change
/// the paths are counted and can be chosen; the others still connect the graph.
///
/// Build one from a repository with [`crate::Objects::bisect_candidates`], or directly for tests
/// and other graph sources with [`Self::new`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidates {
    ids: Vec<ObjectId>,
    parents: Vec<Vec<usize>>,
    counted: Vec<bool>,
}

/// One commit in a [`Candidates`] list.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Candidate {
    /// The commit.
    pub id: ObjectId,
    /// Positions of its candidate parents in the list.
    pub parents: Vec<usize>,
    /// Whether it changes the bisected paths; always true without a pathspec.
    pub changes_paths: bool,
}

impl Candidate {
    /// A commit that changes the bisected paths, with the given candidate parent positions.
    pub fn new(id: ObjectId, parents: Vec<usize>) -> Self {
        Self {
            id,
            parents,
            changes_paths: true,
        }
    }

    /// Marks the commit as leaving the bisected paths unchanged.
    #[must_use]
    pub fn unchanged(mut self) -> Self {
        self.changes_paths = false;
        self
    }
}

/// Why a [`Candidates`] list was rejected.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum CandidatesError {
    /// The list was empty; it must start with the new commit.
    #[error("bisect candidates must include the new commit")]
    Empty,
    /// A parent index did not name another candidate.
    #[error("candidate {index} names parent {parent}, which is not a candidate")]
    Parent {
        /// Position of the commit in the list.
        index: usize,
        /// The out-of-range or self-referencing parent position.
        parent: usize,
    },
    /// The same commit appeared twice.
    #[error("candidate {id} appears more than once")]
    Duplicate {
        /// The repeated commit.
        id: ObjectId,
    },
    /// Parent links formed a cycle, which commit history cannot.
    #[error("bisect candidate parents form a cycle")]
    Cycle,
}

/// Whether the parent links contain a cycle (Kahn's algorithm over child counts).
fn has_cycle(parents: &[Vec<usize>]) -> bool {
    let mut children = vec![0usize; parents.len()];
    for list in parents {
        for &parent in list {
            children[parent] += 1;
        }
    }

    let mut ready: Vec<usize> = (0..parents.len()).filter(|&i| children[i] == 0).collect();
    let mut visited = 0;
    while let Some(index) = ready.pop() {
        visited += 1;
        for &parent in &parents[index] {
            children[parent] -= 1;
            if children[parent] == 0 {
                ready.push(parent);
            }
        }
    }
    visited != parents.len()
}

impl Candidates {
    /// Builds a candidate list from commits in walk order.
    ///
    /// The first commit is the new commit. Each commit names its parents by position in the same
    /// list; leave out parents that are not candidates. Repeated parent positions are ignored.
    ///
    /// ```
    /// use girt::ObjectId;
    /// use girt::bisect::{Candidate, Candidates, Selection};
    ///
    /// let id = |n: u8| ObjectId::Sha1([n; 20]);
    ///
    /// // Five commits in a line above a known-old one, newest first.
    /// let line = Candidates::new((0..5).map(|n| {
    ///     let parents = if n < 4 { vec![n + 1] } else { vec![] };
    ///     Candidate::new(id(5 - n as u8), parents)
    /// }))?;
    ///
    /// let Selection::Test {
    ///     commit,
    ///     remaining,
    ///     steps,
    /// } = line.select(&Default::default())
    /// else {
    ///     panic!("five candidates need testing");
    /// };
    /// assert_eq!((commit, remaining, steps), (id(2), 2, 1));
    /// # Ok::<(), girt::bisect::CandidatesError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`CandidatesError`] for an empty list, a repeated commit, a parent position that is
    /// out of range or names the commit itself, or parent links that form a cycle.
    pub fn new(commits: impl IntoIterator<Item = Candidate>) -> Result<Self, CandidatesError> {
        let mut ids = Vec::new();
        let mut parents = Vec::new();
        let mut counted = Vec::new();
        for commit in commits {
            ids.push(commit.id);
            parents.push(commit.parents);
            counted.push(commit.changes_paths);
        }
        if ids.is_empty() {
            return Err(CandidatesError::Empty);
        }

        let mut seen = HashSet::with_capacity(ids.len());
        for id in &ids {
            if !seen.insert(*id) {
                return Err(CandidatesError::Duplicate { id: *id });
            }
        }
        for (index, list) in parents.iter_mut().enumerate() {
            if let Some(&parent) = list.iter().find(|&&p| p >= ids.len() || p == index) {
                return Err(CandidatesError::Parent { index, parent });
            }
            let mut unique = HashSet::with_capacity(list.len());
            list.retain(|p| unique.insert(*p));
        }
        if has_cycle(&parents) {
            return Err(CandidatesError::Cycle);
        }

        Ok(Self {
            ids,
            parents,
            counted,
        })
    }

    /// The candidate commits in walk order; the first is the new commit.
    pub fn ids(&self) -> &[ObjectId] {
        &self.ids
    }

    /// The candidate parents of the commit at `index`, as positions in [`Self::ids`].
    ///
    /// # Panics
    ///
    /// Panics when `index` is out of range.
    pub fn parents(&self, index: usize) -> &[usize] {
        &self.parents[index]
    }

    /// Whether the commit at `index` changes the bisected paths and so can be chosen.
    ///
    /// # Panics
    ///
    /// Panics when `index` is out of range.
    pub fn changes_paths(&self, index: usize) -> bool {
        self.counted[index]
    }

    /// The commits that can be chosen, in walk order.
    pub fn testable(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.ids
            .iter()
            .zip(&self.counted)
            .filter_map(|(id, counted)| counted.then_some(*id))
    }

    /// Picks the next commit to test as `git bisect` does, given the commits marked skip.
    ///
    /// Without skips, Git looks for a commit that reaches about half of the counted candidates,
    /// checking merges first (oldest first) and then single-parent commits as their counts become
    /// known. If none is halfway, it takes the last commit in walk order with the best split.
    ///
    /// With skips, candidates are ranked by how evenly they split the rest, then by object ID.
    /// If the best-ranked commit is not skipped it is chosen. Otherwise Git picks from the
    /// unskipped commits with a fixed pseudo-random sequence that prefers the better-ranked ones,
    /// so repeated skips spread out instead of testing neighbours of a broken commit.
    ///
    /// The candidates and skips fully determine the result, so girt and Git choose the same
    /// commit for the same state.
    pub fn select(&self, skipped: &HashSet<ObjectId>) -> Selection {
        let weights = Weights::new(self);
        let total = weights.total;
        match total {
            0 => return Selection::Untestable,
            1 => {
                // A single counted commit is the answer only if it is the known-new one.
                let only = self.testable().next().unwrap_or(self.ids[0]);
                return if only == self.ids[0] {
                    Selection::Found(only)
                } else if skipped.contains(&only) {
                    Selection::OnlySkipped(vec![only])
                } else {
                    Selection::Test {
                        commit: only,
                        remaining: 0,
                        steps: 0,
                    }
                };
            }
            _ => {}
        }
        let steps = estimate_steps(total);

        if !self.ids.iter().any(|id| skipped.contains(id)) {
            let chosen = weights.halfway(self).unwrap_or_else(|| weights.last_best());
            return Selection::Test {
                commit: self.ids[chosen],
                remaining: (total - weights.of(chosen)).saturating_sub(1),
                steps,
            };
        }

        let ranked = weights.ranked(self);
        let remaining = (total - weights.of(ranked[0])).saturating_sub(1);
        let unskipped: Vec<usize> = ranked
            .iter()
            .copied()
            .filter(|&i| !skipped.contains(&self.ids[i]))
            .collect();

        let chosen = match unskipped.first() {
            None => None,
            Some(&best) if best == ranked[0] => Some(best),
            Some(_) => {
                // Landing on the new commit steps back to the better-ranked neighbour.
                let pick = skip_away(unskipped.len());
                match unskipped[pick] {
                    0 if pick > 0 => Some(unskipped[pick - 1]),
                    index => Some(index),
                }
            }
        };

        match chosen {
            Some(index) if index != 0 => Selection::Test {
                commit: self.ids[index],
                remaining,
                steps,
            },
            _ => {
                // Git lists them in ranking order, then the new commit.
                let mut possible: Vec<ObjectId> = ranked
                    .iter()
                    .filter(|&&i| i != 0)
                    .map(|&i| self.ids[i])
                    .collect();
                possible.push(self.ids[0]);
                Selection::OnlySkipped(possible)
            }
        }
    }
}

/// The outcome of choosing the next commit to test.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Selection {
    /// Test this commit next.
    Test {
        /// The commit to check out and test.
        commit: ObjectId,
        /// Git's "N revisions left to test after this" count. Where Git prints -1 (the best
        /// split reaches every counted commit), this is 0.
        remaining: usize,
        /// Git's "roughly N steps" estimate.
        steps: usize,
    },
    /// The known-new commit is the only commit left that can be chosen, so it is the first new
    /// commit.
    Found(ObjectId),
    /// Every commit other than the new commit is skipped. Any of these could be the first new
    /// commit. As in Git's message, they are ranked like the skip choice ranks them, with the
    /// new commit last.
    OnlySkipped(Vec<ObjectId>),
    /// No candidate changes the bisected paths. Git reports "No testable commit found".
    Untestable,
}

/// Git's "roughly N steps" estimate for `total` candidates, the new commit included.
///
/// With `e = floor(log2 total)` and `x = total - 2^e`, this is `e - 1`, plus one when
/// `3x >= 2^e`.
///
/// ```
/// use girt::bisect::estimate_steps;
/// assert_eq!(estimate_steps(15), 3);
/// assert_eq!(estimate_steps(22), 4);
/// ```
pub fn estimate_steps(total: usize) -> usize {
    if total < 2 {
        return 0;
    }
    let exponent = total.ilog2() as usize;
    let power = 1usize << exponent;
    let extra = total - power;
    exponent - 1 + usize::from(3 * extra >= power)
}

/// The position Git picks among `count` unskipped candidates when the best one is skipped.
///
/// A linear congruential generator seeded with `count` yields `p` in `0..32768`, and the
/// position is `(count * p / 32768) * isqrt(p) / isqrt(32768)` in integer arithmetic. The
/// `p^1.5` shape favours candidates near the top of the ranking.
fn skip_away(count: usize) -> usize {
    const MODULO: u64 = 32768;
    let seed = (count as u64)
        .wrapping_mul(1_103_515_245)
        .wrapping_add(12345)
        & 0xffff_ffff;
    let random = (seed >> 16) % MODULO;
    let scaled = (count as u64) * random / MODULO;
    let position = scaled * random.isqrt() / MODULO.isqrt();
    (position as usize).min(count - 1)
}

/// How many counted candidates each candidate reaches, itself included.
struct Weights {
    reach: Vec<usize>,
    counted: Vec<bool>,
    total: usize,
}

impl Weights {
    fn new(candidates: &Candidates) -> Self {
        let size = candidates.ids.len();
        let counted = candidates.counted.clone();
        let total = counted.iter().filter(|&&c| c).count();
        let mut reach = vec![0; size];
        let mut known = vec![false; size];
        let mut stamp = vec![usize::MAX; size];
        let mut stack = Vec::new();

        // Merges count what they reach directly; everything else adds itself to its parent.
        for (index, parents) in candidates.parents.iter().enumerate() {
            match parents.len() {
                0 => reach[index] = usize::from(counted[index]),
                1 => continue,
                _ => {
                    reach[index] = count_reachable(candidates, index, &mut stamp, &mut stack);
                }
            }
            known[index] = true;
        }
        let mut pending: Vec<usize> = (0..size).filter(|&i| !known[i]).collect();
        while !pending.is_empty() {
            let before = pending.len();
            pending.retain(|&index| {
                let parent = candidates.parents[index][0];
                if !known[parent] {
                    return true;
                }
                reach[index] = reach[parent] + usize::from(counted[index]);
                known[index] = true;
                false
            });
            // `Candidates::new` rejects cycles, so every pass resolves at least one commit.
            assert!(pending.len() < before, "bisect candidates contain a cycle");
        }

        Self {
            reach,
            counted,
            total,
        }
    }

    fn of(&self, index: usize) -> usize {
        self.reach[index]
    }

    fn distance(&self, index: usize) -> usize {
        self.reach[index].min(self.total - self.reach[index])
    }

    fn is_halfway(&self, index: usize) -> bool {
        self.counted[index] && (2 * self.reach[index]).abs_diff(self.total) <= 1
    }

    /// Git's early answer: the first counted commit found to split the candidates in half.
    ///
    /// Merges are checked oldest first. Then single-parent commits are checked in repeated
    /// oldest-first passes as their parent's count becomes known; commits with no candidate
    /// parent are never checked.
    fn halfway(&self, candidates: &Candidates) -> Option<usize> {
        let order = (0..self.reach.len()).rev();
        if let Some(found) = order
            .clone()
            .find(|&i| candidates.parents[i].len() > 1 && self.is_halfway(i))
        {
            return Some(found);
        }

        let mut known: Vec<bool> = candidates.parents.iter().map(|p| p.len() != 1).collect();
        loop {
            let mut changed = false;
            for index in order.clone() {
                if known[index] || !known[candidates.parents[index][0]] {
                    continue;
                }
                known[index] = true;
                changed = true;
                if self.is_halfway(index) {
                    return Some(index);
                }
            }
            if !changed {
                return None;
            }
        }
    }

    /// The last counted commit in walk order with the largest distance.
    fn last_best(&self) -> usize {
        (0..self.reach.len())
            .filter(|&i| self.counted[i])
            .max_by_key(|&i| (self.distance(i), i))
            .unwrap_or(0)
    }

    /// Counted candidates by descending distance, ties by ascending object ID.
    fn ranked(&self, candidates: &Candidates) -> Vec<usize> {
        let mut order: Vec<usize> = (0..candidates.ids.len())
            .filter(|&i| self.counted[i])
            .collect();
        order.sort_by(|&a, &b| {
            self.distance(b)
                .cmp(&self.distance(a))
                .then_with(|| candidates.ids[a].cmp(&candidates.ids[b]))
        });
        order
    }
}

/// Counts the counted candidates reachable from `start`, itself included.
fn count_reachable(
    candidates: &Candidates,
    start: usize,
    stamp: &mut [usize],
    stack: &mut Vec<usize>,
) -> usize {
    let mut count = 0;
    stack.clear();
    stack.push(start);
    stamp[start] = start;
    while let Some(index) = stack.pop() {
        count += usize::from(candidates.counted[index]);
        for &parent in &candidates.parents[index] {
            if stamp[parent] != start {
                stamp[parent] = start;
                stack.push(parent);
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: usize) -> ObjectId {
        let mut bytes = [0; 20];
        bytes[..8].copy_from_slice(&(n as u64).to_be_bytes());
        ObjectId::Sha1(bytes)
    }

    /// A line of `count` candidates, newest (`count`) first.
    fn line(count: usize) -> Candidates {
        Candidates::new((0..count).map(|i| {
            let parents = if i + 1 < count { vec![i + 1] } else { vec![] };
            Candidate::new(id(count - i), parents)
        }))
        .unwrap()
    }

    #[test]
    fn step_estimates_match_git() {
        // Observed from `git rev-list --bisect-vars` on lines of 2 to 70 commits.
        let thresholds = [
            (2, 0),
            (3, 1),
            (5, 1),
            (6, 2),
            (10, 2),
            (11, 3),
            (21, 3),
            (22, 4),
        ];
        for (total, steps) in thresholds {
            assert_eq!(estimate_steps(total), steps, "{total}");
        }
        assert_eq!(estimate_steps(42), 4);
        assert_eq!(estimate_steps(43), 5);
    }

    #[test]
    fn skip_positions_match_git() {
        // Observed by skipping the best commit of a 70-commit line and counting unskipped ones.
        let observed = [
            (69, 20),
            (68, 61),
            (41, 0),
            (40, 16),
            (35, 33),
            (33, 30),
            (16, 1),
            (7, 3),
            (5, 1),
            (2, 0),
        ];
        for (count, position) in observed {
            assert_eq!(skip_away(count), position, "{count}");
        }
    }

    #[test]
    fn line_picks_lower_middle() {
        // Git picks c67 of c66..c70 (weight 2 of 5) and c69 of c68..c70.
        let Selection::Test {
            commit, remaining, ..
        } = line(5).select(&HashSet::new())
        else {
            panic!()
        };
        assert_eq!((commit, remaining), (id(2), 2));

        let Selection::Test {
            commit, remaining, ..
        } = line(3).select(&HashSet::new())
        else {
            panic!()
        };
        assert_eq!((commit, remaining), (id(2), 0));
    }

    #[test]
    fn single_candidate_is_found() {
        assert_eq!(line(1).select(&HashSet::new()), Selection::Found(id(1)));
    }

    #[test]
    fn all_skipped_lists_new_last() {
        let skipped = [id(1), id(2)].into_iter().collect();
        assert_eq!(
            line(3).select(&skipped),
            Selection::OnlySkipped(vec![id(1), id(2), id(3)])
        );
    }

    #[test]
    fn unchanged_commits_connect_but_are_not_counted() {
        // `git bisect start c10 c1 -- b` where only c3, c6 and c9 touch `b`: Git tests c3, the
        // last best split in walk order, because no counted commit is exactly halfway.
        let commits = (2..=10).rev().map(|n| {
            let parents = if n > 2 { vec![10 - n + 1] } else { vec![] };
            let candidate = Candidate::new(id(n), parents);
            if n % 3 == 0 {
                candidate
            } else {
                candidate.unchanged()
            }
        });
        let candidates = Candidates::new(commits).unwrap();

        assert_eq!(
            candidates.select(&HashSet::new()),
            Selection::Test {
                commit: id(3),
                remaining: 1,
                steps: 1
            }
        );
    }

    #[test]
    fn rejects_bad_parents() {
        assert_eq!(
            Candidates::new([Candidate::new(id(1), vec![0])]),
            Err(CandidatesError::Parent {
                index: 0,
                parent: 0
            })
        );
        assert_eq!(Candidates::new([]), Err(CandidatesError::Empty));
    }
}
