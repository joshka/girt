use crate::{ObjectId, ObjectReadError as Error};

#[derive(Debug)]
pub(crate) struct Entry {
    pub id: ObjectId,
    pub offset: usize,
    pub end: usize,
    pub crc: Option<u32>,
}

/// Identity order serves lookups; a separate offset order serves OFS_DELTA and entry boundaries.
#[derive(Debug)]
pub(super) struct Index {
    pub entries: Vec<Entry>,
    pub offsets: Vec<usize>,
    pub pack_hash: ObjectId,
}

impl Index {
    pub fn parse(
        format: crate::ObjectFormat,
        bytes: &[u8],
        pack_end: usize,
    ) -> Result<Self, Error> {
        let width = format.digest_len();
        if bytes.len() < 8 {
            return Err(Error::Corrupt("truncated index header"));
        }
        if &bytes[..4] != b"\xfftOc" {
            return Self::parse_v1(format, bytes, pack_end);
        }
        let version = word(bytes, 4)?;
        if version != 2 {
            return Err(Error::IndexVersion(version));
        }
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
        verify_hash(format, bytes, "index checksum")?;
        let large_count = (trailer - table_end) / 8;
        if large_count > count {
            return Err(Error::Corrupt("excess large offsets"));
        }
        let mut used_large = vec![false; large_count];
        let mut entries = Vec::with_capacity(count);
        for position in 0..count {
            let start = 1032 + position * width;
            let id = ObjectId::from_bytes(format, &bytes[start..start + width]).unwrap();
            if entries.last().is_some_and(|entry: &Entry| entry.id >= id) {
                return Err(Error::Corrupt("unsorted or duplicate index identities"));
            }
            let crc = word(bytes, 1032 + count * width + position * 4)?;
            let raw_offset = word(bytes, 1032 + count * (width + 4) + position * 4)?;
            let offset = if raw_offset & 0x8000_0000 == 0 {
                raw_offset as u64
            } else {
                let slot = (raw_offset & 0x7fff_ffff) as usize;
                let used = used_large
                    .get_mut(slot)
                    .ok_or(Error::Corrupt("large offset index"))?;
                if *used {
                    return Err(Error::Corrupt("duplicate large offset index"));
                }
                *used = true;
                let start = table_end + slot * 8;
                u64::from_be_bytes(bytes[start..start + 8].try_into().unwrap())
            };
            let offset = usize::try_from(offset).map_err(|_| Error::Corrupt("offset overflow"))?;
            if offset < 12 || offset >= pack_end {
                return Err(Error::Corrupt("offset outside pack entries"));
            }
            entries.push(Entry {
                id,
                offset,
                end: 0,
                crc: Some(crc),
            });
        }
        if used_large.contains(&false) {
            return Err(Error::Corrupt("unused large offset"));
        }
        Self::finish(format, bytes, 8, entries, pack_end)
    }

    fn parse_v1(format: crate::ObjectFormat, bytes: &[u8], pack_end: usize) -> Result<Self, Error> {
        let width = format.digest_len();
        let count = word(bytes, 1020)? as usize;
        let trailer = count
            .checked_mul(width + 4)
            .and_then(|n| n.checked_add(1024))
            .ok_or(Error::Corrupt("index length overflow"))?;
        if trailer.checked_add(2 * width) != Some(bytes.len()) {
            return Err(Error::Corrupt("index table length"));
        }
        verify_hash(format, bytes, "index checksum")?;
        let mut entries = Vec::with_capacity(count);
        for position in 0..count {
            let start = 1024 + position * (width + 4);
            let offset = word(bytes, start)? as usize;
            let id = ObjectId::from_bytes(format, &bytes[start + 4..start + 4 + width]).unwrap();
            if entries.last().is_some_and(|entry: &Entry| entry.id >= id) {
                return Err(Error::Corrupt("unsorted or duplicate index identities"));
            }
            if offset < 12 || offset >= pack_end {
                return Err(Error::Corrupt("offset outside pack entries"));
            }
            entries.push(Entry {
                id,
                offset,
                end: 0,
                crc: None,
            });
        }
        Self::finish(format, bytes, 0, entries, pack_end)
    }

    fn finish(
        format: crate::ObjectFormat,
        bytes: &[u8],
        fanout_start: usize,
        mut entries: Vec<Entry>,
        pack_end: usize,
    ) -> Result<Self, Error> {
        let width = format.digest_len();
        let trailer = bytes.len() - 2 * width;
        let mut fanout = [0u32; 256];
        for entry in &entries {
            fanout[entry.id.as_bytes()[0] as usize] += 1;
        }
        let mut cumulative = 0;
        for (bucket, size) in fanout.into_iter().enumerate() {
            cumulative += size;
            if word(bytes, fanout_start + bucket * 4)? != cumulative {
                return Err(Error::Corrupt("index fanout"));
            }
        }
        let mut offsets: Vec<_> = (0..entries.len()).collect();
        offsets.sort_unstable_by_key(|&position| entries[position].offset);
        let mut next = pack_end;
        for &position in offsets.iter().rev() {
            let entry = &mut entries[position];
            if entry.offset == next {
                return Err(Error::Corrupt("duplicate pack offset"));
            }
            entry.end = next;
            next = entry.offset;
        }
        if next != 12 {
            return Err(Error::Corrupt("unindexed pack bytes"));
        }
        Ok(Self {
            entries,
            offsets,
            pack_hash: ObjectId::from_bytes(format, &bytes[trailer..trailer + width]).unwrap(),
        })
    }

    pub fn find(&self, id: ObjectId) -> Option<usize> {
        self.entries
            .binary_search_by_key(&id, |entry| entry.id)
            .ok()
    }

    pub fn at_offset(&self, offset: usize) -> Result<usize, Error> {
        let position = self
            .offsets
            .binary_search_by_key(&offset, |&i| self.entries[i].offset)
            .map_err(|_| Error::Corrupt("base offset is not an indexed entry"))?;
        Ok(self.offsets[position])
    }
}

pub(crate) fn word(bytes: &[u8], position: usize) -> Result<u32, Error> {
    let word = bytes
        .get(
            position
                ..position
                    .checked_add(4)
                    .ok_or(Error::Corrupt("integer offset overflow"))?,
        )
        .ok_or(Error::Corrupt("truncated integer"))?;
    Ok(u32::from_be_bytes(word.try_into().unwrap()))
}

pub(crate) fn verify_hash(
    format: crate::ObjectFormat,
    bytes: &[u8],
    reason: &'static str,
) -> Result<(), Error> {
    let end = bytes
        .len()
        .checked_sub(format.digest_len())
        .ok_or(Error::Corrupt(reason))?;
    if *format.checksum(&bytes[..end]).as_bytes() != bytes[end..] {
        return Err(Error::Corrupt(reason));
    }
    Ok(())
}
