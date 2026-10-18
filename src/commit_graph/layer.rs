//! One checked commit-graph file whose records are read in place.
//!
//! Parsing checks every structural claim over the whole file before a layer exists, so later
//! accessors index the retained bytes without failing. Records are never decoded eagerly.
use std::cmp::Ordering;
use std::ops::Range;

use super::bloom::{BloomFilter, BloomLayout};
use super::{Chunks, GraphCommit, GraphError, GraphLimits, bad, id, long, word};
use crate::{ObjectFormat, ObjectId};

/// A `CDAT` parent slot with no parent.
const NO_PARENT: u32 = 0x7000_0000;
/// Marks an `EDGE` index in the second parent slot, and the last entry of an `EDGE` run.
const EXTRA_EDGES: u32 = 0x8000_0000;
/// Marks a `GDA2` entry that indexes the `GDO2` overflow table.
const LARGE_OFFSET: u32 = 0x8000_0000;

/// Structurally checked bytes of one standalone file or split layer.
///
/// Parsing borrows the bytes, so their checksum can be computed concurrently; the checked layout
/// then moves to the owned bytes with [`Layer::detach`] and [`Layer::attach`].
#[derive(Clone, Debug)]
pub(super) struct Layer<B = Vec<u8>> {
    bytes: B,
    format: ObjectFormat,
    /// Chain position of this layer's first record; parent fields use chain positions.
    start: usize,
    count: usize,
    fanout: Range<usize>,
    ids: Range<usize>,
    data: Range<usize>,
    edges: Range<usize>,
    /// Corrected-date offsets, when this layer has generation v2 data.
    offsets: Option<Range<usize>>,
    overflow: Range<usize>,
    bloom: Option<BloomLayout>,
}

