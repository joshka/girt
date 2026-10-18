//! Indexed commit metadata from Git's auxiliary commit-graph files.
//!
//! [`CommitGraph::parse`] checks file structure and checksum, then resolves positional parents
//! to object IDs. This does not establish that objects exist or agree with the index. Keep
//! object verification separate when consuming repositories whose auxiliary metadata is untrusted.
//! Missing commits require an ordinary object lookup: a graph may cover only part of a repository.
//!
//! # Inspect a graph
//!
//! ```no_run
//! use girt::ObjectFormat;
//! use girt::commit_graph::CommitGraph;
//! let bytes = std::fs::read("repo.git/objects/info/commit-graph")?;
//! let graph = CommitGraph::parse(&bytes, ObjectFormat::Sha1)?;
//! for commit in graph.commits() {
//!     println!("{}: {} parents", commit.id, commit.parents.len());
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Parsing follows the `git help gitformat-commit-graph` format documentation. Unknown chunks are
//! ignored. Bloom filters are not interpreted by this parser. Split files require their base
//! layers through [`CommitGraph::parse_chain`].

mod store;
use std::collections::BTreeMap;

pub use store::{GraphLoadError, GraphVerifyError};

use crate::{ObjectFormat, ObjectId};

/// An immutable structurally checked single-file commit index.
#[derive(Clone, Debug)]
pub struct CommitGraph {
    commits: Vec<GraphCommit>,
}

/// Metadata claimed by a commit-graph, without object-store verification.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct GraphCommit {
    /// Commit identity, in the repository's hash format.
    pub id: ObjectId,
    /// Claimed root tree identity.
    pub tree: ObjectId,
    /// Ordered parent identities, including every octopus-merge parent.
    pub parents: Vec<ObjectId>,
    /// Unsigned 34-bit committer timestamp in seconds since the Unix epoch.
    pub commit_time: u64,
    /// Generation v1 (topological level); zero means unavailable.
    pub generation: u32,
    /// Generation v2, the commit time plus its corrected-date offset, when present.
    pub corrected_commit_date: Option<u64>,
}

/// Resource bounds applied before allocating decoded records.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct GraphLimits {
    /// Maximum complete input length, including the checksum (default 512 MiB).
    pub max_bytes: usize,
    /// Maximum number of indexed commits (default 5 million).
    pub max_commits: usize,
    /// Maximum parent occurrences across all records (default 20 million).
    pub max_parents: usize,
}

impl Default for GraphLimits {
    fn default() -> Self {
        Self {
            max_bytes: 512 * 1024 * 1024,
            max_commits: 5_000_000,
            max_parents: 20_000_000,
        }
    }
}

