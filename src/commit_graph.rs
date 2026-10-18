//! Indexed commit metadata from Git's auxiliary commit-graph files.
//!
//! [`CommitGraph::parse`] checks the checksum and every structural claim of the whole file, then
//! reads records from the retained bytes on demand. This does not establish that objects exist or
//! agree with the index. Keep
//! object verification separate when consuming repositories whose auxiliary metadata is untrusted.
//! Use [`crate::Objects::with_commit_graph`] to opt into indexed history queries.
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
//! ignored. Changed-path Bloom filters provide conservative first-parent membership.
//! Split files require their base
//! layers through [`CommitGraph::parse_chain`].

mod bloom;
mod history;
mod install;
mod layer;
pub use install::{GraphInstallError, GraphInstallReport, GraphWriteMode};
mod write;
pub use write::GraphWriteError;
mod store;
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::OnceLock;

use layer::Layer;
pub use store::{GraphLoadError, GraphVerifyError};

use crate::{ObjectFormat, ObjectId};

/// An immutable structurally checked commit index, standalone or a split chain.
///
/// Every structural claim is checked over the whole input before a value exists. Records are then
/// read from the retained file bytes as they are needed rather than decoded up front.
#[derive(Clone, Debug)]
pub struct CommitGraph {
    /// Base-to-tip; a record's position counts the records of every layer below it.
    layers: Vec<Layer>,
    format: ObjectFormat,
    /// Derived while parsing; attaching rejects a graph whose ancestry has no checked order.
    history: history::Prepared,
    sorted: OnceLock<Vec<GraphCommit>>,
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

/// Resource bounds applied before accepting a graph.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct GraphLimits {
    /// Maximum complete input length, including the checksum (default 512 MiB).
    pub max_bytes: usize,
    /// Maximum number of indexed commits (default 5 million).
    pub max_commits: usize,
    /// Maximum parent occurrences across all records (default 20 million).
    ///
    /// Each layer is also limited to twice its commit count plus its encoded extra-edge count,
    /// preventing repeated references to shared edge runs from amplifying decoded storage.
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
    /// Copies `bytes`; records are read from that copy on demand.
    ///
    /// # Errors
    ///
    /// Has the same structural and compatibility errors as [`Self::parse`]. Checks input length
    /// before hashing, and the commit and parent budgets before accepting the graph.
    pub fn parse_with(
        bytes: &[u8],
        format: ObjectFormat,
        limits: GraphLimits,
    ) -> Result<Self, GraphError> {
        if bytes.len() > limits.max_bytes {
            return Err(GraphError::Limit("bytes"));
        }
        Self::parse_owned(bytes.to_vec(), format, limits)
    }

