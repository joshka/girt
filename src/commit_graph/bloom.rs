//! Conservative first-parent changed-path membership from Git-written Bloom chunks.
use std::ops::Range;

use super::{Chunks, CommitGraph, word};
use crate::ObjectId;

/// One commit's filter, borrowed from its layer.
pub(super) struct BloomFilter<'a> {
    version: u32,
    hashes: u32,
    bits: &'a [u8],
}

impl CommitGraph {
    /// Tests whether a path might differ from this commit's first parent.
    ///
    /// `Some(false)` is a negative filter answer; `Some(true)` includes false positives.
    /// `None` means no usable filter or indexed commit, an unsupported hash version/count, or
    /// a non-ASCII query against a legacy version 1 filter. Paths are repository-relative bytes,
    /// without a trailing slash. This does not implement pathspec matching or rename following.
    /// The index's claim is not checked against objects. Merge filters describe only the first
    /// parent and cannot exclude differences from other parents.
    pub fn might_change_path(&self, commit: ObjectId, path: &[u8]) -> Option<bool> {
        if path.is_empty()
            || path
                .split(|byte| *byte == b'/')
                .any(|part| part.is_empty() || part == b"." || part == b".." || part.contains(&0))
        {
            return None;
        }
        let (layer, index) = self.layer_at(self.locate(commit)?);
        let filter = layer.bloom(index)?;
        if filter.version == 1 && !path.is_ascii() {
            return None;
        }
        if filter.bits == [0] {
            return Some(false);
        }
        if filter.bits == [255] {
            return Some(true);
        }
        let first = murmur3(path, 0x293ae76f);
        let second = murmur3(path, 0x7e646e2c);
        let size = filter.bits.len() as u64 * 8;
        Some((0..filter.hashes).all(|i| {
            let hash = first.wrapping_add(i.wrapping_mul(second));
            let bit = (u64::from(hash) % size) as usize;
            filter.bits[bit / 8] & (1 << (bit % 8)) != 0
        }))
    }
}

/// Locations of a layer's usable filter chunks; unusable chunks are treated as absent.
#[derive(Clone, Debug)]
pub(super) struct BloomLayout {
    version: u32,
    hashes: u32,
    offsets: Range<usize>,
    data: Range<usize>,
}

impl BloomLayout {
    pub(super) fn parse(chunks: &Chunks<'_>, count: usize) -> Option<Self> {
        let offsets = chunks.optional(b"BIDX")?;
        let data = chunks.optional(b"BDAT")?;
        if offsets.len() != count.checked_mul(4)? || data.len() < 12 {
            return None;
        }
        let version = word(data, 0).ok()?;
        let hashes = word(data, 4).ok()?;
        if !matches!(version, 1 | 2) || !(1..=64).contains(&hashes) || word(data, 8).ok()? == 0 {
            return None;
        }
        let filters = data.len() - 12;
        let mut previous = 0;
        for index in 0..count {
            let end = word(offsets, index * 4).ok()? as usize;
            if end < previous || end > filters {
                return None;
            }
            previous = end;
        }
        let data = chunks.range(b"BDAT")?;
        Some(Self {
            version,
            hashes,
            offsets: chunks.range(b"BIDX")?,
            data: data.start + 12..data.end,
        })
    }

    /// Returns a record's filter from its layer's bytes; an empty filter is unusable.
    pub(super) fn filter<'a>(&self, bytes: &'a [u8], index: usize) -> Option<BloomFilter<'a>> {
        let offsets = &bytes[self.offsets.clone()];
        let start = if index == 0 {
            0
        } else {
            word(offsets, (index - 1) * 4).ok()? as usize
        };
        let end = word(offsets, index * 4).ok()? as usize;
        let bits = bytes[self.data.clone()].get(start..end)?;
        if bits.is_empty() {
            return None;
        }
        Some(BloomFilter {
            version: self.version,
            hashes: self.hashes,
            bits,
        })
    }
}

// MurmurHash3 x86_32 arithmetic, as specified by the format's hash version 2.
fn murmur3(bytes: &[u8], seed: u32) -> u32 {
    let mut hash = seed;
    let (blocks, tail) = bytes.as_chunks::<4>();
    for block in blocks {
        let value = u32::from_le_bytes([block[0], block[1], block[2], block[3]]);
        hash ^= mix(value);
        hash = hash
            .rotate_left(13)
            .wrapping_mul(5)
            .wrapping_add(0xe6546b64);
    }
    let mut value = 0u32;
    for (i, byte) in tail.iter().enumerate() {
        value |= u32::from(*byte) << (i * 8);
    }
    if !tail.is_empty() {
        hash ^= mix(value);
    }
    hash ^= bytes.len() as u32;
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x85ebca6b);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0xc2b2ae35);
    hash ^ (hash >> 16)
}

fn mix(value: u32) -> u32 {
    value
        .wrapping_mul(0xcc9e2d51)
        .rotate_left(15)
        .wrapping_mul(0x1b873593)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::unknown_version(99, 7, [1,2])]
    #[case::zero_hashes(2, 0, [1,2])]
    #[case::excessive_hashes(2, 65, [1,2])]
    #[case::reversed_offsets(2, 7, [2,1])]
    #[case::outside_data(2, 7, [1,3])]
    fn unusable_filters_are_unknown(
        #[case] version: u32,
        #[case] hashes: u32,
        #[case] ends: [u32; 2],
    ) {
        let mut data = version.to_be_bytes().to_vec();
        data.extend_from_slice(&hashes.to_be_bytes());
        data.extend_from_slice(&10u32.to_be_bytes());
        data.extend_from_slice(&[0, 255]);
        let offsets: Vec<_> = ends.into_iter().flat_map(u32::to_be_bytes).collect();
        let mut bytes = offsets;
        let split = bytes.len();
        bytes.extend_from_slice(&data);
        let chunks = Chunks {
            bytes: &bytes,
            ranges: BTreeMap::from([(*b"BIDX", 0..split), (*b"BDAT", split..bytes.len())]),
        };
        assert!(BloomLayout::parse(&chunks, 2).is_none());
    }
}
