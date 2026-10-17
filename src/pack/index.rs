#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]
use std::sync::OnceLock;

use crate::{ObjectFormat, ObjectId, ObjectReadError as Error};

/// An entry with its exact extent, from the index's offset order.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub id: ObjectId,
    pub offset: usize,
    pub end: usize,
    pub crc: Option<u32>,
}

impl Entry {
    /// This entry's exact span.
    pub fn span(&self) -> Span {
        Span {
            offset: self.offset,
            end: self.end,
            exact: true,
            crc: self.crc,
            position: None,
        }
    }
}

/// Where a pack entry's bytes start and how far a reader may look for its end.
///
/// When `exact` is false, `end` only bounds the entry, and the entry ends where its zlib stream
/// does. `position` and `crc` are known for entries looked up by identity, and for entries found
/// by offset once the offset order exists.
#[derive(Debug, Clone)]
pub(crate) struct Span {
    pub offset: usize,
    pub end: usize,
    pub exact: bool,
    pub crc: Option<u32>,
    pub position: Option<usize>,
}

/// One pack index, read in place from its bytes.
///
/// Parsing checks only what every lookup relies on, as Git does when it opens an index: the
/// header, the table sizes the entry count implies, and a monotonic fanout that ends at the entry
/// count. Lookups check the fields they read, so damage to a name or offset fails the read that
/// meets it rather than the open. The offset order, which gives each entry's exact end and maps
/// an offset back to its entry, is built and fully checked on first use; content reads don't
/// need it. [`Index::validate`] checks the whole structure for explicit verification. Neither
/// checks the index's own trailing checksum: that is [`verify_hash`]'s job.
#[derive(Debug)]
pub(crate) struct Index {
    format: ObjectFormat,
    legacy: bool,
    count: usize,
    /// Number of whole 8-byte large offset slots in a v2 index.
    large: usize,
    /// Whether bytes beyond whole large offset slots precede the trailer.
    stray: bool,
    /// Where the entries end: the pack trailer's offset.
    pack_end: usize,
    pub pack_hash: ObjectId,
    order: OnceLock<Result<Order, &'static str>>,
}

/// Identity-order positions sorted by pack offset, and the rank of each position in that order.
#[derive(Debug)]
struct Order {
    order: Vec<u32>,
    rank: Vec<u32>,
}

impl Index {
    /// Checks the header, table sizes and fanout of an index for a pack whose entries end at
    /// `pack_end`, in constant time, accepting what Git accepts when it opens an index.
    pub fn parse(format: ObjectFormat, bytes: &[u8], pack_end: usize) -> Result<Self, Error> {
        if bytes.len() < 8 {
            return Err(Error::Corrupt("truncated index header"));
        }
        let width = format.digest_len();
        let legacy = !bytes.starts_with(b"\xfftOc");
        let fanout = if legacy {
            0
        } else {
            let version = word(bytes, 4)?;
            if version != 2 {
                return Err(Error::IndexVersion(version));
            }
            8
        };
        let count = word(bytes, fanout + 1020)? as usize;
        let trailer = bytes
            .len()
            .checked_sub(2 * width)
            .ok_or(Error::Corrupt("truncated index"))?;
        let record = if legacy { width + 4 } else { width + 8 };
        let table_end = count
            .checked_mul(record)
            .and_then(|n| n.checked_add(fanout + 1024))
            .ok_or(Error::Corrupt("index length overflow"))?;
        // As in Git, the large offset table may hold up to one slot fewer than there are entries,
        // since the first entry never needs one; bytes beyond whole slots are left to validate().
        let large_bytes = trailer
            .checked_sub(table_end)
            .ok_or(Error::Corrupt("index table length"))?;
        if large_bytes > count.saturating_sub(1).saturating_mul(8) || legacy && large_bytes != 0 {
            return Err(Error::Corrupt("index table length"));
        }
        let large = large_bytes / 8;
        let mut previous = 0;
        for bucket in 0..256 {
            let cumulative = word(bytes, fanout + bucket * 4)?;
            if cumulative < previous {
                return Err(Error::Corrupt("index fanout"));
            }
            previous = cumulative;
        }
        let pack_hash = bytes
            .get(trailer..trailer + width)
            .and_then(|hash| ObjectId::from_bytes(format, hash).ok())
            .ok_or(Error::Corrupt("truncated index"))?;
        Ok(Self {
            format,
            legacy,
            count,
            large,
            stray: large_bytes % 8 != 0,
            pack_end,
            pack_hash,
            order: OnceLock::new(),
        })
    }

