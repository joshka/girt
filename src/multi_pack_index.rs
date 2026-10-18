//! Object lookup across multiple packs using Git's optional multi-pack index.
//!
//! [`MultiPackIndex::parse`] checks the complete file, including its checksum, before exposing
//! entries. Parsing establishes structural consistency, not agreement with the named packs.
//! Pack indexes and object hashes remain necessary to verify those claims. Unknown chunks are
//! ignored; bitmap ordering chunks are checked when present.
//!
//! ```no_run
//! use girt::ObjectFormat;
//! use girt::multi_pack_index::MultiPackIndex;
//! let bytes = std::fs::read("repo.git/objects/pack/multi-pack-index")?;
//! let index = MultiPackIndex::parse(&bytes, ObjectFormat::Sha1)?;
//! println!(
//!     "{} objects in {} packs",
//!     index.len(),
//!     index.pack_names().len()
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod store;
use std::collections::BTreeMap;

pub(crate) use store::Lookup;

use crate::{ObjectFormat, ObjectId};

/// An immutable, structurally verified MIDX layer, borrowing its encoded object tables.
#[derive(Clone, Debug)]
pub struct MultiPackIndex<'a> {
    format: ObjectFormat,
    names: Vec<&'a str>,
    ids: &'a [u8],
    offsets: &'a [u8],
    large: Option<&'a [u8]>,
    bases: usize,
}

/// Input and derived-allocation bounds checked before parsing tables.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum file size, including the checksum (default 64 MiB).
    pub max_bytes: usize,
    /// Maximum number of named packs (default 4096).
    pub max_packs: usize,
    /// Maximum number of indexed objects (default 5 million).
    pub max_objects: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
            max_packs: 4096,
            max_objects: 5_000_000,
        }
    }
}

/// A MIDX claim locating an object in a named pack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Location {
    /// Pack ordinal within this layer’s name table.
    pub pack: u32,
    /// Absolute byte offset from the start of the pack file.
    pub offset: u64,
}

