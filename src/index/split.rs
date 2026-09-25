//! Shared-index resolution from the published index and EWAH format specifications.
use super::codec::validate_entries;
use super::{Error, Index, Limits};
use crate::ObjectId;

impl Index {
    /// Returns the immutable shared-file identity, if this encoding depends on one.
    ///
    /// After an entry or version edit, the index is standalone and this returns `None`.
    pub fn shared_index_id(&self) -> Option<ObjectId> {
        let link = self.extensions.iter().find(|e| e.signature == *b"link")?;
        let id =
            ObjectId::from_bytes(self.format, link.data.get(..self.format.digest_len())?).ok()?;
        (id != ObjectId::null(self.format)).then_some(id)
    }

    pub(super) fn resolve_shared(
        mut self,
        shared: Option<&[u8]>,
        limits: Limits,
    ) -> Result<Self, Error> {
        let mut links = self.extensions.iter().filter(|e| e.signature == *b"link");
        let Some(link) = links.next() else {
            return Ok(self);
        };
        if links.next().is_some() {
            return Err(invalid("duplicate link extension"));
        }
        let width = self.format.digest_len();
        let hash = link
            .data
            .get(..width)
            .ok_or(invalid("truncated shared identity"))?;
        let id = ObjectId::from_bytes(self.format, hash).unwrap();
        let mut base = if id == ObjectId::null(self.format) {
            Index::empty(self.format)
        } else {
            let bytes = shared.ok_or(Error::SharedRequired(id))?;
            if bytes.len()
                > limits
                    .max_bytes
                    .saturating_sub(self.original.as_ref().map_or(0, Vec::len))
            {
                return Err(Error::Limit("aggregate index bytes"));
            }
            let base = Self::parse(self.format, bytes, limits)?;
            if base.extensions.iter().any(|e| e.signature == *b"link") {
                return Err(invalid("nested shared index"));
            }
            if bytes[bytes.len() - width..] != *hash {
                return Err(Error::Checksum);
            }
            base
        };
        let mut cursor = width;
        // A null dependency with no bitmaps is Git's standalone split representation.
        let (deleted, replaced) = if cursor == link.data.len() && id == ObjectId::null(self.format)
        {
            (Vec::new(), Vec::new())
        } else {
            (
                bitmap(&link.data, &mut cursor, base.entries.len())?,
                bitmap(&link.data, &mut cursor, base.entries.len())?,
            )
        };
        if cursor != link.data.len() {
            return Err(invalid("trailing link bytes"));
        }
        if replaced.len() > self.entries.len() {
            return Err(invalid("missing replacement entries"));
        }
        let mut drafts = self.entries.into_iter();
        for &position in &replaced {
            if deleted.binary_search(&position).is_ok() {
                return Err(invalid("deleted replacement"));
            }
            let mut entry = drafts.next().unwrap();
            if entry.path.is_empty() {
                entry.path = base.entries[position].path.clone();
            }
            base.entries[position] = entry;
        }
        let mut position = 0;
        base.entries.retain(|_| {
            let keep = deleted.binary_search(&position).is_err();
            position += 1;
            keep
        });
        let count = base
            .entries
            .len()
            .checked_add(drafts.len())
            .ok_or(Error::Limit("entries"))?;
        if count > limits.max_entries {
            return Err(Error::Limit("entries"));
        }
        let additions: Vec<_> = drafts.collect();
        validate_entries(self.format, &additions, limits)?;
        base.entries.extend(additions);
        base.entries
            .sort_unstable_by(|a, b| (&a.path, a.stage).cmp(&(&b.path, b.stage)));
        validate_entries(self.format, &base.entries, limits)?;
        self.entries = base.entries;
        Ok(self)
    }
}

fn invalid(reason: &'static str) -> Error {
    Error::Malformed { offset: 0, reason }
}