/// A commit-graph cannot be consumed under the selected format and bounds.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GraphError {
    /// The bytes violate a required structural invariant.
    #[error("malformed commit-graph: {0}")]
    Malformed(&'static str),
    /// The trailer does not identify the preceding bytes.
    #[error("commit-graph checksum mismatch")]
    Checksum,
    /// The file uses a different hash format from its repository.
    #[error("commit-graph hash format mismatch")]
    HashFormat,
    /// The file needs unsupported interpretation or additional base layers.
    #[error("unsupported commit-graph: {0}")]
    Unsupported(&'static str),
    /// The requested parse exceeds a caller-selected budget.
    #[error("commit-graph resource limit exceeded: {0}")]
    Limit(&'static str),
}

impl CommitGraph {
    /// Parses a standalone graph with [`GraphLimits::default`].
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] for damaged files, unsupported versions or split files, hash
    /// mismatch, checksum failure, or exhausted limits. Does not read any objects or files.
    pub fn parse(bytes: &[u8], format: ObjectFormat) -> Result<Self, GraphError> {
        Self::parse_with(bytes, format, GraphLimits::default())
    }

    /// Parses a standalone graph with explicit resource bounds.
    ///
    /// # Errors
    ///
    /// Has the same structural and compatibility errors as [`Self::parse`]. Checks input length
    /// before hashing and commit/parent budgets before allocating their decoded representations.
    pub fn parse_with(
        bytes: &[u8],
        format: ObjectFormat,
        limits: GraphLimits,
    ) -> Result<Self, GraphError> {
        let commits = parse_layer(bytes, format, limits, &[], &[], &mut { limits.max_parents })?;
        Ok(Self { commits })
    }

    /// Parses a split chain supplied in base-to-tip order with default bounds.
    ///
    /// # Errors
    ///
    /// Rejects damaged layers, missing or reordered bases, duplicate commits and exhausted
    /// aggregate bounds. An empty chain produces an empty index. Every layer must use `format`.
    pub fn parse_chain(layers: &[&[u8]], format: ObjectFormat) -> Result<Self, GraphError> {
        Self::parse_chain_with(layers, format, GraphLimits::default())
    }

    /// Parses a split chain under aggregate byte, commit and parent bounds.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::parse_chain`]. At most 256 layers are accepted because
    /// the format's base-count field has one byte. Layer bytes are retained only by the caller.
    pub fn parse_chain_with(
        layers: &[&[u8]],
        format: ObjectFormat,
        limits: GraphLimits,
    ) -> Result<Self, GraphError> {
        if layers.len() > 256 {
            return Err(GraphError::Limit("chain layers"));
        }
        let total = layers.iter().try_fold(0usize, |total, layer| {
            total
                .checked_add(layer.len())
                .ok_or(GraphError::Limit("bytes"))
        })?;
        if total > limits.max_bytes {
            return Err(GraphError::Limit("bytes"));
        }
        let mut commits = Vec::new();
        let mut ids = Vec::new();
        let mut hashes = Vec::new();
        let mut parents_left = limits.max_parents;
        for bytes in layers {
            let mut remaining = limits;
            remaining.max_commits = limits.max_commits - commits.len();
            let layer = parse_layer(bytes, format, remaining, &ids, &hashes, &mut parents_left)?;
            ids.extend(layer.iter().map(|c| c.id));
            commits.extend(layer);
            hashes.push(id(format, &bytes[bytes.len() - format.digest_len()..])?);
        }
        commits.sort_unstable_by_key(|c| c.id);
        if commits.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(bad("duplicate commit across layers"));
        }
        Ok(Self { commits })
    }

    /// Returns records in ascending object-ID order.
    pub fn commits(&self) -> &[GraphCommit] {
        &self.commits
    }

    /// Finds a record by identity; absence does not imply the object is missing.
    pub fn get(&self, id: ObjectId) -> Option<&GraphCommit> {
        self.commits
            .binary_search_by_key(&id, |c| c.id)
            .ok()
            .map(|i| &self.commits[i])
    }
}

fn parse_layer(
    bytes: &[u8],
    format: ObjectFormat,
    limits: GraphLimits,
    bases: &[ObjectId],
    hashes: &[ObjectId],
    parents_left: &mut usize,
) -> Result<Vec<GraphCommit>, GraphError> {
    let chunks = Chunks::parse(bytes, format, limits)?;
    if usize::from(bytes[7]) != hashes.len() {
        return Err(bad("base graph count"));
    }
    let base_bytes = chunks.optional(b"BASE").unwrap_or_default();
    let expected: Vec<_> = hashes
        .iter()
        .flat_map(|h: &ObjectId| h.as_bytes().iter().copied())
        .collect();
    if base_bytes != expected {
        return Err(bad("base graph identities"));
    }
    let fanout = chunks.required(b"OIDF")?;
    if fanout.len() != 1024 {
        return Err(bad("fanout length"));
    }
    let count = word(fanout, 1020)? as usize;
    if count > limits.max_commits {
        return Err(GraphError::Limit("commits"));
    }
    let ids = chunks.required(b"OIDL")?;
    let data = chunks.required(b"CDAT")?;
    let width = format.digest_len();
    if ids.len() / width != count || ids.len() % width != 0 || data.len() != count * (width + 16) {
        return Err(bad("commit table length"));
    }
    validate_fanout(fanout, ids, width)?;
    let ids: Vec<_> = ids
        .chunks_exact(width)
        .map(|bytes| id(format, bytes))
        .collect::<Result<_, _>>()?;
    let generation = chunks.optional(b"GDA2");
    if generation.is_some_and(|g| g.len() != count * 4) {
        return Err(bad("generation table length"));
    }
    let mut commits = Vec::with_capacity(count);
    for (index, record) in data.chunks_exact(width + 16).enumerate() {
        let positions = parent_positions(record, width, &chunks, parents_left)?;
        let parents = positions
            .into_iter()
            .map(|p| {
                bases
                    .get(p as usize)
                    .or_else(|| {
                        (p as usize)
                            .checked_sub(bases.len())
                            .and_then(|i| ids.get(i))
                    })
                    .copied()
                    .ok_or(bad("parent position"))
            })
            .collect::<Result<_, _>>()?;
        let high = word(record, width + 8)?;
        let commit_time = (u64::from(high & 3) << 32) | u64::from(word(record, width + 12)?);
        let corrected_commit_date = generation
            .map(|g| corrected_date(g, index, commit_time, &chunks))
            .transpose()?;
        commits.push(GraphCommit {
            id: ids[index],
            tree: id(format, &record[..width])?,
            parents,
            commit_time,
            generation: high >> 2,
            corrected_commit_date,
        });
    }
    Ok(commits)
}

