#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]
use crate::{ObjectFormat, ObjectId, ObjectReadError as Error};

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub id: ObjectId,
    pub offset: usize,
    pub end: usize,
    pub crc: Option<u32>,
}

/// Structurally validated tables of one pack index, read from its retained bytes.
///
/// Identity order serves lookups; a separate offset order serves OFS_DELTA and entry boundaries.
/// Parsing checks every table against the others and against the pack, but not the index's own
/// trailing checksum: that is [`verify_hash`]'s job for explicit verification and byte reuse.
#[derive(Debug)]
pub(crate) struct Index {
    format: ObjectFormat,
    legacy: bool,
    /// `(offset, end)` of each entry, in identity order.
    pub ranges: Vec<(usize, usize)>,
    /// Identity-order positions sorted by pack offset.
    pub offsets: Vec<usize>,
    pub pack_hash: ObjectId,
}

impl Index {
    /// Validates the index structure against a pack whose entries end at `pack_end`.
    ///
    /// Checks the header and exact table sizes, strictly increasing identities, the fanout
    /// against those identities, each large offset slot used exactly once, and offsets that are
    /// distinct and tile the pack's entry bytes from offset 12. Each check is linear in the entry
    /// count, apart from the reverse order's radix sort.
    pub fn parse(format: ObjectFormat, bytes: &[u8], pack_end: usize) -> Result<Self, Error> {
        if bytes.len() < 8 {
            return Err(Error::Corrupt("truncated index header"));
        }
        if !bytes.starts_with(b"\xfftOc") {
            return Self::parse_v1(format, bytes, pack_end);
        }
        let version = word(bytes, 4)?;
        if version != 2 {
            return Err(Error::IndexVersion(version));
        }
        let width = format.digest_len();
        let count = word(bytes, 1028)? as usize;
        let table_end = count
            .checked_mul(width + 8)
            .and_then(|n| n.checked_add(1032))
            .ok_or(Error::Corrupt("index length overflow"))?;
        let trailer = bytes
            .len()
            .checked_sub(2 * width)
            .ok_or(Error::Corrupt("truncated index"))?;
        if table_end > trailer || (trailer - table_end) % 8 != 0 {
            return Err(Error::Corrupt("index table length"));
        }
        let table = |start, end| {
            bytes
                .get(start..end)
                .ok_or(Error::Corrupt("index table length"))
        };
        let names = table(1032, 1032 + count * width)?;
        check_names(names, width)?;
        let (large, _) = table(table_end, trailer)?.as_chunks::<8>();
        let mut used_large = vec![false; large.len()];
        if used_large.len() > count {
            return Err(Error::Corrupt("excess large offsets"));
        }
        let table = table(1032 + count * (width + 4), table_end)?;
        let mut ranges = Vec::with_capacity(count);
        for raw in table.as_chunks::<4>().0 {
            let raw = u32::from_be_bytes(*raw);
            let offset = if raw & 0x8000_0000 == 0 {
                u64::from(raw)
            } else {
                let slot = (raw & 0x7fff_ffff) as usize;
                let (Some(used), Some(large)) = (used_large.get_mut(slot), large.get(slot)) else {
                    return Err(Error::Corrupt("large offset index"));
                };
                if *used {
                    return Err(Error::Corrupt("duplicate large offset index"));
                }
                *used = true;
                u64::from_be_bytes(*large)
            };
            ranges.push((checked_offset(offset, pack_end)?, 0));
        }
        if used_large.contains(&false) {
            return Err(Error::Corrupt("unused large offset"));
        }
        Self::finish(format, bytes, (8, names), ranges, pack_end)
    }

    fn parse_v1(format: ObjectFormat, bytes: &[u8], pack_end: usize) -> Result<Self, Error> {
        let width = format.digest_len();
        let count = word(bytes, 1020)? as usize;
        let trailer = count
            .checked_mul(width + 4)
            .and_then(|n| n.checked_add(1024))
            .ok_or(Error::Corrupt("index length overflow"))?;
        if trailer.checked_add(2 * width) != Some(bytes.len()) {
            return Err(Error::Corrupt("index table length"));
        }
        let table = bytes
            .get(1024..trailer)
            .ok_or(Error::Corrupt("index table length"))?;
        let mut previous: Option<&[u8]> = None;
        let mut ranges = Vec::with_capacity(count);
        for record in table.chunks_exact(width + 4) {
            let Some((offset, name)) = record.split_first_chunk::<4>() else {
                return Err(Error::Corrupt("index table length"));
            };
            if previous.is_some_and(|previous| previous >= name) {
                return Err(Error::Corrupt("unsorted or duplicate index identities"));
            }
            previous = Some(name);
            let offset = u32::from_be_bytes(*offset);
            ranges.push((checked_offset(u64::from(offset), pack_end)?, 0));
        }
        Self::finish(format, bytes, (0, table), ranges, pack_end)
    }

