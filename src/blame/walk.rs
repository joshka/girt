use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Error, Hunk, Limits};
use crate::content_diff::{BinaryMode, ContentDiff, DiffLimits};
use crate::{ObjectId, Objects};

struct File {
    id: ObjectId,
    bytes: Arc<Vec<u8>>,
}

struct State {
    commit: ObjectId,
    seconds: i64,
    path: Vec<u8>,
    file: File,
    // (destination line, current source line), both zero-based.
    lines: Vec<(usize, usize)>,
}
#[derive(Default)]
struct Pending {
    order: BinaryHeap<(i64, ObjectId, Vec<u8>)>,
    states: HashMap<(ObjectId, Vec<u8>), State>,
}
impl Pending {
    fn push(&mut self, state: State) {
        let key = (state.commit, state.path.clone());
        if let Some(existing) = self.states.get_mut(&key) {
            existing.lines.extend(state.lines);
        } else {
            self.order.push((state.seconds, key.0, key.1.clone()));
            self.states.insert(key, state);
        }
    }
    fn pop(&mut self) -> Option<State> {
        self.order
            .pop()
            .and_then(|(_, id, path)| self.states.remove(&(id, path)))
    }
}

struct Walk<'a> {
    objects: &'a Objects,
    limits: Limits,
    cancel: &'a AtomicBool,
    steps: usize,
    bytes: usize,
}
impl Objects {
    /// Attributes a regular file at a commit using default [`Limits`].
    /// Wraps [`Self::blame_with`]; see the [module contract](crate::blame).
    ///
    /// # Errors
    ///
    /// Returns missing, malformed, unsupported, shallow-history or resource errors.
    pub fn blame(&self, commit: ObjectId, path: impl AsRef<[u8]>) -> Result<Vec<Hunk>, Error> {
        self.blame_with(commit, path, Limits::default(), &AtomicBool::new(false))
    }

