//! Reachable object sets, optional bitmap acceleration and pack/MIDX reverse indexes.
//!
//! Use [`Objects::reachable_difference`](crate::Objects::reachable_difference) for object counts or
//! a fetch response's want/have difference. It uses valid bitmap shortcuts where available and
//! walks uncovered graph objects. [`QueryLimits`] bounds both closures and optional metadata work;
//! [`Reachability`] retains the object-store snapshot for subsequent reads.
//!
//! ```no_run
//! use girt::Repository;
//! let repository = Repository::open("repo.git")?;
//! let tip = repository.head()?.id.ok_or("unborn HEAD")?;
//! let objects = repository.objects()?;
//! let reachable = objects.reachable_difference(&[tip], &[])?;
//! println!("{} objects reachable from HEAD", reachable.len());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [`Objects::encode_pack_bitmap`](crate::Objects::encode_pack_bitmap) and
//! [`Objects::encode_multi_pack_bitmap`](crate::Objects::encode_multi_pack_bitmap) compute new
//! artifacts from verified graph contents under independent [`WriteLimits`]. Repository bitmap
//! publication methods require maintenance exclusion and report post-publication failures through
//! [`InstallError`]. Repacks write bitmaps only when explicitly enabled; configure that separately
//! through [`crate::retention::RepackLimits`].
//!
//! A bitmap position names an object in pack order (or MIDX pseudo-pack order), not object-ID
//! order. [`BitmapIndex`] checks compressed data and its association with a caller-selected pack
//! or MIDX checksum. [`ReverseIndex`] maps those positions back to ordinary index positions.
//! Parsing does not prove graph reachability: these are claims made by derived metadata.
//!
//! ```no_run
//! use girt::Repository;
//! use girt::bitmap::BitmapIndex;
//! let repository = Repository::open("repo.git")?;
//! let objects = repository.objects()?;
//! let pack = objects.packs().next().ok_or("no pack")?;
//! let bytes = std::fs::read(pack.path().with_extension("bitmap"))?;
//! let index = BitmapIndex::parse(&bytes, pack.checksum(), pack.object_count())?;
//! println!("{} selected commits", index.entry_positions().len());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod ewah;
mod install;
mod inventory;
mod query;
mod reverse;
mod write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ewah::{Ewah, long, word};
pub(crate) use install::write_repack_bitmap;
pub use install::{InstallError, Published};
pub use query::{QueryLimits, Reachability};
pub use reverse::ReverseIndex;
pub use write::{MultiPackWritten, WriteError, WriteLimits, WriteOptions, Written};

use crate::{ObjectId, ObjectKind};

/// Input, object-universe and decompression bounds for bitmap metadata.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum complete input length (default 1 GiB).
    pub max_bytes: usize,
    /// Maximum objects in the associated pack/MIDX (default 40 million, about four times the
    /// Linux kernel).
    pub max_objects: usize,
    /// Maximum selected-commit records (default 100,000).
    pub max_entries: usize,
    /// Maximum expanded 64-bit words per parse or decode operation (default 64 million, 512 MB).
    /// One uncompressed bitmap over 40 million objects is 625,000 words.
    /// Counts repeated XOR dependencies as work, not just retained output storage.
    pub max_decoded_words: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024 * 1024,
            max_objects: 40_000_000,
            max_entries: 100_000,
            max_decoded_words: 64_000_000,
        }
    }
}