    pub fn len(&self) -> usize {
        self.count
    }

    #[cfg(test)]
    pub fn has_order(&self) -> bool {
        self.order.get().is_some()
    }

    fn fanout_start(&self) -> usize {
        if self.legacy { 0 } else { 8 }
    }

    /// Start of the identity at `position`, which must be below `len()`.
    fn name_start(&self, position: usize) -> usize {
        let width = self.format.digest_len();
        if self.legacy {
            1028 + position * (width + 4)
        } else {
            1032 + position * width
        }
    }

    /// Identity at `position`, which must be below `len()`.
    #[expect(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        reason = "parse checked that the name table holds len() names in these bytes"
    )]
    pub fn identity(&self, bytes: &[u8], position: usize) -> ObjectId {
        let start = self.name_start(position);
        ObjectId::from_bytes(self.format, &bytes[start..start + self.format.digest_len()]).unwrap()
    }

    /// Position of `id`, binary searched within its fanout bucket, as Git does.
    ///
    /// Out-of-order names make some lookups miss, as in Git: lookups trust the order, and only
    /// [`Self::validate`] checks it.
    #[expect(
        clippy::indexing_slicing,
        reason = "parse checked that the fanout is monotonic and ends at len(), so every bucket \
                  lies within the name table"
    )]
    pub fn find(&self, bytes: &[u8], id: ObjectId) -> Result<Option<usize>, Error> {
        let width = self.format.digest_len();
        let target = id.as_bytes();
        let first = usize::from(target[0]);
        let bound = |bucket: usize| word(bytes, self.fanout_start() + bucket * 4);
        let mut low = if first == 0 { 0 } else { bound(first - 1)? } as usize;
        let mut high = bound(first)? as usize;
        while low < high {
            let middle = low + (high - low) / 2;
            let start = self.name_start(middle);
            match bytes[start..start + width].cmp(target) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return Ok(Some(middle)),
            }
        }
        Ok(None)
    }

    /// Range of positions whose identities match `prefix`, binary searched within its fanout
    /// bucket. Like [`Self::find`], it trusts the name order that only [`Self::validate`] checks.
    pub(crate) fn prefix_range(
        &self,
        bytes: &[u8],
        prefix: crate::ObjectIdPrefix,
    ) -> Result<std::ops::Range<usize>, Error> {
        let first = usize::from(
            *prefix
                .lower()
                .as_bytes()
                .first()
                .ok_or(Error::Corrupt("empty prefix"))?,
        );
        let fanout = self.fanout_start();
        let mut low = if first == 0 {
            0
        } else {
            word(bytes, fanout + (first - 1) * 4)? as usize
        };
        let mut high = word(bytes, fanout + first * 4)? as usize;
        let end = high;
        while low < high {
            let middle = low + (high - low) / 2;
            if self.identity(bytes, middle) < prefix.lower() {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let start = low;
        high = end;
        while low < high {
            let middle = low + (high - low) / 2;
            if prefix.matches(self.identity(bytes, middle)) {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Ok(start..low)
    }

    /// Pack offset of the entry at `position`, which must be below `len()`, checked to lie in
    /// the pack's entry bytes.
    pub fn offset(&self, bytes: &[u8], position: usize) -> Result<usize, Error> {
        let width = self.format.digest_len();
        if self.legacy {
            let offset = word(bytes, 1024 + position * (width + 4))?;
            return checked_offset(u64::from(offset), self.pack_end);
        }
        let table = 1032 + self.count * (width + 4);
        let raw = word(bytes, table + position * 4)?;
        if raw & 0x8000_0000 == 0 {
            return checked_offset(u64::from(raw), self.pack_end);
        }
        let slot = (raw & 0x7fff_ffff) as usize;
        if slot >= self.large {
            return Err(Error::Corrupt("large offset index"));
        }
        let start = table + self.count * 4 + slot * 8;
        let large = bytes
            .get(start..)
            .and_then(<[u8]>::first_chunk::<8>)
            .ok_or(Error::Corrupt("large offset index"))?;
        checked_offset(u64::from_be_bytes(*large), self.pack_end)
    }

    fn crc(&self, bytes: &[u8], position: usize) -> Result<Option<u32>, Error> {
        if self.legacy {
            return Ok(None);
        }
        let start = 1032 + self.count * self.format.digest_len() + position * 4;
        word(bytes, start).map(Some)
    }

    /// The entry at `position`, below `len()`, bounded exactly if the offset order exists and by
    /// the pack's entry bytes otherwise.
    pub fn span(&self, bytes: &[u8], position: usize) -> Result<Span, Error> {
        let offset = self.offset(bytes, position)?;
        let end = match self.order.get() {
            Some(Ok(order)) => Some(self.end(bytes, order, position)?),
            _ => None,
        };
        Ok(Span {
            offset,
            end: end.unwrap_or(self.pack_end),
            exact: end.is_some(),
            crc: self.crc(bytes, position)?,
            position: Some(position),
        })
    }

    /// The entry starting at `offset`, such as a delta base. Without the offset order, nothing
    /// shows that an entry starts there; the entry's own framing and checks still apply.
    pub fn span_at(&self, bytes: &[u8], offset: usize) -> Result<Span, Error> {
        if let Some(Ok(_)) = self.order.get() {
            let position = self.at_offset(bytes, offset)?;
            return self.span(bytes, position);
        }
        Ok(Span {
            offset: checked_offset(offset as u64, self.pack_end)?,
            end: self.pack_end,
            exact: false,
            crc: None,
            position: None,
        })
    }

    /// The entry at `position`, below `len()`, with its exact end. Builds the offset order.
    pub fn entry(&self, bytes: &[u8], position: usize) -> Result<Entry, Error> {
        let order = self.order(bytes)?;
        Ok(Entry {
            id: self.identity(bytes, position),
            offset: self.offset(bytes, position)?,
            end: self.end(bytes, order, position)?,
            crc: self.crc(bytes, position)?,
        })
    }

    fn end(&self, bytes: &[u8], order: &Order, position: usize) -> Result<usize, Error> {
        let rank = order
            .rank
            .get(position)
            .ok_or(Error::Corrupt("index position"))?;
        match order.order.get(*rank as usize + 1) {
            Some(&next) => self.offset(bytes, next as usize),
            None => Ok(self.pack_end),
        }
    }

    /// Identity-order position of the entry starting at `offset`. Builds the offset order.
    pub fn at_offset(&self, bytes: &[u8], offset: usize) -> Result<usize, Error> {
        let order = &self.order(bytes)?.order;
        let mut search = Ok(());
        let rank = order.binary_search_by(|&position| {
            match self.offset(bytes, position as usize) {
                Ok(found) => found.cmp(&offset),
                // The order was built from these offsets, so this cannot fail.
                Err(error) => {
                    search = Err(error);
                    std::cmp::Ordering::Equal
                }
            }
        });
        search?;
        rank.ok()
            .and_then(|rank| order.get(rank))
            .map(|&position| position as usize)
            .ok_or(Error::Corrupt("base offset is not an indexed entry"))
    }

    /// Identity-order positions in pack offset order. Builds the offset order.
    pub fn by_offset(&self, bytes: &[u8]) -> Result<impl Iterator<Item = usize> + '_, Error> {
        Ok(self
            .order(bytes)?
            .order
            .iter()
            .map(|&position| position as usize))
    }

    /// Checks the whole structure: a large offset table of whole slots, strictly increasing
    /// identities, a fanout that matches them, and the offset order's checks.
    pub fn validate(&self, bytes: &[u8]) -> Result<(), Error> {
        if self.stray {
            return Err(Error::Corrupt("index table length"));
        }
        let width = self.format.digest_len();
        let stride = if self.legacy { width + 4 } else { width };
        let names = bytes
            .get(self.name_start(0)..)
            .ok_or(Error::Corrupt("index table length"))?;
        let mut fanout = [0u32; 256];
        let mut previous: Option<&[u8]> = None;
        for position in 0..self.count {
            let name = names
                .get(position * stride..position * stride + width)
                .ok_or(Error::Corrupt("index table length"))?;
            if previous.is_some_and(|previous| previous >= name) {
                return Err(Error::Corrupt("unsorted or duplicate index identities"));
            }
            previous = Some(name);
            if let Some(bucket) = name
                .first()
                .and_then(|&first| fanout.get_mut(usize::from(first)))
            {
                *bucket += 1;
            }
        }
        let mut cumulative = 0;
        for (bucket, size) in fanout.into_iter().enumerate() {
            cumulative += size;
            if word(bytes, self.fanout_start() + bucket * 4)? != cumulative {
                return Err(Error::Corrupt("index fanout"));
            }
        }
        self.order(bytes).map(|_| ())
    }

    /// The offset order, built once from every offset.
    ///
    /// Checks each offset as [`Self::offset`] does, each large offset slot used exactly once,
    /// and offsets that are distinct and tile the pack's entry bytes from offset 12. Each check is
    /// linear in the entry count, apart from the radix sort.
    fn order(&self, bytes: &[u8]) -> Result<&Order, Error> {
        self.order
            .get_or_init(|| self.build_order(bytes))
            .as_ref()
            .map_err(|&reason| Error::Corrupt(reason))
    }

    #[expect(
        clippy::indexing_slicing,
        reason = "offset_order returns a permutation of offsets' positions"
    )]
    fn build_order(&self, bytes: &[u8]) -> Result<Order, &'static str> {
        let reason = |error| match error {
            Error::Corrupt(reason) => reason,
            _ => "index table length",
        };
        let mut used_large = vec![false; self.large];
        let mut offsets = Vec::with_capacity(self.count);
        let table = 1032 + self.count * (self.format.digest_len() + 4);
        for position in 0..self.count {
            if !self.legacy {
                let raw = word(bytes, table + position * 4).map_err(reason)?;
                if raw & 0x8000_0000 != 0 {
                    let slot = (raw & 0x7fff_ffff) as usize;
                    let used = used_large.get_mut(slot).ok_or("large offset index")?;
                    if *used {
                        return Err("duplicate large offset index");
                    }
                    *used = true;
                }
            }
            offsets.push(self.offset(bytes, position).map_err(reason)? as u64);
        }
        if used_large.contains(&false) {
            return Err("unused large offset");
        }
        let (order, sorted) = offset_order(offsets);
        if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("duplicate pack offset");
        }
        // Each entry ends where the next begins, so the first must start right after the header.
        if sorted.first().map_or(self.pack_end as u64, |&first| first) != 12 {
            return Err("unindexed pack bytes");
        }
        drop(sorted);
        let mut rank = vec![0; order.len()];
        for (index, &position) in order.iter().enumerate() {
            rank[position as usize] = index as u32;
        }
        Ok(Order { order, rank })
    }
}