    /// Attributes lines with explicit whole-walk bounds and cancellation.
    ///
    /// An identical parent file takes precedence; otherwise parents are considered in stored
    /// order, passing unchanged lines to the first matching parent. Missing objects are errors,
    /// never history roots. Both SHA-1 and SHA-256 object stores are supported. No repository
    /// data is changed.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] without partial results. Increase the relevant limit or fetch missing
    /// history before retrying. Cancellation is checked between reads and during comparisons.
    pub fn blame_with(
        &self,
        commit: ObjectId,
        path: impl AsRef<[u8]>,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Vec<Hunk>, Error> {
        let path = path.as_ref();
        if path.is_empty()
            || path
                .split(|b| *b == b'/')
                .any(|p| p.is_empty() || p == b"." || p == b".." || p.contains(&0))
        {
            return Err(Error::InvalidPath);
        }
        let mut walk = Walk {
            objects: self,
            limits,
            cancel,
            steps: limits.max_steps,
            bytes: limits.max_bytes,
        };
        walk.check()?;
        let initial = self
            .read_commit_with(commit, limits.read)
            .map_err(crate::Error::from)?;
        let tree = initial.tree();
        let blob = walk.file(tree, path)?.ok_or(Error::Absent)?;
        let file = walk.content(blob)?;
        if file.bytes.contains(&0) {
            return Err(Error::Binary);
        }
        let count = line_count(&file.bytes);
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut pending = Pending::default();
        pending.push(State {
            commit,
            seconds: priority(&initial),
            path: path.to_vec(),
            file,
            lines: (0..count).map(|n| (n, n)).collect(),
        });
        let mut origins = vec![None; count];
        while let Some(mut state) = pending.pop() {
            walk.step()?;
            let current = self
                .read_commit_with(state.commit, limits.read)
                .map_err(crate::Error::from)?;
            if self.shallow_roots().contains(state.commit) {
                return Err(Error::Shallow(state.commit));
            }
            let id = state.file.id;
            let mut parents = Vec::new();
            for parent in current.parents() {
                walk.step()?;
                let parent_commit = self
                    .read_commit_with(*parent, limits.read)
                    .map_err(crate::Error::from)?;
                if let Some((path, old_id)) =
                    walk.predecessor(parent_commit.tree(), current.tree(), &state.path)?
                {
                    parents.push((*parent, path, old_id, priority(&parent_commit)));
                }
            }
            // Git passes the whole file to an identical parent before considering partial
            // matches, even when that parent comes later in the commit's parent list.
            if let Some(index) = parents.iter().position(|(_, _, old_id, _)| *old_id == id) {
                let (parent, path, _, seconds) = parents.swap_remove(index);
                state.commit = parent;
                state.seconds = seconds;
                state.path = path;
                pending.push(state);
                continue;
            }
            let content = &state.file.bytes;
            for (parent, path, old_id, seconds) in parents {
                if state.lines.is_empty() {
                    break;
                }
                let old = walk.content(old_id)?;
                let mapping = unchanged(&old.bytes, content, limits.diff, cancel)?;
                let mut inherited = Vec::new();
                state.lines.retain(|&(destination, source)| {
                    if let Some(original) = mapping[source] {
                        inherited.push((destination, original));
                        false
                    } else {
                        true
                    }
                });
                if !inherited.is_empty() {
                    pending.push(State {
                        commit: parent,
                        seconds,
                        path,
                        file: old,
                        lines: inherited,
                    });
                }
            }
            let path = Arc::new(state.path);
            for (destination, source) in state.lines {
                origins[destination] = Some((state.commit, Arc::clone(&path), source));
            }
        }
        let mut hunks: Vec<Hunk> = Vec::new();
        for (line, origin) in origins.into_iter().enumerate() {
            let (commit, path, source) = origin.expect("every pending line receives an origin");
            if let Some(last) = hunks.last_mut()
                && last.commit == commit
                && last.original_path == *path
                && last.original_start + last.line_count == source + 1
            {
                last.line_count += 1;
            } else {
                hunks.push(Hunk {
                    commit,
                    original_path: (*path).clone(),
                    original_start: source + 1,
                    final_start: line + 1,
                    line_count: 1,
                });
            }
        }
        walk.check()?;
        Ok(hunks)
    }
}
impl Walk<'_> {
    fn check(&self) -> Result<(), Error> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    fn step(&mut self) -> Result<(), Error> {
        self.check()?;
        self.steps = self.steps.checked_sub(1).ok_or(Error::Limit("history"))?;
        Ok(())
    }
    fn file(&self, mut tree: ObjectId, path: &[u8]) -> Result<Option<ObjectId>, Error> {
        let mut components = path.split(|b| *b == b'/').peekable();
        while let Some(component) = components.next() {
            self.check()?;
            let value = self
                .objects
                .read_tree_with(tree, self.limits.read)
                .map_err(crate::Error::from)?;
            let Some(entry) = value.entries().iter().find(|e| e.name == component) else {
                return Ok(None);
            };
            if components.peek().is_some() {
                if entry.mode != crate::EntryMode::Tree {
                    return Ok(None);
                }
            } else if !matches!(
                entry.mode,
                crate::EntryMode::Blob | crate::EntryMode::Executable
            ) {
                return Err(Error::NotFile);
            }
            tree = entry.id;
        }
        Ok(Some(tree))
    }
    fn content(&mut self, id: ObjectId) -> Result<File, Error> {
        self.check()?;
        let bytes = self
            .objects
            .read_blob_with(id, self.limits.read)
            .map_err(crate::Error::from)?;
        self.bytes = self
            .bytes
            .checked_sub(bytes.len())
            .ok_or(Error::Limit("bytes"))?;
        if line_count(&bytes) > self.limits.max_lines {
            return Err(Error::Limit("lines"));
        }
        Ok(File {
            id,
            bytes: Arc::new(bytes),
        })
    }
    fn predecessor(
        &self,
        old: ObjectId,
        new: ObjectId,
        path: &[u8],
    ) -> Result<Option<(Vec<u8>, ObjectId)>, Error> {
        let previous = match self.file(old, path) {
            Err(Error::NotFile) => None,
            result => result?,
        };
        if let Some(id) = previous {
            return Ok(Some((path.to_vec(), id)));
        }
        let comparison = self
            .objects
            .diff_trees_with(
                Some(old),
                Some(new),
                crate::rewrites::Options::default()
                    .with_track_empty(true)
                    .with_candidate_limit(self.limits.rewrites.max_pairs),
                self.limits.rewrites,
                self.cancel,
            )
            .map_err(crate::Error::from)?;
        for entry in comparison.entries {
            if let crate::tree_diff::Entry::Rewrite {
                source,
                target,
                old,
                ..
            } = entry
                && target == path
                && matches!(
                    old.mode,
                    crate::EntryMode::Blob | crate::EntryMode::Executable
                )
            {
                return Ok(Some((source, old.id)));
            }
        }
        if comparison.inexact_skipped.is_some() {
            return Err(Error::IncompleteRenames);
        }
        Ok(None)
    }
}
// Dates schedule work only; malformed/missing dates sort oldest without changing attribution.
// Timestamp skew can still cause a shared ancestor to be revisited, within whole-walk bounds.
fn priority(commit: &crate::Commit) -> i64 {
    commit
        .committer()
        .ok()
        .flatten()
        .and_then(|person| person.date().ok())
        .map_or(i64::MIN, |date| date.seconds)
}
fn line_count(bytes: &[u8]) -> usize {
    bytes.iter().filter(|b| **b == b'\n').count()
        + usize::from(!bytes.is_empty() && bytes.last() != Some(&b'\n'))
}
fn unchanged(
    old: &[u8],
    new: &[u8],
    limits: DiffLimits,
    cancel: &AtomicBool,
) -> Result<Vec<Option<usize>>, Error> {
    let diff = crate::content_diff::diff(old, new, BinaryMode::Text, limits, cancel)
        .map_err(crate::Error::from)?;
    let mut mapping = vec![None; line_count(new)];
    let mut old_cursor = 0;
    let mut new_cursor = 0;
    if let ContentDiff::Text(text) = diff {
        for edit in text.edits() {
            for slot in &mut mapping[new_cursor..edit.new_lines.start] {
                *slot = Some(old_cursor);
                old_cursor += 1;
            }
            old_cursor = edit.old_lines.end;
            new_cursor = edit.new_lines.end;
        }
    }
    for slot in &mut mapping[new_cursor..] {
        *slot = Some(old_cursor);
        old_cursor += 1;
    }
    Ok(mapping)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::empty(b"", b"", vec![])]
    #[case::insert(b"a\nc\n", b"a\nb\nc\n", vec![Some(0), None, Some(1)])]
    #[case::delete(b"a\nb\nc\n", b"a\nc\n", vec![Some(0), Some(2)])]
    #[case::replace(b"a\nb\nc", b"a\nB\nc", vec![Some(0), None, Some(2)])]
    #[case::newline(b"a\nb", b"a\nb\n", vec![Some(0), None])]
    fn unchanged_line_mapping(
        #[case] old: &[u8],
        #[case] new: &[u8],
        #[case] expected: Vec<Option<usize>>,
    ) {
        assert_eq!(
            unchanged(old, new, DiffLimits::default(), &AtomicBool::new(false)).unwrap(),
            expected
        );
    }
}