    /// Checks the fanout against the identities' first bytes, then derives each entry's end
    /// from the offset order.
    ///
    /// `names` holds the identities at a fixed stride: `width` in v2, `4 + width` in v1, whose
    /// records start with the offset.
    #[expect(
        clippy::indexing_slicing,
        reason = "each record is at least 20 bytes and first_byte is 0 or 4; a u8 selects one of \
                  256 fanout buckets; offset_order returns a permutation of ranges' positions"
    )]
    fn finish(
        format: ObjectFormat,
        bytes: &[u8],
        (fanout_start, names): (usize, &[u8]),
        mut ranges: Vec<(usize, usize)>,
        pack_end: usize,
    ) -> Result<Self, Error> {
        let width = format.digest_len();
        let legacy = fanout_start == 0;
        let first_byte = usize::from(legacy) * 4;
        let mut fanout = [0u32; 256];
        if !ranges.is_empty() {
            for record in names.chunks_exact(names.len() / ranges.len()) {
                fanout[usize::from(record[first_byte])] += 1;
            }
        }
        let mut cumulative = 0;
        for (bucket, size) in fanout.into_iter().enumerate() {
            cumulative += size;
            if word(bytes, fanout_start + bucket * 4)? != cumulative {
                return Err(Error::Corrupt("index fanout"));
            }
        }
        let offsets = offset_order(&ranges);
        let mut next = pack_end;
        for &position in offsets.iter().rev() {
            let range = &mut ranges[position];
            if range.0 == next {
                return Err(Error::Corrupt("duplicate pack offset"));
            }
            range.1 = next;
            next = range.0;
        }
        if next != 12 {
            return Err(Error::Corrupt("unindexed pack bytes"));
        }
        let pack_hash = bytes
            .len()
            .checked_sub(2 * width)
            .and_then(|trailer| bytes.get(trailer..trailer + width))
            .and_then(|hash| ObjectId::from_bytes(format, hash).ok())
            .ok_or(Error::Corrupt("truncated index"))?;
        Ok(Self {
            format,
            legacy,
            ranges,
            offsets,
            pack_hash,
        })
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Identity at `position`, read from the index bytes this table was parsed from.
    #[expect(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        reason = "parse checked the name table for every position below len() in these bytes"
    )]
    pub fn identity(&self, bytes: &[u8], position: usize) -> ObjectId {
        let width = self.format.digest_len();
        let start = if self.legacy {
            1028 + position * (width + 4)
        } else {
            1032 + position * width
        };
        ObjectId::from_bytes(self.format, &bytes[start..start + width]).unwrap()
    }

    /// Entry at `position`, read from the index bytes this table was parsed from.
    #[expect(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        reason = "parse checked the CRC table for every position below len() in these bytes"
    )]
    pub fn entry(&self, bytes: &[u8], position: usize) -> Entry {
        let (offset, end) = self.ranges[position];
        let crc = (!self.legacy).then(|| {
            let start = 1032 + self.len() * self.format.digest_len() + position * 4;
            u32::from_be_bytes(bytes[start..start + 4].try_into().unwrap())
        });
        Entry {
            id: self.identity(bytes, position),
            offset,
            end,
            crc,
        }
    }

    /// Position of `id`, binary searched in the index bytes this table was parsed from.
    pub fn find(&self, bytes: &[u8], id: ObjectId) -> Option<usize> {
        let (mut low, mut high) = (0, self.len());
        while low < high {
            let middle = low + (high - low) / 2;
            match self.identity(bytes, middle).cmp(&id) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return Some(middle),
            }
        }
        None
    }

    /// Identity-order position of the entry starting at `offset`.
    #[expect(
        clippy::indexing_slicing,
        reason = "offsets holds a permutation of the positions in ranges"
    )]
    pub fn at_offset(&self, offset: usize) -> Result<usize, Error> {
        let position = self
            .offsets
            .binary_search_by_key(&offset, |&i| self.ranges[i].0)
            .map_err(|_| Error::Corrupt("base offset is not an indexed entry"))?;
        Ok(self.offsets[position])
    }
}

/// Rejects identities that are not strictly increasing, which lookups rely on.
fn check_names(names: &[u8], width: usize) -> Result<(), Error> {
    let mut names = names.chunks_exact(width);
    let Some(mut previous) = names.next() else {
        return Ok(());
    };
    for name in names {
        if previous >= name {
            return Err(Error::Corrupt("unsorted or duplicate index identities"));
        }
        previous = name;
    }
    Ok(())
}

fn checked_offset(offset: u64, pack_end: usize) -> Result<usize, Error> {
    let offset = usize::try_from(offset).map_err(|_| Error::Corrupt("offset overflow"))?;
    if offset < 12 || offset >= pack_end {
        return Err(Error::Corrupt("offset outside pack entries"));
    }
    Ok(offset)
}

