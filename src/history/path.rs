//! First-parent path history with conservative changed-path Bloom skipping.
use crate::{HistoryError, HistoryLimits, ObjectId, Objects, ReadLimits, TreeValue};

/// Failure to inspect first-parent history for a literal repository-relative path.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PathHistoryError {
    /// The input is not a nonempty slash-separated relative path.
    #[error("invalid history path")]
    InvalidPath,
    /// An explored history exceeds its bounds or contains a cycle.
    #[error(transparent)]
    History(#[from] HistoryError),
    /// A fallback commit could not be read and parsed.
    #[error(transparent)]
    Commit(#[from] crate::ReadCommitError),
    /// A tree on the selected path could not be read and parsed.
    #[error(transparent)]
    Tree(#[from] crate::ReadTreeError),
}

impl Objects {
    /// Finds commits changing a literal path along the first-parent chain, newest first.
    ///
    /// Compares entry identity and mode against the first parent, or an empty snapshot at a root
    /// or declared shallow boundary. Includes additions and deletions. Directories compare their
    /// subtree identities; pathspecs and rename following are not performed. An explicitly
    /// attached graph can skip tree comparisons using Bloom negatives. Missing or uncertain
    /// filters always require ordinary comparison. Uses default history and per-read bounds.
    ///
    /// # Errors
    ///
    /// Reports invalid paths, missing/corrupt commits or trees on the selected path, cycles and
    /// exhausted bounds. Bloom-skipped trees and side-parent histories are not read or verified.
    pub fn walk_path_first_parent(
        &self,
        root: ObjectId,
        path: &[u8],
    ) -> Result<Vec<ObjectId>, PathHistoryError> {
        self.walk_path_first_parent_with(root, path, HistoryLimits::default())
    }

    /// Finds first-parent path changes with explicit history and per-object read limits.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::walk_path_first_parent`]. Commit bounds count visited
    /// first-parent commits; parent bounds count followed first-parent edges. Each tree read uses
    /// `limits.read`; memory includes returned IDs and the visited set, plus one decoded tree.
    pub fn walk_path_first_parent_with(
        &self,
        root: ObjectId,
        path: &[u8],
        limits: HistoryLimits,
    ) -> Result<Vec<ObjectId>, PathHistoryError> {
        if path.is_empty()
            || path
                .split(|b| *b == b'/')
                .any(|part| part.is_empty() || part == b"." || part == b".." || part.contains(&0))
        {
            return Err(PathHistoryError::InvalidPath);
        }
        let mut seen = crate::IdSet::default();
        let mut remaining = limits.max_parents;
        let mut next = Some(root);
        let mut result = Vec::new();
        while let Some(id) = next {
            if seen.len() >= limits.max_commits {
                return Err(HistoryError::Limit("commits").into());
            }
            if !seen.insert(id) {
                return Err(HistoryError::Cycle.into());
            }
            let (tree, parent) = self.path_metadata(id, limits.read)?;
            let parent = parent.filter(|_| !self.shallow_roots().contains(id));
            if parent.is_some() {
                remaining = remaining
                    .checked_sub(1)
                    .ok_or(HistoryError::Limit("parent occurrences"))?;
            }
            let negative = self
                .commit_graph
                .as_ref()
                .and_then(|g| g.might_change_path(id, path))
                == Some(false);
            // A shallow boundary is compared with empty, not the indexed stored parent.
            if !negative || self.shallow_roots().contains(id) {
                let current = path_value(self, tree, path, limits.read)?;
                let previous = if let Some(parent) = parent {
                    let (tree, _) = self.path_metadata(parent, limits.read)?;
                    path_value(self, tree, path, limits.read)?
                } else {
                    None
                };
                if current != previous {
                    result.push(id);
                }
            }
            next = parent;
        }
        Ok(result)
    }

    fn path_metadata(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> Result<(ObjectId, Option<ObjectId>), PathHistoryError> {
        if let Some(record) = self.commit_graph.as_ref().and_then(|g| g.get(id)) {
            return Ok((record.tree, record.parents.first().copied()));
        }
        let commit = self.read_commit_with(id, limits)?;
        Ok((commit.tree(), commit.parents().first().copied()))
    }
}

fn path_value(
    objects: &Objects,
    mut tree: ObjectId,
    path: &[u8],
    limits: ReadLimits,
) -> Result<Option<TreeValue>, PathHistoryError> {
    let mut parts = path.split(|b| *b == b'/').peekable();
    while let Some(part) = parts.next() {
        let value = objects.read_tree_with(tree, limits)?;
        let Some(entry) = value.entries().iter().find(|entry| entry.name == part) else {
            return Ok(None);
        };
        if parts.peek().is_none() {
            return Ok(Some(TreeValue {
                id: entry.id,
                mode: entry.mode,
            }));
        }
        if entry.mode != crate::EntryMode::Tree {
            return Ok(None);
        }
        tree = entry.id;
    }
    Ok(None)
}
