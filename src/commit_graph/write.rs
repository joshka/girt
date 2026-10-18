//! Verified graph construction and standalone/split artifact encoding.
use std::collections::BTreeMap;

use super::{Checksum, Chunks, CommitGraph, GraphCommit, GraphError, GraphLimits, bad, id};
use crate::{HistoryError, HistoryLimits, IdMap, ObjectFormat, ObjectId, Objects};

/// Failure to construct or encode a complete commit-graph artifact.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GraphWriteError {
    /// A selected commit could not be read and verified.
    #[error("reading commit-graph input {id}")]
    Read {
        /// Commit being read.
        id: ObjectId,
        /// Underlying storage, kind or parse failure.
        #[source]
        source: crate::ReadCommitError,
    },
    /// A commit lacks an unsigned timestamp representable by the graph format.
    #[error("commit {0} has no representable commit-graph timestamp")]
    Date(ObjectId),
    /// Graph construction encountered a cycle or exhausted its bounds.
    #[error(transparent)]
    History(#[from] HistoryError),
    /// Graph encoding or base-layer validation failed.
    #[error(transparent)]
    Graph(#[from] GraphError),
    /// Shallow history cannot supply the complete parent closure required by the format.
    #[error("cannot construct a commit-graph from shallow history")]
    Shallow,
}

impl CommitGraph {
    /// Builds metadata by reading every commit reachable from `roots`, with default bounds.
    ///
    /// Reads verified objects even when `objects` has an attached auxiliary index. The result
    /// contains exact ordered parents, generation v1 and corrected dates; Bloom filters are not
    /// generated. Construction writes nothing. Encode with [`Self::encode`] or
    /// [`Self::encode_incremental`], then publish under the caller's writer coordination.
    ///
    /// # Errors
    ///
    /// Reports missing/corrupt/wrong-kind inputs, cycles, limits, shallow history or timestamps
    /// outside the format's unsigned 34-bit range. Does not return a partial graph.
    pub fn from_objects(objects: &Objects, roots: &[ObjectId]) -> Result<Self, GraphWriteError> {
        Self::from_objects_with(objects, roots, HistoryLimits::default())
    }

    /// Builds metadata under explicit commit, parent-occurrence and object-read bounds.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::from_objects`]. Each distinct commit is decoded once;
    /// every stored parent occurrence counts, including duplicate merge parents.
    pub fn from_objects_with(
        objects: &Objects,
        roots: &[ObjectId],
        limits: HistoryLimits,
    ) -> Result<Self, GraphWriteError> {
        if !objects.shallow_roots().is_empty() {
            return Err(GraphWriteError::Shallow);
        }
        let mut ids = Vec::new();
        let mut positions = IdMap::default();
        for root in roots {
            discover(*root, &mut ids, &mut positions, limits.max_commits)?;
        }
        let mut commits = Vec::new();
        let mut remaining = limits.max_parents;
        let mut index = 0;
        while index < ids.len() {
            let id = ids[index];
            let commit = objects
                .read_commit_with(id, limits.read)
                .map_err(|source| GraphWriteError::Read { id, source })?;
            let time = commit
                .committer()
                .ok()
                .flatten()
                .and_then(|person| person.date().ok())
                .and_then(|date| u64::try_from(date.seconds).ok())
                .filter(|time| *time < 1 << 34)
                .ok_or(GraphWriteError::Date(id))?;
            remaining = remaining
                .checked_sub(commit.parents().len())
                .ok_or(HistoryError::Limit("parent occurrences"))?;
            for parent in commit.parents() {
                discover(*parent, &mut ids, &mut positions, limits.max_commits)?;
            }
            commits.push(GraphCommit {
                id,
                tree: commit.tree(),
                parents: commit.parents().to_vec(),
                commit_time: time,
                generation: 0,
                corrected_commit_date: None,
            });
            index += 1;
        }
        set_generations(&mut commits, &positions)?;
        commits.sort_unstable_by_key(|record| record.id);

        // Graphs are read from file bytes, so encode the records and check them like a file.
        let format = objects.object_format();
        let positions: IdMap<u32> = commits
            .iter()
            .enumerate()
            .map(|(position, record)| {
                u32::try_from(position)
                    .ok()
                    .filter(|position| *position < 0x70000000)
                    .map(|position| (record.id, position))
                    .ok_or(bad("commit positions"))
            })
            .collect::<Result<_, _>>()?;
        let records: Vec<_> = commits.iter().collect();
        let bytes = encode_records(format, &records, &positions, &[])?;
        let limits = GraphLimits {
            max_commits: usize::MAX,
            max_parents: usize::MAX,
            ..GraphLimits::default()
        };
        Ok(Self::parse_owned(bytes, format, limits, Checksum::Verify)?)
    }

    /// Encodes a standalone graph with generation v1/v2 and no Bloom filters.
    ///
    /// Parsing or object construction determines this graph's format. Encodes the stored claims;
    /// use [`Self::from_objects`] to establish them from objects. Does not write files.
    /// Output is bounded by [`GraphLimits::default`]'s byte limit.
    ///
    /// # Errors
    ///
    /// Rejects invalid parent closure, incompatible bases, missing corrected dates, unrepresentable
    /// positions or output exceeding the default byte budget.
    pub fn encode(&self) -> Result<Vec<u8>, GraphWriteError> {
        self.encode_incremental(&[])
    }

    /// Encodes one new split layer above exact base-file bytes supplied in base-to-tip order.
    ///
    /// Retains base positions and checks overlapping commit metadata. Base commits absent from
    /// this graph remain in the chain; only this graph's new commits are emitted. The returned
    /// file names every base checksum in `BASE`. Callers must publish those exact base files and
    /// append this file's checksum to their chain manifest. Does not change any existing file.
    /// Existing base Bloom filters remain in their files; this layer omits Bloom filters.
    ///
    /// # Errors
    ///
    /// Reports structural/checksum errors, format mismatch, too many bases, conflicting overlapping
    /// metadata, missing parent positions or corrected dates, and an oversized output. Base
    /// metadata is not verified against objects here. Use verified construction and verify bases
    /// separately when the input is untrusted.
    pub fn encode_incremental(&self, base_layers: &[&[u8]]) -> Result<Vec<u8>, GraphWriteError> {
        if base_layers.len() > 255 {
            return Err(GraphError::Limit("base layers").into());
        }
        let base = Self::parse_chain(base_layers, self.format)?;
        let mut positions = IdMap::default();
        let mut hashes = Vec::new();
        for bytes in base_layers {
            let chunks = Chunks::parse(bytes, self.format, GraphLimits::default())?;
            for raw in chunks
                .required(b"OIDL")?
                .chunks_exact(self.format.digest_len())
            {
                let id = id(self.format, raw)?;
                positions.insert(id, positions.len() as u32);
            }
            hashes.extend_from_slice(&bytes[bytes.len() - self.format.digest_len()..]);
        }
        let mut new = Vec::new();
        for record in self.commits() {
            if let Some(old) = base.get(record.id) {
                if old.tree != record.tree
                    || old.parents != record.parents
                    || old.commit_time != record.commit_time
                    || old.generation != record.generation
                    || old
                        .corrected_commit_date
                        .is_some_and(|date| Some(date) != record.corrected_commit_date)
                {
                    return Err(bad("conflicting base commit metadata").into());
                }
            } else {
                let position =
                    u32::try_from(positions.len()).map_err(|_| bad("commit positions"))?;
                if position >= 0x70000000 {
                    return Err(bad("commit positions").into());
                }
                positions.insert(record.id, position);
                new.push(record);
            }
        }
        encode_records(self.format, &new, &positions, &hashes)
    }

    /// Returns the hash format retained by this graph, including an empty graph.
    pub fn object_format(&self) -> ObjectFormat {
        self.format
    }
}

fn discover(
    id: ObjectId,
    ids: &mut Vec<ObjectId>,
    positions: &mut IdMap<usize>,
    max: usize,
) -> Result<(), HistoryError> {
    if positions.contains_key(&id) {
        return Ok(());
    }
    if ids.len() >= max {
        return Err(HistoryError::Limit("commits"));
    }
    positions.insert(id, ids.len());
    ids.push(id);
    Ok(())
}

fn set_generations(
    commits: &mut [GraphCommit],
    positions: &IdMap<usize>,
) -> Result<(), HistoryError> {
    let mut state = vec![0; commits.len()];
    for root in 0..commits.len() {
        let mut stack = vec![(root, false)];
        while let Some((index, exiting)) = stack.pop() {
            if exiting {
                let mut generation = 1;
                let mut date = commits[index].commit_time;
                for parent in &commits[index].parents {
                    let parent = &commits[positions[parent]];
                    generation =
                        generation.max(parent.generation.saturating_add(1).min(0x3fffffff));
                    date = date.max(
                        parent
                            .corrected_commit_date
                            .unwrap_or(parent.commit_time)
                            .saturating_add(1),
                    );
                }
                commits[index].generation = generation;
                commits[index].corrected_commit_date = Some(date);
                state[index] = 2;
                continue;
            }
            if state[index] == 2 {
                continue;
            }
            if state[index] == 1 {
                return Err(HistoryError::Cycle);
            }
            state[index] = 1;
            stack.push((index, true));
            for parent in commits[index].parents.iter().rev() {
                stack.push((positions[parent], false));
            }
        }
    }
    Ok(())
}

pub(super) fn encode_records(
    format: ObjectFormat,
    commits: &[&GraphCommit],
    positions: &IdMap<u32>,
    hashes: &[u8],
) -> Result<Vec<u8>, GraphWriteError> {
    let mut fanout = Vec::with_capacity(1024);
    let mut count = 0;
    for bucket in 0..256 {
        while count < commits.len() && usize::from(commits[count].id.as_bytes()[0]) <= bucket {
            count += 1;
        }
        fanout.extend_from_slice(&(count as u32).to_be_bytes());
    }
    let mut ids = Vec::new();
    let mut data = Vec::new();
    let mut edges = Vec::new();
    let mut generation = Vec::new();
    let mut overflow = Vec::new();
    for commit in commits {
        ids.extend_from_slice(commit.id.as_bytes());
        data.extend_from_slice(commit.tree.as_bytes());
        let parents: Vec<_> = commit
            .parents
            .iter()
            .map(|id| {
                positions
                    .get(id)
                    .copied()
                    .ok_or(bad("missing encoded parent"))
            })
            .collect::<Result<_, _>>()?;
        data.extend_from_slice(&parents.first().copied().unwrap_or(0x70000000).to_be_bytes());
        let second = if parents.len() > 2 {
            let start = edges.len() / 4;
            for (index, parent) in parents[1..].iter().enumerate() {
                let last = if index + 2 == parents.len() {
                    0x80000000
                } else {
                    0
                };
                edges.extend_from_slice(&(parent | last).to_be_bytes());
            }
            u32::try_from(start).map_err(|_| bad("extra edge position"))? | 0x80000000
        } else {
            parents.get(1).copied().unwrap_or(0x70000000)
        };
        data.extend_from_slice(&second.to_be_bytes());
        if commit.commit_time >= 1 << 34 || commit.generation > 0x3fffffff {
            return Err(bad("unrepresentable commit data").into());
        }
        data.extend_from_slice(
            &((commit.generation << 2) | (commit.commit_time >> 32) as u32).to_be_bytes(),
        );
        data.extend_from_slice(&(commit.commit_time as u32).to_be_bytes());
        let offset = commit
            .corrected_commit_date
            .and_then(|date| date.checked_sub(commit.commit_time))
            .ok_or(bad("missing corrected date"))?;
        let encoded = if offset <= 0x7fffffff {
            offset as u32
        } else {
            let position = u32::try_from(overflow.len() / 8)
                .map_err(|_| bad("generation overflow position"))?;
            overflow.extend_from_slice(&offset.to_be_bytes());
            position | 0x80000000
        };
        generation.extend_from_slice(&encoded.to_be_bytes());
    }
    let mut chunks = BTreeMap::from([
        (*b"OIDF", fanout),
        (*b"OIDL", ids),
        (*b"CDAT", data),
        (*b"GDA2", generation),
    ]);
    if !edges.is_empty() {
        chunks.insert(*b"EDGE", edges);
    }
    if !overflow.is_empty() {
        chunks.insert(*b"GDO2", overflow);
    }
    if !hashes.is_empty() {
        chunks.insert(*b"BASE", hashes.to_vec());
    }
    let table_end = 8 + (chunks.len() + 1) * 12;
    let size = chunks.values().try_fold(table_end, |size, data| {
        size.checked_add(data.len())
            .ok_or(GraphError::Limit("output bytes"))
    })?;
    if size.saturating_add(format.digest_len()) > GraphLimits::default().max_bytes {
        return Err(GraphError::Limit("output bytes").into());
    }
    let mut bytes = vec![
        b'C',
        b'G',
        b'P',
        b'H',
        1,
        if format == ObjectFormat::Sha1 { 1 } else { 2 },
        chunks.len() as u8,
        (hashes.len() / format.digest_len()) as u8,
    ];
    let mut offset = table_end as u64;
    for (tag, data) in &chunks {
        bytes.extend_from_slice(tag);
        bytes.extend_from_slice(&offset.to_be_bytes());
        offset += data.len() as u64;
    }
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&offset.to_be_bytes());
    for data in chunks.values() {
        bytes.extend_from_slice(data);
    }
    bytes.extend_from_slice(format.checksum(&bytes).as_bytes());
    Ok(bytes)
}
