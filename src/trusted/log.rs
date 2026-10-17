use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::fmt::Display;
use std::sync::Arc;

use super::reader::ObjectView;
use super::{Error, Handle};
use crate::{Commit, HistoryError, HistoryLimits, ObjectId, ObjectKind};

/// A lazy committer-date-priority walk, newest discovered commit first.
///
/// This prototype is deliberately narrower than `git log --date-order`: it does not compute
/// topology ranks or exclusions. Timestamp-skewed ancestors can appear after older children.
/// Ties use object-ID order. The root is read at construction; parents are read only on the next
/// call after yielding a commit, so `take(1)` does not read its parents. Each ID is yielded once.
/// Uses [`crate::Objects::read_commit`] and the default [`HistoryLimits`]; shallow roots stop
/// edges. The frontier retains full commit bodies: memory can approach the number of discovered
/// commits times the per-object read bound, plus graph bookkeeping. There is no aggregate byte
/// budget. The iterator owns shared reader state and does not borrow its originating handle.
/// Other handle reads can run between steps, and the iterator can outlive the handle. Drop to stop
/// walking.
#[derive(Debug)]
pub struct Log {
    reader: Arc<ObjectView>,
    frontier: BinaryHeap<Candidate>,
    seen: HashSet<ObjectId>,
    pending: Vec<ObjectId>,
    parents: usize,
    failed: bool,
}

#[derive(Debug)]
struct Candidate {
    seconds: i64,
    id: ObjectId,
    commit: Commit,
}
impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        (self.seconds, self.id) == (other.seconds, other.id)
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.seconds, self.id).cmp(&(other.seconds, other.id))
    }
}

impl Handle {
    /// Starts a lazy date-priority walk at a revision, peeling tags with
    /// [`crate::revision::Resolver::resolve_to`]. Accepts text or an [`ObjectId`], as in
    /// [`Self::rev_parse`]. See [`Log`] for ordering and scope.
    ///
    /// # Errors
    ///
    /// Reports root resolution/read failures. Later errors are iterator items; the iterator stops
    /// after its first error. No history is silently omitted except declared shallow boundaries.
    pub fn log(&self, revision: impl Display) -> crate::Result<Log> {
        let root = self.reader.resolve(revision, Some(ObjectKind::Commit))?;
        let mut log = Log {
            reader: Arc::clone(&self.reader),
            frontier: BinaryHeap::new(),
            seen: HashSet::new(),
            pending: Vec::new(),
            parents: 0,
            failed: false,
        };
        log.discover(root)?;
        Ok(log)
    }
}

impl Log {
    fn discover(&mut self, id: ObjectId) -> crate::Result<()> {
        if self.seen.contains(&id) {
            return Ok(());
        }
        if self.seen.len() >= HistoryLimits::default().max_commits {
            return Err(HistoryError::Limit("commits").into());
        }
        let commit = self.reader.read_commit(id)?;
        let seconds = commit
            .committer()?
            .ok_or(Error::MissingCommitter { id })?
            .date()?
            .seconds;
        self.seen.insert(id);
        self.frontier.push(Candidate {
            seconds,
            id,
            commit,
        });
        Ok(())
    }

    fn advance(&mut self) -> crate::Result<Option<(ObjectId, Commit)>> {
        for id in std::mem::take(&mut self.pending) {
            self.discover(id)?;
        }
        let Some(candidate) = self.frontier.pop() else {
            return Ok(None);
        };
        if !self
            .reader
            .repository
            .shallow_roots()
            .contains(candidate.id)
        {
            self.parents = self
                .parents
                .checked_add(candidate.commit.parents().len())
                .ok_or(HistoryError::Limit("parents"))?;
            if self.parents > HistoryLimits::default().max_parents {
                return Err(HistoryError::Limit("parents").into());
            }
            self.pending = candidate.commit.parents().to_vec();
        }
        Ok(Some((candidate.id, candidate.commit)))
    }
}

impl Iterator for Log {
    type Item = crate::Result<(ObjectId, Commit)>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.advance() {
            Ok(item) => item.map(Ok),
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}
impl std::iter::FusedIterator for Log {}