fn checked_offset(offset: u64, pack_end: usize) -> Result<usize, Error> {
    let offset = usize::try_from(offset).map_err(|_| Error::Corrupt("offset overflow"))?;
    if offset < 12 || offset >= pack_end {
        return Err(Error::Corrupt("offset outside pack entries"));
    }
    Ok(offset)
}

/// Sorts identity-order positions by offset with an LSD radix sort over packed
/// `offset << shift | position` keys, returning the positions and the sorted offsets.
///
/// Only the offset bits are sorted, in at most 16-bit digits, so a pack under 4 GiB needs two
/// passes. Equal offsets stay adjacent for the caller's duplicate check. Keys that cannot fit in
/// 64 bits fall back to a comparison sort. Positions fit in `u32`: both index versions store the
/// entry count in 32 bits.
#[expect(
    clippy::indexing_slicing,
    reason = "digits are masked below counts.len(), counted slots stay below keys.len(), and the \
              fallback sorts positions below offsets.len()"
)]
fn offset_order(offsets: Vec<u64>) -> (Vec<u32>, Vec<u64>) {
    let shift = usize::BITS - offsets.len().leading_zeros();
    let largest = offsets.iter().copied().max().unwrap_or(0);
    let offset_bits = u64::BITS - largest.leading_zeros();
    if offset_bits + shift > u64::BITS {
        let mut order: Vec<u32> = (0..offsets.len() as u32).collect();
        order.sort_unstable_by_key(|&position| offsets[position as usize]);
        let sorted = order
            .iter()
            .map(|&position| offsets[position as usize])
            .collect();
        return (order, sorted);
    }
    let mut keys = offsets;
    for (position, key) in keys.iter_mut().enumerate() {
        *key = *key << shift | position as u64;
    }
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
    drop(scratch);
    let positions = (1u64 << shift) - 1;
    let order = keys.iter().map(|&key| (key & positions) as u32).collect();
    for key in &mut keys {
        *key >>= shift;
    }
    (order, keys)
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

    fn validate(bytes: &[u8]) -> Result<Index, Error> {
        let index = Index::parse(ObjectFormat::Sha1, bytes, 13)?;
        index.validate(bytes)?;
        Ok(index)
    }

    #[test]
    fn rejects_every_truncation_of_a_valid_index() {
        let bytes = v2_index(12, &[]);
        assert!(validate(&bytes).is_ok());
        for len in 0..bytes.len() {
            assert!(
                Index::parse(ObjectFormat::Sha1, &bytes[..len], 13).is_err(),
                "accepted {len} bytes"
            );
        }
    }

    /// A two-entry SHA-1 v2 index for pack bytes 12..14, with the second entry's offset word.
    fn two_entry_index(offset: u32, large: &[u8]) -> Vec<u8> {
        let mut bytes = b"\xfftOc\0\0\0\x02".to_vec();
        for bucket in 0..256 {
            let count = u32::from(bucket >= 0x81) + u32::from(bucket >= 0x82);
            bytes.extend_from_slice(&count.to_be_bytes());
        }
        bytes.extend_from_slice(&[0x81; 20]);
        bytes.extend_from_slice(&[0x82; 20]);
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(&12u32.to_be_bytes());
        bytes.extend_from_slice(&offset.to_be_bytes());
        bytes.extend_from_slice(large);
        bytes.extend_from_slice(&[0; 40]);
        bytes
    }

    #[test]
    fn rejects_large_offset_slot_outside_table_on_lookup() {
        let bytes = two_entry_index(0x8000_0001, &13u64.to_be_bytes());
        let index = Index::parse(ObjectFormat::Sha1, &bytes, 14).unwrap();
        assert_eq!(index.offset(&bytes, 0).unwrap(), 12);
        assert!(matches!(
            index.offset(&bytes, 1),
            Err(Error::Corrupt("large offset index"))
        ));
        assert!(matches!(
            index.validate(&bytes),
            Err(Error::Corrupt("large offset index"))
        ));
        let bytes = two_entry_index(0x8000_0000, &13u64.to_be_bytes());
        let index = Index::parse(ObjectFormat::Sha1, &bytes, 14).unwrap();
        index.validate(&bytes).unwrap();
        assert_eq!(index.offset(&bytes, 1).unwrap(), 13);
    }

    /// As in Git, an index has at most one large offset slot fewer than it has entries.
    #[test]
    fn rejects_more_large_offset_slots_than_git_at_parse() {
        let bytes = v2_index(0x8000_0000, &12u64.to_be_bytes());
        assert!(matches!(
            Index::parse(ObjectFormat::Sha1, &bytes, 13),
            Err(Error::Corrupt("index table length"))
        ));
        let bytes = two_entry_index(13, &[0; 4]);
        let index = Index::parse(ObjectFormat::Sha1, &bytes, 14).unwrap();
        assert!(matches!(
            index.validate(&bytes),
            Err(Error::Corrupt("index table length"))
        ));
    }

    #[test]
    fn rejects_non_monotonic_fanout_at_parse() {
        let mut bytes = v2_index(12, &[]);
        bytes[8 + 0x90 * 4 + 3] = 0;
        assert!(matches!(
            Index::parse(ObjectFormat::Sha1, &bytes, 13),
            Err(Error::Corrupt("index fanout"))
        ));
    }

    #[test]
    fn offset_order_sorts_by_offset_at_every_key_width() {
        for offsets in [
            vec![],
            vec![12],
            vec![900, 12, 70_000, 4_000_000_000, 13],
            vec![1 << 40, 12, 1 << 33, 99],
        ] {
            let mut expected: Vec<u32> = (0..offsets.len() as u32).collect();
            expected.sort_by_key(|&position| offsets[position as usize]);
            let mut sorted = offsets.clone();
            sorted.sort_unstable();
            assert_eq!(offset_order(offsets), (expected, sorted));
        }
    }

    #[test]
    fn offset_order_falls_back_when_keys_exceed_64_bits() {
        let offsets = vec![u64::MAX - 1, 12, u64::MAX - 2];
        assert_eq!(
            offset_order(offsets),
            (vec![1, 2, 0], vec![12, u64::MAX - 2, u64::MAX - 1])
        );
    }
}