impl<'a> Layer<&'a [u8]> {
    /// Checks every structural claim of a file, except its checksum, and charges its parent
    /// occurrences against `parents_left`.
    ///
    /// `hashes` names the checksums of the layers below, in base-to-tip order, and `start` is the
    /// number of records they hold. Every parent position must name a record in those layers or
    /// in this one. Callers check the header with [`Chunks::header`] first and must not use the
    /// layer unless [`Chunks::checksum`] also accepts the bytes.
    pub(super) fn parse(
        bytes: &'a [u8],
        format: ObjectFormat,
        limits: GraphLimits,
        start: usize,
        hashes: &[ObjectId],
        parents_left: &mut usize,
    ) -> Result<Self, GraphError> {
        let chunks = Chunks::table(bytes, format)?;
        if usize::from(bytes[7]) != hashes.len() {
            return Err(bad("base graph count"));
        }
        let base_bytes = chunks.optional(b"BASE").unwrap_or_default();
        if !base_bytes
            .chunks(format.digest_len())
            .map(Some)
            .eq(hashes.iter().map(|hash| Some(hash.as_bytes())))
        {
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
        if ids.len() / width != count
            || ids.len() % width != 0
            || Some(data.len()) != count.checked_mul(width + 16)
        {
            return Err(bad("commit table length"));
        }
        validate_fanout(fanout, ids, width)?;
        let offsets = chunks.optional(b"GDA2");
        if offsets.is_some_and(|g| g.len() != count * 4) {
            return Err(bad("generation table length"));
        }

        let ranges = |tag| chunks.range(tag).unwrap_or_default();
        let (fanout, ids, data, edges) = (
            ranges(b"OIDF"),
            ranges(b"OIDL"),
            ranges(b"CDAT"),
            ranges(b"EDGE"),
        );
        let (offsets, overflow) = (chunks.range(b"GDA2"), ranges(b"GDO2"));
        let has_edges = chunks.range(b"EDGE").is_some();
        let bloom = BloomLayout::parse(&chunks, count);
        let layer = Self {
            bytes,
            format,
            start,
            count,
            fanout,
            ids,
            data,
            edges,
            offsets,
            overflow,
            bloom,
        };
        layer.check_records(has_edges, parents_left)?;
        Ok(layer)
    }

    /// Releases the borrowed bytes, keeping the checked layout.
    pub(super) fn detach(self) -> Layer<()> {
        Layer {
            bytes: (),
            format: self.format,
            start: self.start,
            count: self.count,
            fanout: self.fanout,
            ids: self.ids,
            data: self.data,
            edges: self.edges,
            offsets: self.offsets,
            overflow: self.overflow,
            bloom: self.bloom,
        }
    }
}

impl Layer<()> {
    /// Places a detached layout over the owned copy of the exact bytes it was parsed from.
    pub(super) fn attach(self, bytes: Vec<u8>) -> Layer {
        Layer {
            bytes,
            format: self.format,
            start: self.start,
            count: self.count,
            fanout: self.fanout,
            ids: self.ids,
            data: self.data,
            edges: self.edges,
            offsets: self.offsets,
            overflow: self.overflow,
            bloom: self.bloom,
        }
    }
}

impl<B: AsRef<[u8]>> Layer<B> {
    /// Checks every record's parent fields and corrected date without retaining anything.
    fn check_records(&self, has_edges: bool, parents_left: &mut usize) -> Result<(), GraphError> {
        // Shared EDGE runs must not amplify a small file into an unbounded decoded graph.
        // Allow at most two inline parents per record plus the encoded extra-edge entries.
        let encoded_parents = self
            .count
            .checked_mul(2)
            .and_then(|inline| inline.checked_add(self.edges.len() / 4))
            .ok_or(GraphError::Limit("parents"))?;
        let budget = (*parents_left).min(encoded_parents);
        let mut left = budget;
        let limit = self.start + self.count;
        let mut charge = |parent: u32| {
            left = left.checked_sub(1).ok_or(GraphError::Limit("parents"))?;
            if parent as usize >= limit {
                return Err(bad("parent position"));
            }
            Ok(())
        };
        let edges = &self.bytes.as_ref()[self.edges.clone()];
        for index in 0..self.count {
            let (first, second) = self.parent_fields(index);
            if first == NO_PARENT {
                if second != NO_PARENT {
                    return Err(bad("second parent without first"));
                }
            } else {
                charge(first)?;
                if second & EXTRA_EDGES != 0 {
                    if !has_edges {
                        return Err(bad("missing required chunk"));
                    }
                    if edges.len() % 4 != 0 {
                        return Err(bad("extra edge length"));
                    }
                    let mut at = ((second & !EXTRA_EDGES) as usize)
                        .checked_mul(4)
                        .ok_or(bad("extra edge offset"))?;
                    loop {
                        let edge = word(edges, at)?;
                        charge(edge & !EXTRA_EDGES)?;
                        if edge & EXTRA_EDGES != 0 {
                            break;
                        }
                        at += 4;
                    }
                } else if second != NO_PARENT {
                    charge(second)?;
                }
            }
            if let Some(offsets) = &self.offsets {
                corrected_date(
                    &self.bytes.as_ref()[offsets.clone()],
                    &self.bytes.as_ref()[self.overflow.clone()],
                    index,
                    self.commit_time(index),
                )?;
            }
        }
        *parents_left -= budget - left;
        Ok(())
    }

    pub(super) fn start(&self) -> usize {
        self.start
    }

    pub(super) fn count(&self) -> usize {
        self.count
    }

    /// Returns the checksum trailer, which names this layer in a chain.
    pub(super) fn checksum(&self) -> ObjectId {
        let bytes = self.bytes.as_ref();
        let trailer = bytes.len() - self.format.digest_len();
        ObjectId::from_bytes(self.format, &bytes[trailer..])
            .expect("trailer has the format's digest width")
    }

    pub(super) fn has_corrected_dates(&self) -> bool {
        self.offsets.is_some()
    }

    /// Finds a record's index within this layer by fanout bucket and binary search.
    pub(super) fn find(&self, id: ObjectId) -> Option<usize> {
        let id = id.as_bytes();
        let width = self.format.digest_len();
        if id.len() != width {
            return None;
        }
        let bucket = usize::from(id[0]);
        let fanout = &self.bytes.as_ref()[self.fanout.clone()];
        let mut low = if bucket == 0 {
            0
        } else {
            be32(fanout, (bucket - 1) * 4) as usize
        };
        let mut high = be32(fanout, bucket * 4) as usize;
        let ids = &self.bytes.as_ref()[self.ids.clone()];
        while low < high {
            let middle = low + (high - low) / 2;
            match ids[middle * width..(middle + 1) * width].cmp(id) {
                Ordering::Less => low = middle + 1,
                Ordering::Greater => high = middle,
                Ordering::Equal => return Some(middle),
            }
        }
        None
    }