    fn parse_owned(
        bytes: Vec<u8>,
        format: ObjectFormat,
        limits: GraphLimits,
    ) -> Result<Self, GraphError> {
        if bytes.get(7).is_some_and(|bases| *bases != 0) {
            Chunks::parse(&bytes, format, limits)?;
            return Err(GraphError::Unsupported("split file requires bases"));
        }
        Self::parse_files(vec![bytes], format, limits)
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
    /// the format's base-count field has one byte. Layer bytes are copied; records are read from
    /// the copies on demand.
    pub fn parse_chain_with(
        layers: &[&[u8]],
        format: ObjectFormat,
        limits: GraphLimits,
    ) -> Result<Self, GraphError> {
        check_chain_bounds(layers.iter().map(|layer| layer.len()), layers.len(), limits)?;
        let owned = layers.iter().map(|layer| layer.to_vec()).collect();
        Self::parse_chain_owned(owned, format, limits)
    }

    fn parse_chain_owned(
        files: Vec<Vec<u8>>,
        format: ObjectFormat,
        limits: GraphLimits,
    ) -> Result<Self, GraphError> {
        check_chain_bounds(files.iter().map(Vec::len), files.len(), limits)?;
        Self::parse_files(files, format, limits)
    }

    /// Checks base-to-tip files completely, hashing large inputs on a second thread.
    ///
    /// The structural checks and the history index do not depend on the checksum, so they run
    /// while it is computed. Nothing is returned until both have finished, and a checksum
    /// mismatch is reported in preference to any structural error found in the same bytes.
    fn parse_files(
        files: Vec<Vec<u8>>,
        format: ObjectFormat,
        limits: GraphLimits,
    ) -> Result<Self, GraphError> {
        for bytes in &files {
            Chunks::header(bytes, format, limits)?;
        }
        let checksums = || {
            files
                .iter()
                .try_for_each(|bytes| Chunks::checksum(bytes, format))
        };
        let total: usize = files.iter().map(Vec::len).sum();
        let (layouts, history) = std::thread::scope(|scope| {
            let hashing = (total >= CONCURRENT_CHECKSUM_BYTES)
                .then(|| {
                    std::thread::Builder::new()
                        .spawn_scoped(scope, checksums)
                        .ok()
                })
                .flatten();
            if hashing.is_none() {
                checksums()?;
            }
            let structure = check_structure(&files, format, limits);
            let hashed = hashing.map_or(Ok(()), |hashing| {
                hashing
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            });
            hashed.and(structure)
        })?;
        let layers = layouts
            .into_iter()
            .zip(files)
            .map(|(layout, bytes)| layout.attach(bytes));
        Ok(Self {
            layers: layers.collect(),
            format,
            history,
            sorted: OnceLock::new(),
        })
    }

    /// Returns records in ascending object-ID order.
    ///
    /// The first call decodes and retains every record; prefer [`Self::get`] for a few commits.
    pub fn commits(&self) -> &[GraphCommit] {
        self.sorted.get_or_init(|| {
            let mut commits: Vec<_> = (0..self.len())
                .map(|position| self.record(position))
                .collect();
            if self.layers.len() > 1 {
                commits.sort_unstable_by_key(|commit| commit.id);
            }
            commits
        })
    }

    /// Decodes a record by identity; absence does not imply the object is missing.
    pub fn get(&self, id: ObjectId) -> Option<GraphCommit> {
        self.locate(id).map(|position| self.record(position))
    }

    /// Returns the number of indexed commits across every layer.
    fn len(&self) -> usize {
        self.layers
            .last()
            .map_or(0, |layer| layer.start() + layer.count())
    }

    /// Finds a record's chain position, whatever generation order was checked.
    fn locate(&self, id: ObjectId) -> Option<usize> {
        self.layers
            .iter()
            .find_map(|layer| layer.find(id).map(|index| layer.start() + index))
    }

    /// Returns the layer holding a chain position and the record's index within it.
    fn layer_at(&self, position: usize) -> (&Layer, usize) {
        let layer = self
            .layers
            .partition_point(|layer| layer.start() <= position)
            - 1;
        let layer = &self.layers[layer];
        (layer, position - layer.start())
    }

    fn record(&self, position: usize) -> GraphCommit {
        let (layer, index) = self.layer_at(position);
        layer.decode(index, |parent| self.id_at(parent))
    }
}

/// Inputs at least this large are hashed concurrently with their structural checks.
const CONCURRENT_CHECKSUM_BYTES: usize = 1 << 20;

/// Checks every layer's structure and cross-layer uniqueness, then derives the history index.
fn check_structure(
    files: &[Vec<u8>],
    format: ObjectFormat,
    limits: GraphLimits,
) -> Result<(Vec<Layer<()>>, history::Prepared), GraphError> {
    let mut layers: Vec<Layer<&[u8]>> = Vec::with_capacity(files.len());
    let mut hashes = Vec::new();
    let mut commits = 0;
    let mut parents_left = limits.max_parents;
    for bytes in files {
        let mut remaining = limits;
        remaining.max_commits = limits.max_commits - commits;
        let layer = Layer::parse(
            bytes,
            format,
            remaining,
            commits,
            &hashes,
            &mut parents_left,
        )?;
        // Sorted, unique IDs within each layer leave only cross-layer duplicates to find.
        if (0..layer.count()).any(|index| {
            let id = layer.id(index);
            layers.iter().any(|base| base.find(id).is_some())
        }) {
            return Err(bad("duplicate commit across layers"));
        }
        commits += layer.count();
        hashes.push(layer.checksum());
        layers.push(layer);
    }
    let history = history::prepare(&layers);
    Ok((layers.into_iter().map(Layer::detach).collect(), history))
}

fn check_chain_bounds(
    lengths: impl Iterator<Item = usize>,
    count: usize,
    limits: GraphLimits,
) -> Result<(), GraphError> {
    if count > 256 {
        return Err(GraphError::Limit("chain layers"));
    }
    let mut total = 0usize;
    for length in lengths {
        total = total
            .checked_add(length)
            .ok_or(GraphError::Limit("bytes"))?;
    }
    if total > limits.max_bytes {
        return Err(GraphError::Limit("bytes"));
    }
    Ok(())
}

/// The chunk table of one file, located within its bytes.
struct Chunks<'a> {
    bytes: &'a [u8],
    ranges: BTreeMap<[u8; 4], Range<usize>>,
}

