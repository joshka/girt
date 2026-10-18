//! Lookup of tracked paths and their directories during the walk, and the comparison of each
//! entry with what the walk observed.
use super::Observed;
use crate::index;
use crate::status::Change;
use crate::status::types::value;

/// Where an observation of one path is recorded: a path can be both a tracked leaf and the
/// directory of another tracked path.
#[derive(Clone, Copy)]
pub(super) struct Slots {
    pub(super) entry: Option<usize>,
    pub(super) directory: Option<usize>,
}

pub(super) struct Tracked<'a> {
    /// Sorted by path, then stage.
    pub(super) entries: &'a [index::Entry],
    /// Every ancestor of an entry path, sorted and unique.
    pub(super) directories: Vec<&'a [u8]>,
    /// Number of distinct children of the root, then of each directory; empty when not needed.
    child_counts: Vec<usize>,
}

impl<'a> Tracked<'a> {
    /// Indexes `entries`, counting each directory's children when `count_children` is set.
    pub(super) fn new(
        entries: &'a [index::Entry],
        directories: Vec<&'a [u8]>,
        count_children: bool,
    ) -> Self {
        let mut tracked = Self {
            entries,
            directories,
            child_counts: Vec::new(),
        };
        if count_children {
            let mut counts = vec![0; tracked.directories.len() + 1];
            let mut previous: Option<&[u8]> = None;
            for entry in entries {
                // Stages of one path are adjacent and count once.
                if previous != Some(&entry.path[..]) {
                    counts[tracked.parent_slot(&entry.path)] += 1;
                }
                previous = Some(&entry.path);
            }
            for directory in &tracked.directories {
                counts[tracked.parent_slot(directory)] += 1;
            }
            tracked.child_counts = counts;
        }
        tracked
    }

    /// Position in `child_counts` of the directory holding `path`.
    fn parent_slot(&self, path: &[u8]) -> usize {
        path.iter()
            .rposition(|byte| *byte == b'/')
            .and_then(|end| self.directory(&path[..end]))
            .map_or(0, |directory| directory + 1)
    }

    /// Number of distinct tracked entries and directories directly inside `directory`.
    pub(super) fn child_count(&self, directory: &[u8]) -> usize {
        let slot = if directory.is_empty() {
            Some(0)
        } else {
            self.directory(directory).map(|position| position + 1)
        };
        slot.and_then(|slot| self.child_counts.get(slot).copied())
            .unwrap_or(0)
    }

    /// Names of the tracked entries and directories directly inside `directory`, in index order.
    pub(super) fn children(&self, directory: &[u8]) -> Vec<&'a [u8]> {
        let mut prefix = directory.to_vec();
        if !prefix.is_empty() {
            prefix.push(b'/');
        }
        let mut names: Vec<&'a [u8]> = Vec::new();
        let mut position = self
            .entries
            .partition_point(|entry| entry.path.as_slice() < prefix.as_slice());
        while let Some(entry) = self.entries.get(position) {
            let Some(rest) = entry.path.strip_prefix(prefix.as_slice()) else {
                break;
            };
            let name = rest.split(|byte| *byte == b'/').next().unwrap_or(rest);
            if names.last() != Some(&name) {
                names.push(name);
            }
            // Skip the rest of a subdirectory: `0` follows `/` in byte order.
            let mut next = prefix.clone();
            next.extend_from_slice(name);
            if name.len() < rest.len() {
                next.push(b'0');
                position = self
                    .entries
                    .partition_point(|entry| entry.path.as_slice() < next.as_slice());
            } else {
                position += 1;
            }
        }
        names
    }

    /// Positions of every stage recorded at `path`.
    fn stages(&self, path: &[u8]) -> std::ops::Range<usize> {
        let start = self
            .entries
            .partition_point(|entry| entry.path.as_slice() < path);
        let count = self.entries[start..]
            .iter()
            .take_while(|entry| entry.path == path)
            .count();
        start..start + count
    }

    /// The slot and entry tracked at `path`. A conflicted path returns its last stage, but the
    /// slot of its first.
    pub(super) fn entry(&self, path: &[u8]) -> Option<(usize, &index::Entry)> {
        let stages = self.stages(path);
        let last = stages.end.checked_sub(1).filter(|_| !stages.is_empty())?;
        Some((stages.start, &self.entries[last]))
    }

    /// Position of `path` among the directories holding tracked paths.
    pub(super) fn directory(&self, path: &[u8]) -> Option<usize> {
        self.directories
            .binary_search_by(|directory| (*directory).cmp(path))
            .ok()
    }

    pub(super) fn slots(&self, path: &[u8]) -> Slots {
        let stages = self.stages(path);
        Slots {
            entry: (!stages.is_empty()).then_some(stages.start),
            directory: self
                .directory(path)
                .map(|directory| self.entries.len() + directory),
        }
    }

    /// Places each observation in its slot. A slot is observed at most once.
    pub(super) fn collect(&self, observations: Vec<(usize, Observed)>) -> Vec<Option<Observed>> {
        let mut slots = vec![None; self.entries.len() + self.directories.len()];
        for (slot, observed) in observations {
            slots[slot] = Some(observed);
        }
        slots
    }

    /// For each directory, `None` if it and every ancestor were traversed, otherwise how its
    /// contents differ: deleted, or obstructed at the shallowest blocked ancestor.
    pub(super) fn directory_states(&self, observed: &[Option<Observed>]) -> Vec<Option<Change>> {
        let mut states: Vec<Option<Change>> = Vec::with_capacity(self.directories.len());
        for (position, directory) in self.directories.iter().enumerate() {
            // Sorting places a parent before its children.
            let parent = directory
                .iter()
                .rposition(|byte| *byte == b'/')
                .and_then(|end| self.directory(&directory[..end]))
                .and_then(|parent| states[parent].clone());
            let state = parent.or_else(|| match observed[self.entries.len() + position] {
                Some(Observed::Directory) => None,
                Some(_) => Some(Change::Obstructed {
                    path: directory.to_vec(),
                }),
                None => Some(Change::Deleted),
            });
            states.push(state);
        }
        states
    }

    /// How the stage-zero entry at `position` differs from what traversal observed.
    pub(super) fn difference(
        &self,
        position: usize,
        observed: &[Option<Observed>],
        directories: &[Option<Change>],
    ) -> Option<Change> {
        let entry = &self.entries[position];
        if let Some(end) = entry.path.iter().rposition(|byte| *byte == b'/') {
            // Every ancestor of an entry is a tracked directory.
            let parent = self.directory(&entry.path[..end])?;
            if let Some(change) = &directories[parent] {
                return Some(change.clone());
            }
        }
        match observed[position] {
            Some(Observed::Leaf(actual)) if actual == value(entry) => None,
            Some(Observed::Leaf(actual) | Observed::Resized(actual)) => {
                Some(Change::Modified(actual))
            }
            Some(_) => Some(Change::Obstructed {
                path: entry.path.clone(),
            }),
            None => Some(Change::Deleted),
        }
    }
}