struct Chunks<'a>(BTreeMap<[u8; 4], &'a [u8]>);

impl<'a> Chunks<'a> {
    fn parse(
        bytes: &'a [u8],
        format: ObjectFormat,
        limits: GraphLimits,
    ) -> Result<Self, GraphError> {
        if bytes.len() > limits.max_bytes {
            return Err(GraphError::Limit("bytes"));
        }
        if bytes.len() < 8 + format.digest_len() || bytes.get(..4) != Some(b"CGPH") {
            return Err(bad("header"));
        }
        if bytes[4] != 1 {
            return Err(GraphError::Unsupported("file version"));
        }
        let hash = match format {
            ObjectFormat::Sha1 => 1,
            ObjectFormat::Sha256 => 2,
        };
        if bytes[5] != hash {
            return Err(GraphError::HashFormat);
        }
        let end = bytes.len() - format.digest_len();
        if format.checksum(&bytes[..end]).as_bytes() != &bytes[end..] {
            return Err(GraphError::Checksum);
        }
        let count = usize::from(bytes[6]);
        let table_end = 8 + (count + 1) * 12;
        if table_end > end {
            return Err(bad("chunk table"));
        }
        let mut chunks = BTreeMap::new();
        let mut previous = table_end;
        for i in 0..=count {
            let entry = 8 + i * 12;
            let tag: [u8; 4] = bytes[entry..entry + 4]
                .try_into()
                .map_err(|_| bad("chunk id"))?;
            let offset =
                usize::try_from(long(bytes, entry + 4)?).map_err(|_| bad("chunk offset"))?;
            if offset < previous || offset > end || (i == 0 && offset != table_end) {
                return Err(bad("chunk offset"));
            }
            if i == count {
                if tag != [0; 4] || offset != end {
                    return Err(bad("chunk terminator"));
                }
            } else {
                if tag == [0; 4] {
                    return Err(bad("early chunk terminator"));
                }
                let next =
                    usize::try_from(long(bytes, entry + 16)?).map_err(|_| bad("chunk offset"))?;
                let value = bytes
                    .get(offset..next)
                    .filter(|_| next <= end)
                    .ok_or(bad("chunk bounds"))?;
                if chunks.insert(tag, value).is_some() {
                    return Err(bad("duplicate chunk"));
                }
            }
            previous = offset;
        }
        Ok(Self(chunks))
    }
    fn required(&self, tag: &[u8; 4]) -> Result<&'a [u8], GraphError> {
        self.optional(tag).ok_or(bad("missing required chunk"))
    }
    fn optional(&self, tag: &[u8; 4]) -> Option<&'a [u8]> {
        self.0.get(tag).copied()
    }
}

fn validate_fanout(fanout: &[u8], ids: &[u8], width: usize) -> Result<(), GraphError> {
    let mut position = 0;
    for bucket in 0..256 {
        while position < ids.len() / width && usize::from(ids[position * width]) <= bucket {
            position += 1;
        }
        if word(fanout, bucket * 4)? as usize != position {
            return Err(bad("fanout contents"));
        }
    }
    if ids
        .chunks_exact(width)
        .zip(ids.chunks_exact(width).skip(1))
        .any(|(a, b)| a >= b)
    {
        return Err(bad("object ID ordering"));
    }
    Ok(())
}

fn parent_positions(
    record: &[u8],
    width: usize,
    chunks: &Chunks<'_>,
    left: &mut usize,
) -> Result<Vec<u32>, GraphError> {
    let first = word(record, width)?;
    let second = word(record, width + 4)?;
    let mut parents = Vec::new();
    if first == 0x70000000 {
        if second != 0x70000000 {
            return Err(bad("second parent without first"));
        }
        return Ok(parents);
    }
    push_parent(&mut parents, first, left)?;
    if second & 0x80000000 != 0 {
        let edges = chunks.required(b"EDGE")?;
        if edges.len() % 4 != 0 {
            return Err(bad("extra edge length"));
        }
        let mut at = (second & 0x7fffffff) as usize * 4;
        loop {
            let edge = word(edges, at)?;
            push_parent(&mut parents, edge & 0x7fffffff, left)?;
            if edge & 0x80000000 != 0 {
                break;
            }
            at += 4;
        }
    } else if second != 0x70000000 {
        push_parent(&mut parents, second, left)?;
    }
    Ok(parents)
}