impl<'a> Chunks<'a> {
    /// Checks the header and checksum, then locates the chunks.
    fn parse(
        bytes: &'a [u8],
        format: ObjectFormat,
        limits: GraphLimits,
    ) -> Result<Self, GraphError> {
        Self::header(bytes, format, limits)?;
        Self::checksum(bytes, format)?;
        Self::table(bytes, format)
    }

    /// Checks the length bound, signature, version and hash format.
    fn header(bytes: &[u8], format: ObjectFormat, limits: GraphLimits) -> Result<(), GraphError> {
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
        Ok(())
    }

    /// Checks that the trailer identifies the preceding bytes.
    fn checksum(bytes: &[u8], format: ObjectFormat) -> Result<(), GraphError> {
        let end = bytes
            .len()
            .checked_sub(format.digest_len())
            .ok_or(bad("header"))?;
        if format.checksum(&bytes[..end]).as_bytes() != &bytes[end..] {
            return Err(GraphError::Checksum);
        }
        Ok(())
    }

    /// Locates the chunks of a file whose header was checked; does not hash it.
    fn table(bytes: &'a [u8], format: ObjectFormat) -> Result<Self, GraphError> {
        let end = bytes.len() - format.digest_len();
        let count = usize::from(bytes[6]);
        let table_end = 8 + (count + 1) * 12;
        if table_end > end {
            return Err(bad("chunk table"));
        }
        let mut ranges = BTreeMap::new();
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
                if next < offset || next > end {
                    return Err(bad("chunk bounds"));
                }
                if ranges.insert(tag, offset..next).is_some() {
                    return Err(bad("duplicate chunk"));
                }
            }
            previous = offset;
        }
        Ok(Self { bytes, ranges })
    }
    fn required(&self, tag: &[u8; 4]) -> Result<&'a [u8], GraphError> {
        self.optional(tag).ok_or(bad("missing required chunk"))
    }
    fn optional(&self, tag: &[u8; 4]) -> Option<&'a [u8]> {
        self.range(tag).map(|range| &self.bytes[range])
    }
    fn range(&self, tag: &[u8; 4]) -> Option<Range<usize>> {
        self.ranges.get(tag).cloned()
    }
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

    #[test]
    fn shared_extra_edges_cannot_amplify_decoded_parents() {
        let edges: Vec<_> = [0u32, 0, 0, 0x80000000]
            .into_iter()
            .flat_map(u32::to_be_bytes)
            .collect();
        let mut fanout = vec![0; 1024];
        for bucket in 1..256 {
            let count = if bucket == 1 { 1u32 } else { 2u32 };
            fanout[bucket * 4..bucket * 4 + 4].copy_from_slice(&count.to_be_bytes());
        }
        let mut ids = vec![1; 20];
        ids.extend_from_slice(&[2; 20]);
        let mut record = vec![0; 20];
        record.extend_from_slice(&0u32.to_be_bytes());
        record.extend_from_slice(&0x80000000u32.to_be_bytes());
        record.extend_from_slice(&[0; 8]);
        let data = record.repeat(2);
        let chunks = [
            (b"OIDF", fanout),
            (b"OIDL", ids),
            (b"CDAT", data),
            (b"EDGE", edges),
        ];
        let mut bytes = b"CGPH\x01\x01\x04\x00".to_vec();
        let mut offset = 68u64;
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
        resign(&mut bytes, ObjectFormat::Sha1, false);
        // Two records share four encoded edges, attempting ten decoded parents.
        assert!(matches!(
            CommitGraph::parse(&bytes, ObjectFormat::Sha1),
            Err(GraphError::Limit("parents"))
        ));
    }

    #[test]
    fn standalone_split_layer_is_unsupported() {
        let mut bytes = fixture(ObjectFormat::Sha1);
        bytes[7] = 1;
        resign(&mut bytes, ObjectFormat::Sha1, true);
        assert!(matches!(
            CommitGraph::parse(&bytes, ObjectFormat::Sha1),
            Err(GraphError::Unsupported("split file requires bases"))
        ));
    }

    #[rstest]
    fn standalone_record(#[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat) {
        let graph = CommitGraph::parse(&fixture(format), format).unwrap();
        assert_eq!(graph.commits()[0].commit_time, 10);
        assert_eq!(graph.commits()[0].generation, 1);
        assert_eq!(graph.commits()[0].corrected_commit_date, None);
    }

    // A linear history large enough to be hashed concurrently with its structural checks.
    fn large_fixture() -> Vec<u8> {
        let format = ObjectFormat::Sha1;
        let count = 20_000u32;
        let ids: Vec<_> = (0..count)
            .map(|i| format.checksum(&i.to_be_bytes()))
            .collect();
        let mut commits: Vec<_> = (0..count as usize)
            .map(|i| GraphCommit {
                id: ids[i],
                tree: ids[i],
                parents: i.checked_sub(1).map(|p| ids[p]).into_iter().collect(),
                commit_time: 1_000 + i as u64,
                generation: i as u32 + 1,
                corrected_commit_date: Some(1_000 + i as u64),
            })
            .collect();
        commits.sort_unstable_by_key(|commit| commit.id);
        let positions = commits
            .iter()
            .enumerate()
            .map(|(position, commit)| (commit.id, position as u32))
            .collect();
        let records: Vec<_> = commits.iter().collect();
        let bytes = write::encode_records(format, &records, &positions, &[]).unwrap();
        assert!(bytes.len() >= CONCURRENT_CHECKSUM_BYTES);
        bytes
    }

    // Points the first record's first parent outside the table.
    fn misplace_first_parent(bytes: &mut [u8]) {
        let chunks = Chunks::table(bytes, ObjectFormat::Sha1).unwrap();
        let at = chunks.range(b"CDAT").unwrap().start + 20;
        bytes[at..at + 4].copy_from_slice(&0x6fff_ffffu32.to_be_bytes());
    }

    #[test]
    fn large_graph_is_checked_completely() {
        let graph = CommitGraph::parse(&large_fixture(), ObjectFormat::Sha1).unwrap();
        assert_eq!(graph.commits().len(), 20_000);
        assert!(graph.position(graph.commits()[0].id).is_some());
    }

    #[rstest]
    #[case::checksum_reported_first(false, GraphError::Checksum)]
    #[case::structure_checked_beside_checksum(true, bad("parent position"))]
    fn large_graph_corruption_is_rejected(#[case] resigned: bool, #[case] expected: GraphError) {
        let mut bytes = large_fixture();
        misplace_first_parent(&mut bytes);
        if resigned {
            resign(&mut bytes, ObjectFormat::Sha1, true);
        }
        let error = CommitGraph::parse(&bytes, ObjectFormat::Sha1).unwrap_err();
        assert_eq!(error.to_string(), expected.to_string());
    }

    #[test]
    fn empty_base_layer_without_dates_keeps_corrected_order() {
        let format = ObjectFormat::Sha1;
        let chunks = [
            (b"OIDF", vec![0; 1024]),
            (b"OIDL", vec![]),
            (b"CDAT", vec![]),
        ];
        let mut base = b"CGPH\x01\x01\x03\x00".to_vec();
        let mut offset = 56u64;
        for (tag, data) in &chunks {
            base.extend_from_slice(*tag);
            base.extend_from_slice(&offset.to_be_bytes());
            offset += data.len() as u64;
        }
        base.extend_from_slice(&[0; 4]);
        base.extend_from_slice(&offset.to_be_bytes());
        base.extend_from_slice(&[0; 1024]);
        resign(&mut base, format, false);
        // Generation v1 is zero, so only corrected dates can order this layer's edge.
        let ids = [format.checksum(b"parent"), format.checksum(b"child")];
        let mut commits: Vec<_> = (0..2)
            .map(|i| GraphCommit {
                id: ids[i],
                tree: ids[i],
                parents: ids[..i].to_vec(),
                commit_time: 10,
                generation: 0,
                corrected_commit_date: Some(10 + i as u64),
            })
            .collect();
        commits.sort_unstable_by_key(|commit| commit.id);
        let positions = commits
            .iter()
            .enumerate()
            .map(|(position, commit)| (commit.id, position as u32))
            .collect();
        let records: Vec<_> = commits.iter().collect();
        let base_hash = &base[base.len() - 20..];
        let tip = write::encode_records(format, &records, &positions, base_hash).unwrap();

        let graph = CommitGraph::parse_chain(&[&base, &tip], format).unwrap();

        assert_eq!(graph.generation(ids[1]), Some(11));
    }

    #[test]
    fn cyclic_graph_parses_but_is_not_attached() {
        let mut bytes = fixture(ObjectFormat::Sha1);
        // The only record names itself as its parent; its generation cannot order that edge.
        bytes[56 + 1024 + 20 + 20..56 + 1024 + 20 + 24].copy_from_slice(&0u32.to_be_bytes());
        resign(&mut bytes, ObjectFormat::Sha1, true);
        let graph = CommitGraph::parse(&bytes, ObjectFormat::Sha1).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let objects = crate::Objects::open(
            ObjectFormat::Sha1,
            directory.path(),
            crate::PackLimits::default(),
            crate::AlternateLimits::default(),
        )
        .unwrap();
        assert_eq!(
            objects.with_commit_graph(graph).unwrap_err().to_string(),
            "malformed commit-graph: cyclic indexed ancestry"
        );
    }
}