/// Failure to parse an auxiliary multi-pack index.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The object-store snapshot does not contain all packs named by the index.
    /// Reopen after repacking, or regenerate the stale MIDX.
    #[error("multi-pack index names unavailable packs")]
    MissingPack,
    /// The encoded structure is inconsistent.
    #[error("malformed multi-pack index: {0}")]
    Malformed(&'static str),
    /// A recognized header choice is not supported by this reader.
    #[error("unsupported multi-pack index: {0}")]
    Unsupported(&'static str),
    /// Input exceeds a caller-selected resource budget.
    #[error("multi-pack index limit exceeded: {0}")]
    Limit(&'static str),
    /// The trailer does not match the preceding bytes.
    #[error("multi-pack index checksum mismatch")]
    Checksum,
    /// The named pack index failed its existing integrity checks.
    #[error("verifying multi-pack index pack")]
    Pack(#[from] crate::ObjectReadError),
}

impl<'a> MultiPackIndex<'a> {
    /// Parses a layer with default bounds; see [`Self::parse_with`].
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for malformed, unsupported, over-budget or checksum-invalid input.
    pub fn parse(bytes: &'a [u8], format: ObjectFormat) -> Result<Self, Error> {
        Self::parse_with(bytes, format, Limits::default())
    }

    /// Checks the header, checksum, chunk boundaries, names, fanout, sorted IDs and offsets.
    ///
    /// Versions 1 and 2 and both object formats are supported. Nonzero base counts are
    /// retained for explicit callers and ignored by automatic lookup.
    /// Unknown chunks are accepted. No filesystem access or pack verification occurs.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] before allocating names or ordering validation storage beyond `limits`.
    pub fn parse_with(
        bytes: &'a [u8],
        format: ObjectFormat,
        limits: Limits,
    ) -> Result<Self, Error> {
        if bytes.len() > limits.max_bytes {
            return Err(Error::Limit("bytes"));
        }
        if bytes.len() < 12 + format.digest_len() || bytes.get(..4) != Some(b"MIDX") {
            return Err(bad("header"));
        }
        if !matches!(bytes[4], 1 | 2) {
            return Err(Error::Unsupported("version"));
        }
        let hash = match format {
            ObjectFormat::Sha1 => 1,
            ObjectFormat::Sha256 => 2,
        };
        if bytes[5] != hash {
            return Err(Error::Unsupported("object format"));
        }
        let end = bytes.len() - format.digest_len();
        if format.checksum(&bytes[..end]).as_bytes() != &bytes[end..] {
            return Err(Error::Checksum);
        }
        let count = usize::from(bytes[6]);
        let table_end = 12 + (count + 1) * 12;
        if table_end > end {
            return Err(bad("chunk table"));
        }
        let mut chunks = BTreeMap::new();
        let mut previous = table_end;
        for i in 0..=count {
            let row = 12 + i * 12;
            let tag: [u8; 4] = bytes[row..row + 4]
                .try_into()
                .map_err(|_| bad("chunk tag"))?;
            let start = usize::try_from(long(bytes, row + 4)?).map_err(|_| bad("chunk offset"))?;
            if start < previous || start > end {
                return Err(bad("chunk order"));
            }
            if i == count {
                if tag != [0; 4] || start != end {
                    return Err(bad("chunk terminator"));
                }
            } else {
                let next =
                    usize::try_from(long(bytes, row + 16)?).map_err(|_| bad("chunk offset"))?;
                let value = bytes
                    .get(start..next)
                    .filter(|_| next <= end)
                    .ok_or(bad("chunk range"))?;
                if tag == [0; 4] || chunks.insert(tag, value).is_some() {
                    return Err(bad("chunk tag"));
                }
            }
            previous = start;
        }
        let required = |tag| chunks.get(&tag).copied().ok_or(bad("missing chunk"));
        let packs = word(bytes, 8)? as usize;
        if packs > limits.max_packs {
            return Err(Error::Limit("packs"));
        }
        let names = parse_names(required(*b"PNAM")?, packs, bytes[4])?;
        let fanout = required(*b"OIDF")?;
        if fanout.len() != 1024 {
            return Err(bad("fanout size"));
        }
        let objects = word(fanout, 1020)? as usize;
        if objects > limits.max_objects {
            return Err(Error::Limit("objects"));
        }
        let ids = required(*b"OIDL")?;
        let offsets = required(*b"OOFF")?;
        if objects.checked_mul(format.digest_len()) != Some(ids.len())
            || objects.checked_mul(8) != Some(offsets.len())
        {
            return Err(bad("object table size"));
        }
        let large = chunks.get(b"LOFF").copied();
        if large.is_some_and(|large| large.len() % 8 != 0) {
            return Err(bad("large offset size"));
        }
        let index = Self {
            format,
            names,
            ids,
            offsets,
            large,
            bases: usize::from(bytes[7]),
        };
        index.validate_objects(fanout)?;
        if let Some(order) = chunks.get(b"RIDX") {
            validate_order(order, objects)?;
        }
        if let Some(bitmap) = chunks.get(b"BTMP") {
            validate_bitmap_packs(bitmap, packs, objects)?;
        }
        Ok(index)
    }

    /// Verifies every named pack index and every claimed ID/offset against an object-store
    /// snapshot.
    ///
    /// The snapshot may contain other packs. Names must resolve together within one object
    /// directory. This checks index checksums and structure, not pack payloads; use
    /// [`crate::Objects::verify_packs`] for full pack verification. No files are changed.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for missing packs, unsupported base counts, conflicting entries or
    /// damaged pack indexes. Reopen the object store after concurrent repacking before retrying.
    pub fn verify(&self, objects: &crate::Objects) -> Result<(), Error> {
        if self.bases != 0 {
            return Err(Error::Unsupported("base layers"));
        }
        if objects.object_format() != self.format {
            return Err(Error::Unsupported("object format"));
        }
        objects.verify_multi_pack_index(self)
    }

    /// Number of objects represented by this layer, excluding its bases.
    pub fn len(&self) -> usize {
        self.ids.len() / self.format.digest_len()
    }

    /// Whether this layer represents no objects.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Index filenames, in encoded pack order, relative to the containing pack directory.
    pub fn pack_names(&self) -> &[&'a str] {
        &self.names
    }

    /// Number of preceding MIDX layers required to interpret this layer.
    pub fn base_count(&self) -> usize {
        self.bases
    }

    /// Finds an object's location using one binary search. A hash-format mismatch returns `None`.
    pub fn find(&self, id: ObjectId) -> Option<Location> {
        if id.format() != self.format {
            return None;
        }
        let width = self.format.digest_len();
        let mut low = 0;
        let mut high = self.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let candidate = &self.ids[middle * width..(middle + 1) * width];
            match candidate.cmp(id.as_bytes()) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return self.location(middle).ok(),
            }
        }
        None
    }

    pub(crate) fn verify_entries(
        &self,
        packs: &[&std::sync::Arc<crate::pack::FilePack>],
    ) -> Result<(), Error> {
        for (position, bytes) in self.ids.chunks_exact(self.format.digest_len()).enumerate() {
            let id = ObjectId::from_bytes(self.format, bytes).map_err(|_| bad("object ID"))?;
            let location = self.location(position)?;
            let pack = packs
                .get(location.pack as usize)
                .ok_or(bad("pack ordinal"))?;
            let found = pack.find(id)?.ok_or(bad("object absent from named pack"))?;
            if pack.offset(found)? as u64 != location.offset {
                return Err(bad("object offset disagrees with pack index"));
            }
        }
        Ok(())
    }

    fn location(&self, position: usize) -> Result<Location, Error> {
        let row = position * 8;
        let pack = word(self.offsets, row)?;
        let offset = word(self.offsets, row + 4)?;
        let offset = if let Some(large) = self.large.filter(|_| offset & 0x8000_0000 != 0) {
            long(
                large,
                ((offset & 0x7fff_ffff) as usize)
                    .checked_mul(8)
                    .ok_or(bad("large offset slot"))?,
            )?
        } else {
            u64::from(offset)
        };
        if offset < 12 {
            return Err(bad("object offset"));
        }
        Ok(Location { pack, offset })
    }

    fn validate_objects(&self, fanout: &[u8]) -> Result<(), Error> {
        let mut counts = [0_u32; 256];
        let mut previous = None;
        for (position, id) in self.ids.chunks_exact(self.format.digest_len()).enumerate() {
            if previous.is_some_and(|previous| previous >= id) {
                return Err(bad("object order"));
            }
            previous = Some(id);
            counts[usize::from(id[0])] += 1;
            let location = self.location(position)?;
            if self.bases == 0 && location.pack as usize >= self.names.len() {
                return Err(bad("pack ordinal"));
            }
        }
        let mut total = 0;
        for (i, count) in counts.into_iter().enumerate() {
            total += count;
            if word(fanout, i * 4)? != total {
                return Err(bad("fanout contents"));
            }
        }
        Ok(())
    }
}

fn parse_names(bytes: &[u8], count: usize, version: u8) -> Result<Vec<&str>, Error> {
    let mut rest = bytes;
    let mut names = Vec::with_capacity(count.min(bytes.len()));
    for _ in 0..count {
        let end = rest
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(bad("pack name terminator"))?;
        let name = std::str::from_utf8(&rest[..end]).map_err(|_| bad("pack name encoding"))?;
        if !name.starts_with("pack-")
            || !name.ends_with(".idx")
            || name.bytes().any(|b| b == b'/' || b == b'\\')
        {
            return Err(bad("pack name"));
        }
        if version == 1 && names.last().is_some_and(|last| *last >= name) {
            return Err(bad("pack name order"));
        }
        names.push(name);
        rest = &rest[end + 1..];
    }
    if rest.len() > 3 || rest.iter().any(|byte| *byte != 0) {
        return Err(bad("pack name padding"));
    }
    let mut unique = names.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != count {
        return Err(bad("duplicate pack name"));
    }
    Ok(names)
}

fn validate_order(bytes: &[u8], objects: usize) -> Result<(), Error> {
    if objects.checked_mul(4) != Some(bytes.len()) {
        return Err(bad("reverse index size"));
    }
    let mut seen = vec![false; objects];
    for row in bytes.as_chunks::<4>().0 {
        let position = word(row, 0)? as usize;
        let slot = seen
            .get_mut(position)
            .ok_or(bad("reverse index position"))?;
        if *slot {
            return Err(bad("duplicate reverse index position"));
        }
        *slot = true;
    }
    Ok(())
}

fn validate_bitmap_packs(bytes: &[u8], packs: usize, objects: usize) -> Result<(), Error> {
    if packs.checked_mul(8) != Some(bytes.len()) {
        return Err(bad("bitmap pack table size"));
    }
    for row in bytes.as_chunks::<8>().0 {
        let start = word(row, 0)? as usize;
        let count = word(row, 4)? as usize;
        if start.checked_add(count).is_none_or(|end| end > objects) {
            return Err(bad("bitmap pack range"));
        }
    }
    Ok(())
}

fn word(bytes: &[u8], start: usize) -> Result<u32, Error> {
    let row = bytes
        .get(start..start.checked_add(4).ok_or(bad("integer offset"))?)
        .ok_or(bad("integer range"))?;
    Ok(u32::from_be_bytes(
        row.try_into().map_err(|_| bad("integer width"))?,
    ))
}

fn long(bytes: &[u8], start: usize) -> Result<u64, Error> {
    let row = bytes
        .get(start..start.checked_add(8).ok_or(bad("integer offset"))?)
        .ok_or(bad("integer range"))?;
    Ok(u64::from_be_bytes(
        row.try_into().map_err(|_| bad("integer width"))?,
    ))
}

fn bad(reason: &'static str) -> Error {
    Error::Malformed(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut bytes = b"MIDX\x01\x01\x04\0\0\0\0\x01".to_vec();
        let names = b"pack-original.idx\0\0\0";
        let start = 12 + 5 * 12;
        for (tag, offset) in [
            (*b"PNAM", start),
            (*b"OIDF", start + names.len()),
            (*b"OIDL", start + names.len() + 1024),
            (*b"OOFF", start + names.len() + 1044),
            ([0; 4], start + names.len() + 1052),
        ] {
            bytes.extend_from_slice(&tag);
            bytes.extend_from_slice(&(offset as u64).to_be_bytes());
        }
        bytes.extend_from_slice(names);
        for _ in 0..256 {
            bytes.extend_from_slice(&1_u32.to_be_bytes());
        }
        bytes.extend_from_slice(&[0; 20]);
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&12_u32.to_be_bytes());
        let checksum = ObjectFormat::Sha1.checksum(&bytes);
        bytes.extend_from_slice(checksum.as_bytes());
        bytes
    }

    fn resign(bytes: &mut [u8]) {
        let end = bytes.len() - 20;
        let checksum = ObjectFormat::Sha1.checksum(&bytes[..end]);
        bytes[end..].copy_from_slice(checksum.as_bytes());
    }

    #[test]
    fn every_truncation_is_rejected() {
        let bytes = fixture();
        assert_eq!(
            MultiPackIndex::parse(&bytes, ObjectFormat::Sha1)
                .unwrap()
                .len(),
            1
        );
        reject_truncations(&bytes);
    }

    fn reject_truncations(bytes: &[u8]) {
        for end in 0..bytes.len() {
            assert!(
                MultiPackIndex::parse(&bytes[..end], ObjectFormat::Sha1).is_err(),
                "accepted {end}"
            );
        }
    }

    #[rstest::rstest]
    #[case::fanout(92, 2)]
    #[case::name_path(77, b'/')]
    #[case::pack_ordinal(1139, 2)]
    #[case::chunk_offset(35, 0xff)]
    fn checksum_valid_structural_damage_is_rejected(#[case] position: usize, #[case] value: u8) {
        let mut bytes = fixture();
        bytes[position] = value;
        resign(&mut bytes);
        assert!(MultiPackIndex::parse(&bytes, ObjectFormat::Sha1).is_err());
    }
    fn with_large_offsets(large: &[u8]) -> Vec<u8> {
        let original = fixture();
        let old_end = original.len() - 20;
        let mut bytes = original[..12].to_vec();
        bytes[6] = 5;
        for row in original[12..60].as_chunks::<12>().0 {
            bytes.extend_from_slice(&row[..4]);
            let offset = u64::from_be_bytes(row[4..].try_into().unwrap()) + 12;
            bytes.extend_from_slice(&offset.to_be_bytes());
        }
        bytes.extend_from_slice(b"LOFF");
        bytes.extend_from_slice(&((old_end + 12) as u64).to_be_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&((old_end + 12 + large.len()) as u64).to_be_bytes());
        bytes.extend_from_slice(&original[72..old_end]);
        bytes[1152..1156].copy_from_slice(&0x8000_0000_u32.to_be_bytes());
        bytes.extend_from_slice(large);
        let checksum = ObjectFormat::Sha1.checksum(&bytes);
        bytes.extend_from_slice(checksum.as_bytes());
        bytes
    }

    #[test]
    fn present_empty_large_offset_chunk_does_not_turn_slot_into_direct_offset() {
        let bytes = with_large_offsets(&[]);
        assert!(MultiPackIndex::parse(&bytes, ObjectFormat::Sha1).is_err());
    }

    #[test]
    fn large_offset_slot_resolves_full_width() {
        let bytes = with_large_offsets(&0x1_0000_0000_u64.to_be_bytes());
        let index = MultiPackIndex::parse(&bytes, ObjectFormat::Sha1).unwrap();
        let id = ObjectId::from_bytes(ObjectFormat::Sha1, &[0; 20]).unwrap();
        assert_eq!(index.find(id).unwrap().offset, 0x1_0000_0000);
    }

    #[test]
    fn object_limit_applies_before_decoded_allocations() {
        let bytes = fixture();
        let limits = Limits {
            max_objects: 0,
            ..Limits::default()
        };
        assert!(matches!(
            MultiPackIndex::parse_with(&bytes, ObjectFormat::Sha1, limits),
            Err(Error::Limit("objects"))
        ));
    }
}