fn push_parent(parents: &mut Vec<u32>, parent: u32, left: &mut usize) -> Result<(), GraphError> {
    *left = left.checked_sub(1).ok_or(GraphError::Limit("parents"))?;
    parents.push(parent);
    Ok(())
}

fn corrected_date(
    generation: &[u8],
    index: usize,
    time: u64,
    chunks: &Chunks<'_>,
) -> Result<u64, GraphError> {
    let offset = word(generation, index * 4)?;
    let offset = if offset & 0x80000000 == 0 {
        u64::from(offset)
    } else {
        long(
            chunks.required(b"GDO2")?,
            (offset & 0x7fffffff) as usize * 8,
        )?
    };
    time.checked_add(offset)
        .ok_or(bad("corrected date overflow"))
}

fn id(format: ObjectFormat, bytes: &[u8]) -> Result<ObjectId, GraphError> {
    ObjectId::from_bytes(format, bytes).map_err(|_| bad("object ID width"))
}
fn word(bytes: &[u8], at: usize) -> Result<u32, GraphError> {
    let bytes = bytes
        .get(at..at.checked_add(4).ok_or(bad("integer offset"))?)
        .ok_or(bad("truncated integer"))?;
    Ok(u32::from_be_bytes(
        bytes.try_into().map_err(|_| bad("integer width"))?,
    ))
}
fn long(bytes: &[u8], at: usize) -> Result<u64, GraphError> {
    let bytes = bytes
        .get(at..at.checked_add(8).ok_or(bad("integer offset"))?)
        .ok_or(bad("truncated integer"))?;
    Ok(u64::from_be_bytes(
        bytes.try_into().map_err(|_| bad("integer width"))?,
    ))
}
fn bad(reason: &'static str) -> GraphError {
    GraphError::Malformed(reason)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    // Independent minimal graph assembled from the documented chunk format.
    fn fixture(format: ObjectFormat) -> Vec<u8> {
        let width = format.digest_len();
        let ids = vec![1; width];
        let mut fanout = vec![0; 1024];
        for bucket in 1..256 {
            fanout[bucket * 4..bucket * 4 + 4].copy_from_slice(&1u32.to_be_bytes());
        }
        let mut record = vec![2; width];
        record.extend_from_slice(&0x70000000u32.to_be_bytes());
        record.extend_from_slice(&0x70000000u32.to_be_bytes());
        record.extend_from_slice(&4u32.to_be_bytes());
        record.extend_from_slice(&10u32.to_be_bytes());
        let chunks = [(b"OIDF", fanout), (b"OIDL", ids), (b"CDAT", record)];
        let mut bytes = vec![
            b'C',
            b'G',
            b'P',
            b'H',
            1,
            if format == ObjectFormat::Sha1 { 1 } else { 2 },
            3,
            0,
        ];
        let mut offset = 56u64;
        for (tag, data) in &chunks {
            bytes.extend_from_slice(*tag);
            bytes.extend_from_slice(&offset.to_be_bytes());
            offset += data.len() as u64;
        }
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&offset.to_be_bytes());
        for (_, data) in chunks {
            bytes.extend(data);
        }
        resign(&mut bytes, format, false);
        bytes
    }

    fn resign(bytes: &mut Vec<u8>, format: ObjectFormat, replace: bool) {
        if replace {
            bytes.truncate(bytes.len() - format.digest_len());
        }
        bytes.extend_from_slice(format.checksum(bytes).as_bytes());
    }

    #[rstest]
    #[case::overlapping_chunks(31, 0)]
    #[case::bad_fanout(56 + 7, 2)]
    #[case::parent_outside_table(56 + 1024 + 20 + 20 + 3, 1)]
    fn checksum_preserving_corruption_is_rejected(#[case] offset: usize, #[case] value: u8) {
        let mut bytes = fixture(ObjectFormat::Sha1);
        bytes[offset] = value;
        resign(&mut bytes, ObjectFormat::Sha1, true);
        assert!(CommitGraph::parse(&bytes, ObjectFormat::Sha1).is_err());
    }

    #[rstest]
    fn standalone_record(#[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat) {
        let graph = CommitGraph::parse(&fixture(format), format).unwrap();
        assert_eq!(graph.commits()[0].commit_time, 10);
        assert_eq!(graph.commits()[0].generation, 1);
        assert_eq!(graph.commits()[0].corrected_commit_date, None);
    }
}