    pub(super) fn id(&self, index: usize) -> ObjectId {
        let width = self.format.digest_len();
        let at = self.ids.start + index * width;
        ObjectId::from_bytes(self.format, &self.bytes.as_ref()[at..at + width])
            .expect("ID table entries have the format's digest width")
    }

    fn record(&self, index: usize) -> &[u8] {
        let width = self.format.digest_len() + 16;
        let at = self.data.start + index * width;
        &self.bytes.as_ref()[at..at + width]
    }

    fn parent_fields(&self, index: usize) -> (u32, u32) {
        let record = self.record(index);
        let width = self.format.digest_len();
        (be32(record, width), be32(record, width + 4))
    }

    /// Appends a record's parent chain positions in stored order.
    pub(super) fn parents_into(&self, index: usize, parents: &mut Vec<u32>) {
        let (first, second) = self.parent_fields(index);
        if first == NO_PARENT {
            return;
        }
        parents.push(first);
        if second & EXTRA_EDGES != 0 {
            let edges = &self.bytes.as_ref()[self.edges.clone()];
            let mut at = (second & !EXTRA_EDGES) as usize * 4;
            loop {
                let edge = be32(edges, at);
                parents.push(edge & !EXTRA_EDGES);
                if edge & EXTRA_EDGES != 0 {
                    break;
                }
                at += 4;
            }
        } else if second != NO_PARENT {
            parents.push(second);
        }
    }

    pub(super) fn commit_time(&self, index: usize) -> u64 {
        let record = self.record(index);
        let width = self.format.digest_len();
        let high = be32(record, width + 8);
        (u64::from(high & 3) << 32) | u64::from(be32(record, width + 12))
    }

    /// Returns generation v1, the topological level; zero means unavailable.
    pub(super) fn level(&self, index: usize) -> u32 {
        be32(self.record(index), self.format.digest_len() + 8) >> 2
    }

    pub(super) fn corrected_date(&self, index: usize) -> Option<u64> {
        let offsets = self.offsets.as_ref()?;
        let date = corrected_date(
            &self.bytes.as_ref()[offsets.clone()],
            &self.bytes.as_ref()[self.overflow.clone()],
            index,
            self.commit_time(index),
        );
        Some(date.expect("corrected dates were checked while parsing"))
    }

    pub(super) fn bloom(&self, index: usize) -> Option<BloomFilter<'_>> {
        self.bloom.as_ref()?.filter(self.bytes.as_ref(), index)
    }

    /// Decodes a record, resolving parent positions through `id_at`.
    pub(super) fn decode(&self, index: usize, id_at: impl Fn(u32) -> ObjectId) -> GraphCommit {
        let mut positions = Vec::new();
        self.parents_into(index, &mut positions);
        let width = self.format.digest_len();
        GraphCommit {
            id: self.id(index),
            tree: id(self.format, &self.record(index)[..width])
                .expect("records hold a tree ID of the format's digest width"),
            parents: positions.into_iter().map(id_at).collect(),
            commit_time: self.commit_time(index),
            generation: self.level(index),
            corrected_commit_date: self.corrected_date(index),
        }
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

fn corrected_date(
    offsets: &[u8],
    overflow: &[u8],
    index: usize,
    time: u64,
) -> Result<u64, GraphError> {
    let offset = word(offsets, index * 4)?;
    let offset = if offset & LARGE_OFFSET == 0 {
        u64::from(offset)
    } else {
        if overflow.is_empty() {
            return Err(bad("missing required chunk"));
        }
        long(
            overflow,
            ((offset & !LARGE_OFFSET) as usize)
                .checked_mul(8)
                .ok_or(bad("generation overflow offset"))?,
        )?
    };
    time.checked_add(offset)
        .ok_or(bad("corrected date overflow"))
}

/// Reads a big-endian integer from a range checked while parsing.
fn be32(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_be_bytes(word)
}