/// Sorts positions by offset with an LSD radix sort over packed `offset << shift | position` keys.
///
/// Only the offset bits are sorted, in at most 16-bit digits, so a pack under 4 GiB needs two
/// passes. Equal offsets stay adjacent for the caller's duplicate check. Keys that cannot fit in
/// 64 bits fall back to a comparison sort.
#[expect(
    clippy::indexing_slicing,
    reason = "digits are masked below counts.len(), and counted slots stay below keys.len()"
)]
fn offset_order(ranges: &[(usize, usize)]) -> Vec<usize> {
    let shift = usize::BITS - ranges.len().leading_zeros();
    let largest = ranges.iter().map(|range| range.0).max().unwrap_or(0) as u64;
    let offset_bits = u64::BITS - largest.leading_zeros();
    if offset_bits + shift > u64::BITS {
        let mut order: Vec<_> = (0..ranges.len()).collect();
        order.sort_unstable_by_key(|&position| ranges[position].0);
        return order;
    }
    let mut keys: Vec<u64> = ranges
        .iter()
        .enumerate()
        .map(|(position, range)| (range.0 as u64) << shift | position as u64)
        .collect();
    let passes = offset_bits.div_ceil(16).max(1);
    let digit_bits = offset_bits.div_ceil(passes);
    let mask = (1u64 << digit_bits) - 1;
    let mut scratch = vec![0; keys.len()];
    let mut counts = vec![0usize; 1 << digit_bits];
    for pass in 0..passes {
        let digit_shift = shift + pass * digit_bits;
        let digit = |key: u64| ((key >> digit_shift) & mask) as usize;
        counts.fill(0);
        for &key in &keys {
            counts[digit(key)] += 1;
        }
        let mut start = 0;
        for count in &mut counts {
            (*count, start) = (start, start + *count);
        }
        for &key in &keys {
            let slot = &mut counts[digit(key)];
            scratch[*slot] = key;
            *slot += 1;
        }
        std::mem::swap(&mut keys, &mut scratch);
    }
    let positions = (1u64 << shift) - 1;
    keys.into_iter()
        .map(|key| (key & positions) as usize)
        .collect()
}

pub(crate) fn word(bytes: &[u8], position: usize) -> Result<u32, Error> {
    let word = bytes
        .get(position..)
        .and_then(<[u8]>::first_chunk::<4>)
        .ok_or(Error::Corrupt("truncated integer"))?;
    Ok(u32::from_be_bytes(*word))
}

pub(crate) fn verify_hash(
    format: ObjectFormat,
    bytes: &[u8],
    reason: &'static str,
) -> Result<(), Error> {
    let (content, checksum) = bytes
        .len()
        .checked_sub(format.digest_len())
        .and_then(|end| bytes.split_at_checked(end))
        .ok_or(Error::Corrupt(reason))?;
    if format.checksum(content).as_bytes() != checksum {
        return Err(Error::Corrupt(reason));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Index, offset_order};
    use crate::{ObjectFormat, ObjectReadError as Error};

    /// A one-entry SHA-1 v2 index whose entry spans pack bytes 12..13, with the given offset word.
    fn v2_index(offset: u32, large: &[u8]) -> Vec<u8> {
        let mut bytes = b"\xfftOc\0\0\0\x02".to_vec();
        for bucket in 0..256 {
            bytes.extend_from_slice(&u32::from(bucket >= 0x81).to_be_bytes());
        }
        bytes.extend_from_slice(&[0x81; 20]);
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&offset.to_be_bytes());
        bytes.extend_from_slice(large);
        bytes.extend_from_slice(&[0; 40]);
        bytes
    }

    #[test]
    fn rejects_every_truncation_of_a_valid_index() {
        let bytes = v2_index(12, &[]);
        assert!(Index::parse(ObjectFormat::Sha1, &bytes, 13).is_ok());
        for len in 0..bytes.len() {
            assert!(
                Index::parse(ObjectFormat::Sha1, &bytes[..len], 13).is_err(),
                "accepted {len} bytes"
            );
        }
    }

    #[test]
    fn rejects_large_offset_slot_outside_table() {
        let bytes = v2_index(0x8000_0001, &12u64.to_be_bytes());
        assert!(matches!(
            Index::parse(ObjectFormat::Sha1, &bytes, 13),
            Err(Error::Corrupt("large offset index"))
        ));
        let bytes = v2_index(0x8000_0000, &12u64.to_be_bytes());
        assert!(Index::parse(ObjectFormat::Sha1, &bytes, 13).is_ok());
    }

    #[test]
    fn offset_order_sorts_by_offset_at_every_key_width() {
        for offsets in [
            vec![],
            vec![12],
            vec![900, 12, 70_000, 4_000_000_000, 13],
            vec![1 << 40, 12, 1 << 33, 99],
        ] {
            let ranges: Vec<_> = offsets.iter().map(|&offset| (offset, 0)).collect();
            let mut expected: Vec<_> = (0..ranges.len()).collect();
            expected.sort_by_key(|&position| offsets[position]);
            assert_eq!(offset_order(&ranges), expected);
        }
    }

    #[test]
    fn offset_order_falls_back_when_keys_exceed_64_bits() {
        let ranges = [(usize::MAX - 1, 0), (12, 0), (usize::MAX - 2, 0)];
        assert_eq!(offset_order(&ranges), [1, 2, 0]);
    }
}