/// Invalid, unsupported or over-budget derived bitmap data.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The file violates a structural or compression invariant.
    #[error("malformed bitmap metadata: {0}")]
    Malformed(&'static str),
    /// A version or extension is recognized but unsupported.
    #[error("unsupported bitmap metadata: {0}")]
    Unsupported(&'static str),
    /// An input or expansion bound is exhausted.
    #[error("bitmap limit exceeded: {0}")]
    Limit(&'static str),
    /// The file checksum does not match its contents.
    #[error("bitmap metadata checksum mismatch")]
    Checksum,
    /// The embedded pack/MIDX checksum differs from the caller's expected inventory.
    #[error("bitmap metadata belongs to another pack or multi-pack index")]
    Binding,
    /// Reading or validating a graph object failed.
    #[error("reading bitmap query object")]
    Read(#[from] crate::ObjectReadError),
    /// A named graph object is unavailable in this snapshot.
    #[error("missing reachable object {0}")]
    MissingObject(ObjectId),
    /// A graph object's content cannot be interpreted.
    #[error("malformed reachable object {id}")]
    Object {
        /// Object whose contents failed to parse.
        id: ObjectId,
        /// The typed parser failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// An edge's declared kind disagrees with the target object.
    #[error("reachable object {0} has the wrong kind")]
    Kind(ObjectId),
    /// Cooperative cancellation was requested; no partial bitmap is returned.
    #[error("bitmap operation cancelled")]
    Cancelled,
}

/// An immutable set of positions in one fixed object universe.
#[derive(Clone, Debug)]
pub struct Bitmap {
    pub(crate) words: Arc<[u64]>,
    pub(crate) objects: usize,
}

impl Bitmap {
    /// Number of positions available, including unset positions.
    pub fn bit_len(&self) -> usize {
        self.objects
    }
    /// Number of set positions.
    pub fn count(&self) -> usize {
        self.words
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }
    /// Whether `position` is set; out-of-universe positions return false.
    pub fn contains(&self, position: usize) -> bool {
        position < self.objects
            && self
                .words
                .get(position / 64)
                .is_some_and(|word| word & (1 << (position % 64)) != 0)
    }
    /// Set positions in ascending order, without allocating one value per object.
    pub fn positions(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(index, &value)| {
            let mut remaining = value;
            std::iter::from_fn(move || {
                if remaining == 0 {
                    return None;
                }
                let bit = remaining.trailing_zeros() as usize;
                remaining &= remaining - 1;
                Some(index * 64 + bit)
            })
        })
    }
}

/// An owned immutable bitmap index, including structurally checked compressed commit entries.
///
/// Type bitmaps are decoded during parsing and partition the supplied object universe exactly.
/// Commit bitmaps remain compressed until requested; XOR dependencies are resolved iteratively
/// under a work budget. Retaining the index copies at most `Limits::max_bytes` input bytes plus
/// bounded entry descriptors and four type bitmaps.
#[derive(Clone, Debug)]
pub struct BitmapIndex {
    bytes: Arc<[u8]>,
    anchor: ObjectId,
    objects: usize,
    types: [Bitmap; 4],
    entries: Vec<Entry>,
    sorted: Vec<usize>,
    hashes: Option<usize>,
}

#[derive(Clone, Debug)]
struct Entry {
    position: u32,
    xor: usize,
    offset: usize,
    bitmap: Ewah,
}

impl BitmapIndex {
    /// Parses bitmap v1 with default bounds for the expected pack/MIDX inventory.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for checksum, inventory, structural, extension or resource failures.
    pub fn parse(bytes: &[u8], expected: ObjectId, objects: usize) -> Result<Self, Error> {
        Self::parse_with(bytes, expected, objects, Limits::default())
    }

    /// Checks header, checksums, EWAH framing, type partition, XOR references and extensions.
    ///
    /// Supports name-hash caches and lookup tables. Pseudo-merge extensions and unknown flags are
    /// refused explicitly. `expected` selects both hash width and the associated inventory.
    /// Object positions still require binding through the pack/MIDX reverse ordering before they
    /// can become object IDs. Parsing does not read or verify object payloads.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] before input copying or decoded allocations exceed `limits`. EWAH logical
    /// bit lengths may round up to a whole word; set bits outside `objects` are always rejected.
    pub fn parse_with(
        bytes: &[u8],
        expected: ObjectId,
        objects: usize,
        limits: Limits,
    ) -> Result<Self, Error> {
        bounds(bytes, objects, limits)?;
        let width = expected.format().digest_len();
        if bytes.len() < 12 + 2 * width || bytes.get(..4) != Some(b"BITM") {
            return Err(bad("bitmap header"));
        }
        if ewah::short(bytes, 4)? != 1 {
            return Err(Error::Unsupported("bitmap version"));
        }
        let flags = ewah::short(bytes, 6)?;
        if flags & 1 == 0 {
            return Err(bad("full DAG flag"));
        }
        if flags & !0x15 != 0 {
            return Err(Error::Unsupported("bitmap flags"));
        }
        let end = bytes.len() - width;
        if expected.format().checksum(&bytes[..end]).as_bytes() != &bytes[end..] {
            return Err(Error::Checksum);
        }
        if bytes.get(12..12 + width) != Some(expected.as_bytes()) {
            return Err(Error::Binding);
        }
        let count = word(bytes, 8)? as usize;
        if count > limits.max_entries {
            return Err(Error::Limit("entries"));
        }
        let cache_bytes = if flags & 4 != 0 {
            objects.checked_mul(4).ok_or(Error::Limit("hash cache"))?
        } else {
            0
        };
        let table_bytes = if flags & 16 != 0 {
            count.checked_mul(16).ok_or(Error::Limit("lookup table"))?
        } else {
            0
        };
        let cache_start = end
            .checked_sub(cache_bytes)
            .ok_or(bad("hash cache range"))?;
        let entries_end = cache_start
            .checked_sub(table_bytes)
            .ok_or(bad("lookup table range"))?;
        let mut cursor = 12 + width;
        if entries_end < cursor || count > (entries_end - cursor) / 6 {
            return Err(bad("entry count"));
        }
        let mut budget = limits.max_decoded_words;
        let mut parsed_types = Vec::new();
        for _ in 0..4 {
            let encoded = Ewah::parse(bytes, &mut cursor, entries_end, objects)?;
            let mut output = zero_words(objects, &mut budget)?;
            encoded.xor_into(bytes, &mut output, &mut budget, &AtomicBool::new(false))?;
            parsed_types.push(Bitmap {
                words: output.into(),
                objects,
            });
        }
        let types: [Bitmap; 4] = parsed_types
            .try_into()
            .map_err(|_| bad("type bitmap count"))?;
        validate_types(&types, objects)?;
        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            let offset = cursor;
            let position = word(bytes, cursor)?;
            let xor = usize::from(*bytes.get(cursor + 4).ok_or(bad("entry header"))?);
            let entry_flags = *bytes.get(cursor + 5).ok_or(bad("entry header"))?;
            cursor += 6;
            if position as usize >= objects || xor > index || xor > 160 {
                return Err(bad("entry position or XOR reference"));
            }
            if entry_flags & !1 != 0 {
                return Err(Error::Unsupported("entry flags"));
            }
            let bitmap = Ewah::parse(bytes, &mut cursor, entries_end, objects)?;
            entries.push(Entry {
                position,
                xor,
                offset,
                bitmap,
            });
        }
        if cursor != entries_end {
            return Err(bad("trailing entry data"));
        }
        let mut sorted: Vec<_> = (0..count).collect();
        sorted.sort_unstable_by_key(|&index| entries[index].position);
        for pair in sorted.windows(2) {
            if entries[pair[0]].position == entries[pair[1]].position {
                return Err(bad("duplicate commit position"));
            }
        }
        if flags & 16 != 0 {
            validate_lookup(bytes, entries_end, &entries, &sorted)?;
        }
        Ok(Self {
            bytes: Arc::from(bytes),
            anchor: expected,
            objects,
            types,
            entries,
            sorted,
            hashes: (flags & 4 != 0).then_some(cache_start),
        })
    }

    /// Checksum of the associated pack or MIDX, established by parsing.
    pub fn anchor(&self) -> ObjectId {
        self.anchor
    }
    /// Object count of the associated inventory.
    pub fn object_count(&self) -> usize {
        self.objects
    }
    /// Selected commit positions in ascending ordinary-index order, not bitmap bit order.
    pub fn entry_positions(&self) -> impl ExactSizeIterator<Item = u32> + '_ {
        self.sorted
            .iter()
            .map(|&index| self.entries[index].position)
    }
    /// Type membership in bitmap order, not ordinary-index order.
    pub fn type_bitmap(&self, kind: ObjectKind) -> &Bitmap {
        &self.types[kind_index(kind)]
    }
    /// Optional pathname hash in ordinary-index order; this is a heuristic, not object identity.
    pub fn name_hash(&self, position: usize) -> Option<u32> {
        if position >= self.objects {
            return None;
        }
        word(&self.bytes, self.hashes? + position * 4).ok()
    }
    /// Decodes the bitmap for a selected commit's ordinary-index position with default bounds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Limit`] when expansion/XOR work exceeds defaults. Absence is `Ok(None)`.
    pub fn reachable(&self, index_position: u32) -> Result<Option<Bitmap>, Error> {
        self.reachable_with(index_position, Limits::default(), &AtomicBool::new(false))
    }
    /// Decodes iteratively, charging each XOR dependency's expanded words to the same budget.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for exhausted bounds or cancellation, without a partial result.
    pub fn reachable_with(
        &self,
        index_position: u32,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Option<Bitmap>, Error> {
        check(cancel)?;
        bounds(&self.bytes, self.objects, limits)?;
        if self.entries.len() > limits.max_entries {
            return Err(Error::Limit("entries"));
        }
        let Ok(rank) = self
            .sorted
            .binary_search_by_key(&index_position, |&index| self.entries[index].position)
        else {
            return Ok(None);
        };
        let mut index = self.sorted[rank];
        let mut budget = limits.max_decoded_words;
        let mut output = zero_words(self.objects, &mut budget)?;
        loop {
            check(cancel)?;
            let entry = &self.entries[index];
            entry
                .bitmap
                .xor_into(&self.bytes, &mut output, &mut budget, cancel)?;
            if entry.xor == 0 {
                break;
            }
            index -= entry.xor;
        }
        Ok(Some(Bitmap {
            words: output.into(),
            objects: self.objects,
        }))
    }
}

fn validate_types(types: &[Bitmap; 4], objects: usize) -> Result<(), Error> {
    for index in 0..objects.div_ceil(64) {
        let mut union = 0;
        for bitmap in types {
            let word = bitmap.words[index];
            if union & word != 0 {
                return Err(bad("overlapping type bitmaps"));
            }
            union |= word;
        }
        let expected = if index + 1 == objects.div_ceil(64) {
            tail_mask(objects)
        } else {
            u64::MAX
        };
        if union != expected {
            return Err(bad("incomplete type bitmaps"));
        }
    }
    Ok(())
}

fn validate_lookup(
    bytes: &[u8],
    start: usize,
    entries: &[Entry],
    sorted: &[usize],
) -> Result<(), Error> {
    for (row, &index) in sorted.iter().enumerate() {
        let at = start + row * 16;
        let entry = &entries[index];
        if word(bytes, at)? != entry.position || long(bytes, at + 4)? != entry.offset as u64 {
            return Err(bad("lookup entry"));
        }
        let parent = word(bytes, at + 12)?;
        if entry.xor == 0 {
            if parent != u32::MAX {
                return Err(bad("lookup XOR reference"));
            }
        } else if sorted.get(parent as usize) != Some(&(index - entry.xor)) {
            return Err(bad("lookup XOR reference"));
        }
    }
    Ok(())
}

pub(crate) fn kind_index(kind: ObjectKind) -> usize {
    match kind {
        ObjectKind::Commit => 0,
        ObjectKind::Tree => 1,
        ObjectKind::Blob => 2,
        ObjectKind::Tag => 3,
    }
}
fn tail_mask(objects: usize) -> u64 {
    if objects.is_multiple_of(64) {
        u64::MAX
    } else {
        (1 << (objects % 64)) - 1
    }
}
fn zero_words(objects: usize, budget: &mut usize) -> Result<Vec<u64>, Error> {
    let words = objects.div_ceil(64);
    charge(budget, words)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(words)
        .map_err(|_| Error::Limit("decoded storage"))?;
    output.resize(words, 0);
    Ok(output)
}
fn charge(budget: &mut usize, words: usize) -> Result<(), Error> {
    *budget = budget
        .checked_sub(words)
        .ok_or(Error::Limit("decoded words"))?;
    Ok(())
}
fn bounds(bytes: &[u8], objects: usize, limits: Limits) -> Result<(), Error> {
    if bytes.len() > limits.max_bytes {
        return Err(Error::Limit("bytes"));
    }
    if objects > limits.max_objects || objects > u32::MAX as usize {
        return Err(Error::Limit("objects"));
    }
    Ok(())
}
fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    Ok(())
}
fn bad(reason: &'static str) -> Error {
    Error::Malformed(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ewah(bytes: &mut Vec<u8>, bits: u32, value: Option<u64>) {
        bytes.extend_from_slice(&bits.to_be_bytes());
        bytes.extend_from_slice(&(if value.is_some() { 2_u32 } else { 1 }).to_be_bytes());
        bytes.extend_from_slice(&(if value.is_some() { 1_u64 << 33 } else { 0 }).to_be_bytes());
        if let Some(value) = value {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(&0_u32.to_be_bytes());
    }

    fn file() -> (ObjectId, Vec<u8>) {
        let anchor = crate::ObjectFormat::Sha1.checksum(b"pack");
        let mut bytes = b"BITM\0\x01\0\x11".to_vec();
        bytes.extend_from_slice(&2_u32.to_be_bytes());
        bytes.extend_from_slice(anchor.as_bytes());
        ewah(&mut bytes, 8, Some(255));
        for _ in 0..3 {
            ewah(&mut bytes, 0, None);
        }
        let first = bytes.len();
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&[0, 0]);
        ewah(&mut bytes, 64, Some(255));
        let second = bytes.len();
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&[1, 0]);
        ewah(&mut bytes, 64, Some(1));
        for (position, offset, parent) in [(0_u32, first, u32::MAX), (1, second, 0)] {
            bytes.extend_from_slice(&position.to_be_bytes());
            bytes.extend_from_slice(&(offset as u64).to_be_bytes());
            bytes.extend_from_slice(&parent.to_be_bytes());
        }
        let hash = anchor.format().checksum(&bytes);
        bytes.extend_from_slice(hash.as_bytes());
        (anchor, bytes)
    }

    #[test]
    fn xor_resolves_prior_physical_entry_and_lookup_row() {
        let (anchor, bytes) = file();
        let index = BitmapIndex::parse(&bytes, anchor, 8).unwrap();
        assert_eq!(index.reachable(0).unwrap().unwrap().count(), 8);
        let bitmap = index.reachable(1).unwrap().unwrap();
        assert_eq!(bitmap.count(), 7);
        assert!(!bitmap.contains(0));
        assert!(bitmap.contains(7));
        // Output and first record leave no work for the dependency.
        let limits = Limits {
            max_decoded_words: 2,
            ..Default::default()
        };
        assert!(matches!(
            index.reachable_with(1, limits, &AtomicBool::new(false)),
            Err(Error::Limit(_))
        ));
    }

    #[test]
    fn checksum_valid_invalid_lookup_xor_and_type_partition_fail() {
        let (anchor, original) = file();
        let parsed = BitmapIndex::parse(&original, anchor, 8).unwrap();
        let table = original.len() - anchor.as_bytes().len() - 32;
        for (offset, value) in [
            (table, 7),                        // Lookup position differs from its record.
            (table + 12, 0),                   // Non-XOR record has a parent.
            (parsed.entries[1].offset + 4, 2), // XOR points before first entry.
            (12 + anchor.as_bytes().len() + 8 + 8 + 7, 254), // Incomplete type partition.
        ] {
            let mut bytes = original.clone();
            bytes[offset] = value;
            let end = bytes.len() - anchor.as_bytes().len();
            let checksum = anchor.format().checksum(&bytes[..end]);
            bytes[end..].copy_from_slice(checksum.as_bytes());
            assert!(BitmapIndex::parse(&bytes, anchor, 8).is_err());
        }
    }
}