// Decode only set positions. Runs are checked before expansion; work and allocations are bounded
// by the shared entry count, even when a tiny hostile encoding describes billions of set bits.
fn bitmap(bytes: &[u8], cursor: &mut usize, maximum: usize) -> Result<Vec<usize>, Error> {
    let bits = read_u32(bytes, cursor)? as usize;
    if bits > maximum {
        return Err(invalid("bitmap exceeds shared entries"));
    }
    let words = read_u32(bytes, cursor)? as usize;
    let length = words.checked_mul(8).ok_or(Error::Limit("bitmap bytes"))?;
    let end = cursor
        .checked_add(length)
        .ok_or(Error::Limit("bitmap bytes"))?;
    let data = bytes.get(*cursor..end).ok_or(invalid("truncated bitmap"))?;
    *cursor = end;
    let current = read_u32(bytes, cursor)? as usize;
    if words == 0 || current >= words {
        return Err(invalid("invalid bitmap run pointer"));
    }
    let word = |i: usize| u64::from_be_bytes(data[i * 8..i * 8 + 8].try_into().unwrap());
    let mut output = Vec::new();
    let mut offset = 0usize;
    let mut index = 0;
    let mut last_run = 0;
    while index < words {
        last_run = index;
        let run = word(index);
        index += 1;
        let repeats = ((run >> 1) & 0xffff_ffff) as usize;
        let literals = (run >> 33) as usize;
        let run_bits = repeats.checked_mul(64).ok_or(Error::Limit("bitmap bits"))?;
        let next = offset
            .checked_add(run_bits)
            .ok_or(Error::Limit("bitmap bits"))?;
        if next > bits.saturating_add(63) || literals > words - index {
            return Err(invalid("invalid bitmap run"));
        }
        if run & 1 != 0 {
            if next > bits {
                return Err(invalid("set bitmap padding"));
            }
            output.extend(offset..next);
        }
        offset = next;
        for _ in 0..literals {
            let value = word(index);
            index += 1;
            for bit in 0..64 {
                if value & (1 << bit) != 0 {
                    let position = offset.checked_add(bit).ok_or(Error::Limit("bitmap bits"))?;
                    if position >= bits {
                        return Err(invalid("set bitmap padding"));
                    }
                    output.push(position);
                }
            }
            offset = offset.checked_add(64).ok_or(Error::Limit("bitmap bits"))?;
            if offset > bits.saturating_add(63) {
                return Err(invalid("bitmap exceeds bit length"));
            }
        }
    }
    if offset < bits || current != last_run {
        return Err(invalid("invalid bitmap length or run pointer"));
    }
    Ok(output)
}
fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, Error> {
    let end = cursor.checked_add(4).ok_or(Error::Limit("bitmap bytes"))?;
    let raw = bytes
        .get(*cursor..end)
        .ok_or(invalid("truncated bitmap header"))?;
    *cursor = end;
    Ok(u32::from_be_bytes(raw.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn encoded(bits: u32, words: &[u64], pointer: u32) -> Vec<u8> {
        let mut bytes = bits.to_be_bytes().to_vec();
        bytes.extend_from_slice(&(words.len() as u32).to_be_bytes());
        for word in words {
            bytes.extend_from_slice(&word.to_be_bytes());
        }
        bytes.extend_from_slice(&pointer.to_be_bytes());
        bytes
    }

    #[test]
    fn reads_runs_and_literal_positions() {
        let bytes = encoded(130, &[3 | (1 << 33), 2, 1 << 33, 1], 2);
        let mut cursor = 0;
        let positions = bitmap(&bytes, &mut cursor, 130).unwrap();
        assert_eq!(&positions[..64], (0..64).collect::<Vec<_>>());
        assert_eq!(&positions[64..], &[65, 128]);
        assert_eq!(cursor, bytes.len());
    }

    #[rstest]
    #[case::padding(1, vec![1 << 33, 2], 0, 1)]
    #[case::run(1, vec![u32::MAX as u64 * 2], 0, 1)]
    #[case::pointer(1, vec![1 << 33, 1], 1, 1)]
    #[case::literal_count(1, vec![2 << 33, 1], 0, 1)]
    #[case::entry_bound(2, vec![1 << 33, 1], 0, 1)]
    #[case::short(65, vec![0], 0, 65)]
    fn rejects_invalid_bitmaps(
        #[case] bits: u32,
        #[case] words: Vec<u64>,
        #[case] pointer: u32,
        #[case] maximum: usize,
    ) {
        assert!(bitmap(&encoded(bits, &words, pointer), &mut 0, maximum).is_err());
    }

    #[test]
    fn rejects_every_truncation() {
        let bytes = encoded(1, &[1 << 33, 1], 0);
        assert!((0..bytes.len()).all(|end| bitmap(&bytes[..end], &mut 0, 1).is_err()));
    }
}
