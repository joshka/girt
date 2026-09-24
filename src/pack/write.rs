use std::io::{self, Write};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use sha1::{Digest, Sha1};

use crate::{ObjectId, ObjectKind};

/// One explicit SHA-1 object to export, borrowing its exact uncompressed payload.
///
/// The writer verifies `id` against `kind` and `data`. Payload syntax and referenced object
/// existence are not checked; callers choose the object set and any graph validation. The kind
/// enum admits only blobs, trees, commits, and tags, never delta or reserved pack types.
#[derive(Debug, Clone, Copy)]
pub struct PackObject<'a> {
    /// Expected identity, including the canonical Git kind/length header in the hash.
    pub id: ObjectId,
    /// Logical object kind.
    pub kind: ObjectKind,
    /// Exact payload without loose-object framing.
    pub data: &'a [u8],
}

/// Resource bounds for [`write_pack`], independent of the destination's buffering.
///
/// Input occurrences (including duplicates) are charged before sorting or hashing. Memory used by
/// the writer is proportional to that count, plus fixed compression scratch space; payloads are
/// borrowed and compressed directly to the sink. These are not total process heap limits.
#[derive(Debug, Clone, Copy)]
pub struct PackWriteLimits {
    /// Maximum input occurrences, also bounded by `u32::MAX` (default one million).
    pub max_objects: u32,
    /// Maximum individual uncompressed payload length (default 64 MiB).
    pub max_object_bytes: u64,
    /// Maximum sum of input payload bytes, including duplicates (default 512 MiB).
    pub max_input_bytes: u64,
    /// Maximum pack bytes including header and checksum (default 512 MiB).
    pub max_pack_bytes: u64,
    /// Maximum index bytes including both checksums (default 64 MiB).
    pub max_index_bytes: u64,
}

impl Default for PackWriteLimits {
    fn default() -> Self {
        Self {
            max_objects: 1_000_000,
            max_object_bytes: 64 * 1024 * 1024,
            max_input_bytes: 512 * 1024 * 1024,
            max_pack_bytes: 512 * 1024 * 1024,
            max_index_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Completed artifact lengths and identity; no files have been installed by the library.
#[derive(Debug, Clone, Copy)]
pub struct PackWritten {
    /// SHA-1 checksum of the pack before its trailer, suitable for a `pack-<hash>` basename.
    pub checksum: ObjectId,
    /// Number of distinct objects emitted.
    pub objects: u32,
    /// Total pack bytes, including its checksum.
    pub pack_bytes: u64,
    /// Total index bytes, including both checksums.
    pub index_bytes: u64,
}

/// Failure to validate inputs or finish a caller-owned artifact pair.
#[derive(Debug, thiserror::Error)]
pub enum PackWriteError {
    /// An expected identity does not match the supplied kind and bytes.
    #[error("object identity mismatch: expected {expected}, computed {actual}")]
    Identity {
        /// Caller-supplied identity.
        expected: ObjectId,
        /// Identity computed from the supplied kind and bytes.
        actual: ObjectId,
    },
    /// Equal identities had different kinds or payloads; no collision recovery is attempted.
    #[error("conflicting input for object {0}")]
    ConflictingDuplicate(ObjectId),
    /// A configured resource bound or representable format limit was exceeded.
    #[error("pack writing limit exceeded: {0}")]
    Limit(&'static str),
    /// A destination or compression operation failed; partial artifacts must be discarded.
    #[error("pack artifact I/O failed")]
    Io(#[from] io::Error),
}

/// Writes a SHA-1 pack v2 and matching index v2 from an explicit set of objects.
///
/// Exact duplicates collapse to one entry. Both artifacts use ascending object-ID order,
/// independent of input order. Each ordinary entry uses zlib level 6, without deltas. Repeated
/// calls with the same objects and compression backend/version produce identical bytes; compressed
/// bytes are not promised stable across dependency upgrades. Similar objects can occupy much more
/// space than Git's delta-selected packs. No reachability selection or payload parsing occurs.
///
/// Sinks must be empty or positioned at the start of a separate artifact: offsets are relative to
/// this call's first byte. They need only implement [`Write`], not seeking. The pack is completed
/// and flushed before the index is written and flushed. Success does not promise disk durability.
/// The caller owns buffering, storage, cleanup, and installation; never pass live repository pack
/// paths directly. No existing packs or loose objects are removed. Validated received-pack
/// installation is available separately through [`crate::fetch`].
///
/// # Errors
///
/// Input count, payload bounds, identities, and conflicting duplicates are checked before either
/// sink is touched. Output bounds, I/O, or flush failures can leave partial artifacts (or a
/// complete pack and incomplete index). Discard both on any error. Output counters use checked
/// `u64` arithmetic; index offsets at or above 2 GiB use the v2 large-offset table. The number of
/// large offsets cannot exceed 2^31. Allocation failure follows Rust's allocator behavior.
///
/// ```
/// use girt::{ObjectId, ObjectKind, PackObject, PackWriteLimits, write_pack};
/// let input = PackObject {
///     id: ObjectId::for_blob(b"hello"),
///     kind: ObjectKind::Blob,
///     data: b"hello",
/// };
/// let (mut pack, mut index) = (Vec::new(), Vec::new());
/// let written = write_pack(
///     &[input, input],
///     &mut pack,
///     &mut index,
///     PackWriteLimits::default(),
/// )?;
/// assert_eq!(written.objects, 1);
/// # Ok::<(), girt::PackWriteError>(())
/// ```
pub fn write_pack(
    objects: &[PackObject<'_>],
    pack: &mut impl Write,
    index: &mut impl Write,
    limits: PackWriteLimits,
) -> Result<PackWritten, PackWriteError> {
    let objects = validate(objects, limits)?;
    let count = objects.len() as u32;
    let mut pack = Output::new(pack, limits.max_pack_bytes, "pack bytes");
    pack.put(b"PACK")?;
    pack.put(&2u32.to_be_bytes())?;
    pack.put(&count.to_be_bytes())?;
    let mut entries = Vec::with_capacity(objects.len());
    for object in objects {
        let offset = pack.bytes;
        pack.crc = crc32fast::Hasher::new();
        pack.put(&entry_header(object.kind, object.data.len() as u64))?;
        // Finish explicitly so buffered-write failures are reported on the success path.
        let mut encoder = ZlibEncoder::new(&mut pack, Compression::new(6));
        encoder.write_all(object.data).map_err(output_error)?;
        encoder.finish().map_err(output_error)?;
        entries.push(Entry {
            id: object.id,
            offset,
            crc: pack.crc.clone().finalize(),
        });
    }
    let checksum = pack.finish()?;
    let pack_bytes = pack.bytes;
    let mut index = Output::new(index, limits.max_index_bytes, "index bytes");
    write_index(&entries, checksum, &mut index)?;
    index.finish()?;
    Ok(PackWritten {
        checksum,
        objects: count,
        pack_bytes,
        index_bytes: index.bytes,
    })
}

fn validate<'a>(
    objects: &[PackObject<'a>],
    limits: PackWriteLimits,
) -> Result<Vec<PackObject<'a>>, PackWriteError> {
    if objects.len() as u64 > u64::from(limits.max_objects) {
        return Err(PackWriteError::Limit("input occurrences"));
    }
    let mut total = 0u64;
    for object in objects {
        let length = object.data.len() as u64;
        if length > limits.max_object_bytes {
            return Err(PackWriteError::Limit("object bytes"));
        }
        total = total
            .checked_add(length)
            .ok_or(PackWriteError::Limit("input bytes"))?;
        if total > limits.max_input_bytes {
            return Err(PackWriteError::Limit("input bytes"));
        }
    }
    let mut sorted = objects.to_vec();
    sorted.sort_unstable_by_key(|object| object.id);
    for pair in sorted.windows(2) {
        if pair[0].id == pair[1].id
            && (pair[0].kind != pair[1].kind || pair[0].data != pair[1].data)
        {
            return Err(PackWriteError::ConflictingDuplicate(pair[0].id));
        }
    }
    sorted.dedup_by_key(|object| object.id);
    for object in &sorted {
        let actual = ObjectId::for_object(object.kind.as_str(), object.data);
        if actual != object.id {
            return Err(PackWriteError::Identity {
                expected: object.id,
                actual,
            });
        }
    }
    Ok(sorted)
}

fn entry_header(kind: ObjectKind, mut size: u64) -> Vec<u8> {
    let code = match kind {
        ObjectKind::Commit => 1,
        ObjectKind::Tree => 2,
        ObjectKind::Blob => 3,
        ObjectKind::Tag => 4,
    };
    let mut byte = (code << 4) | (size as u8 & 15);
    size >>= 4;
    let mut bytes = Vec::with_capacity(10);
    while size != 0 {
        bytes.push(byte | 128);
        byte = size as u8 & 127;
        size >>= 7;
    }
    bytes.push(byte);
    bytes
}

pub(super) struct Entry {
    pub id: ObjectId,
    pub offset: u64,
    pub crc: u32,
}

pub(super) fn encode_index(
    entries: &[Entry],
    checksum: ObjectId,
) -> Result<Vec<u8>, PackWriteError> {
    let mut bytes = Vec::new();
    let mut out = Output::new(&mut bytes, u64::MAX, "index bytes");
    write_index(entries, checksum, &mut out)?;
    out.finish()?;
    Ok(bytes)
}

fn write_index(
    entries: &[Entry],
    checksum: ObjectId,
    out: &mut Output<'_, impl Write>,
) -> Result<(), PackWriteError> {
    out.put(b"\xfftOc\0\0\0\x02")?;
    let mut fanout = [0u32; 256];
    for entry in entries {
        fanout[entry.id.as_bytes()[0] as usize] += 1;
    }
    let mut total = 0u32;
    for bucket in fanout {
        total += bucket;
        out.put(&total.to_be_bytes())?;
    }
    for entry in entries {
        out.put(entry.id.as_bytes())?;
    }
    for entry in entries {
        out.put(&entry.crc.to_be_bytes())?;
    }
    let mut large = 0u32;
    for entry in entries {
        let offset = if entry.offset < 0x8000_0000 {
            entry.offset as u32
        } else {
            if large == 0x8000_0000 {
                return Err(PackWriteError::Limit("large offsets"));
            }
            let slot = large | 0x8000_0000;
            large += 1;
            slot
        };
        out.put(&offset.to_be_bytes())?;
    }
    for entry in entries {
        if entry.offset >= 0x8000_0000 {
            out.put(&entry.offset.to_be_bytes())?;
        }
    }
    out.put(checksum.as_bytes())
}

// Preserve our limit error through flate2's io::Write boundary without conflating sink errors.
#[derive(Debug, thiserror::Error)]
#[error("output limit: {0}")]
struct OutputLimit(&'static str);

fn output_error(error: io::Error) -> PackWriteError {
    if let Some(limit) = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<OutputLimit>())
    {
        PackWriteError::Limit(limit.0)
    } else {
        PackWriteError::Io(error)
    }
}

struct Output<'a, W> {
    sink: &'a mut W,
    bytes: u64,
    limit: u64,
    label: &'static str,
    hash: Sha1,
    crc: crc32fast::Hasher,
}

impl<'a, W: Write> Output<'a, W> {
    fn new(sink: &'a mut W, limit: u64, label: &'static str) -> Self {
        Self {
            sink,
            bytes: 0,
            limit,
            label,
            hash: Sha1::new(),
            crc: crc32fast::Hasher::new(),
        }
    }
    fn put(&mut self, bytes: &[u8]) -> Result<(), PackWriteError> {
        self.write_all(bytes).map_err(output_error)
    }
    fn finish(&mut self) -> Result<ObjectId, PackWriteError> {
        let checksum = ObjectId::from_bytes(self.hash.clone().finalize().into());
        self.put(checksum.as_bytes())?;
        self.flush()?;
        Ok(checksum)
    }
}

impl<W: Write> Write for Output<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self.bytes.checked_add(bytes.len() as u64);
        if end.is_none_or(|end| end > self.limit) {
            return Err(io::Error::other(OutputLimit(self.label)));
        }
        let written = self.sink.write(bytes)?;
        self.hash.update(&bytes[..written]);
        self.crc.update(&bytes[..written]);
        self.bytes += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn blob(data: &[u8]) -> PackObject<'_> {
        PackObject {
            id: ObjectId::for_blob(data),
            kind: ObjectKind::Blob,
            data,
        }
    }

    #[test]
    fn order_and_exact_duplicates_do_not_change_artifacts() {
        let (a, b) = (blob(b"a"), blob(b"b"));
        let (mut pack, mut idx, mut other_pack, mut other_idx) = (vec![], vec![], vec![], vec![]);
        let result =
            write_pack(&[a, b, a], &mut pack, &mut idx, PackWriteLimits::default()).unwrap();
        write_pack(
            &[b, a],
            &mut other_pack,
            &mut other_idx,
            PackWriteLimits::default(),
        )
        .unwrap();
        assert_eq!(result.objects, 2);
        assert_eq!(pack, other_pack);
        assert_eq!(idx, other_idx);
    }

    #[rstest]
    #[case::wrong_id(PackObject { id: ObjectId::from_bytes([0;20]), ..blob(b"x") })]
    #[case::wrong_kind(PackObject { kind: ObjectKind::Tree, ..blob(b"x") })]
    fn invalid_identity_leaves_outputs_untouched(#[case] input: PackObject<'_>) {
        let (mut pack, mut idx) = (vec![], vec![]);
        assert!(matches!(
            write_pack(&[input], &mut pack, &mut idx, PackWriteLimits::default()),
            Err(PackWriteError::Identity { .. })
        ));
        assert!(pack.is_empty());
        assert!(idx.is_empty());
    }

    #[test]
    fn conflicting_duplicates_leave_outputs_untouched() {
        let a = blob(b"a");
        let (mut pack, mut idx) = (vec![], vec![]);
        assert!(matches!(
            write_pack(
                &[a, PackObject { data: b"b", ..a }],
                &mut pack,
                &mut idx,
                PackWriteLimits::default()
            ),
            Err(PackWriteError::ConflictingDuplicate(_))
        ));
        assert!(pack.is_empty());
        assert!(idx.is_empty());
    }

    #[rstest]
    #[case::count(PackWriteLimits { max_objects: 1, ..PackWriteLimits::default() })]
    #[case::individual(PackWriteLimits { max_object_bytes: 0, ..PackWriteLimits::default() })]
    #[case::total_duplicates(PackWriteLimits { max_input_bytes: 1, ..PackWriteLimits::default() })]
    fn input_bounds_precede_output(#[case] limits: PackWriteLimits) {
        let (mut pack, mut idx) = (vec![], vec![]);
        assert!(matches!(
            write_pack(&[blob(b"a"), blob(b"a")], &mut pack, &mut idx, limits),
            Err(PackWriteError::Limit(_))
        ));
        assert!(pack.is_empty());
        assert!(idx.is_empty());
    }

    #[rstest]
    #[case::pack_header(0, 2000, "pack bytes")]
    #[case::compressed_entry(14, 2000, "pack bytes")]
    #[case::index_header(2000, 0, "index bytes")]
    #[case::index_trailer(2000, 1099, "index bytes")]
    fn output_bounds_never_overrun(
        #[case] pack_limit: u64,
        #[case] index_limit: u64,
        #[case] label: &str,
    ) {
        let limits = PackWriteLimits {
            max_pack_bytes: pack_limit,
            max_index_bytes: index_limit,
            ..PackWriteLimits::default()
        };
        let (mut pack, mut idx) = (vec![], vec![]);
        assert!(
            matches!(write_pack(&[blob(b"a")], &mut pack, &mut idx, limits), Err(PackWriteError::Limit(actual)) if actual == label)
        );
        assert!(pack.len() as u64 <= pack_limit);
        assert!(idx.len() as u64 <= index_limit);
    }

    #[test]
    fn exact_limits_succeed() {
        let (mut pack, mut idx) = (vec![], vec![]);
        let first = write_pack(
            &[blob(b"a")],
            &mut pack,
            &mut idx,
            PackWriteLimits::default(),
        )
        .unwrap();
        let limits = PackWriteLimits {
            max_objects: 1,
            max_object_bytes: 1,
            max_input_bytes: 1,
            max_pack_bytes: first.pack_bytes,
            max_index_bytes: first.index_bytes,
        };
        let result = write_pack(&[blob(b"a")], &mut vec![], &mut vec![], limits).unwrap();
        assert_eq!(result.checksum, first.checksum);
    }

    struct Sink {
        remaining: usize,
        fail_flush: bool,
        bytes: Vec<u8>,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::other("injected write failure"));
            }
            let n = bytes.len().min(self.remaining).min(3);
            self.bytes.extend_from_slice(&bytes[..n]);
            self.remaining -= n;
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                Err(io::Error::other("injected flush failure"))
            } else {
                Ok(())
            }
        }
    }

    #[rstest]
    #[case::pack_write(15, false, usize::MAX, false)]
    #[case::pack_flush(usize::MAX, true, usize::MAX, false)]
    #[case::index_write(usize::MAX, false, 1040, false)]
    #[case::index_flush(usize::MAX, false, usize::MAX, true)]
    fn sink_failures_are_reported(
        #[case] pack_remaining: usize,
        #[case] pack_flush: bool,
        #[case] index_remaining: usize,
        #[case] index_flush: bool,
    ) {
        let mut pack = Sink {
            remaining: pack_remaining,
            fail_flush: pack_flush,
            bytes: vec![],
        };
        let mut index = Sink {
            remaining: index_remaining,
            fail_flush: index_flush,
            bytes: vec![],
        };
        assert!(matches!(
            write_pack(
                &[blob(b"a")],
                &mut pack,
                &mut index,
                PackWriteLimits::default()
            ),
            Err(PackWriteError::Io(_))
        ));
    }

    #[test]
    fn short_writes_preserve_checksums() {
        let mut pack = Sink {
            remaining: usize::MAX,
            fail_flush: false,
            bytes: vec![],
        };
        let (mut index, mut expected_pack, mut expected_index) = (vec![], vec![], vec![]);
        write_pack(
            &[blob(b"abc")],
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
        )
        .unwrap();
        write_pack(
            &[blob(b"abc")],
            &mut expected_pack,
            &mut expected_index,
            PackWriteLimits::default(),
        )
        .unwrap();
        assert_eq!(pack.bytes, expected_pack);
        assert_eq!(index, expected_index);
    }

    #[rstest]
    #[case::empty(0, &[0x30])]
    #[case::fifteen(15, &[0x3f])]
    #[case::sixteen(16, &[0xb0, 1])]
    #[case::two_kib(2048, &[0xb0, 0x80, 1])]
    #[case::maximum(u64::MAX, &[0xbf,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,15])]
    fn original_size_header_vectors(#[case] size: u64, #[case] expected: &[u8]) {
        assert_eq!(entry_header(ObjectKind::Blob, size), expected);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn synthetic_large_offsets_use_ordered_64_bit_table() {
        let entries = [
            Entry {
                id: ObjectId::from_bytes([0; 20]),
                offset: 12,
                crc: 0x12345678,
            },
            Entry {
                id: ObjectId::from_bytes([1; 20]),
                offset: 0x7fff_ffff,
                crc: 1,
            },
            Entry {
                id: ObjectId::from_bytes([2; 20]),
                offset: 0x8000_0000,
                crc: 2,
            },
            Entry {
                id: ObjectId::from_bytes([3; 20]),
                offset: 0x1_0000_0000,
                crc: 3,
            },
        ];
        let mut bytes = vec![];
        let mut output = Output::new(&mut bytes, u64::MAX, "index bytes");
        write_index(&entries, ObjectId::from_bytes([9; 20]), &mut output).unwrap();
        output.finish().unwrap();
        assert_eq!(
            &bytes[1128..1144],
            &[
                0, 0, 0, 12, 0x7f, 0xff, 0xff, 0xff, 0x80, 0, 0, 0, 0x80, 0, 0, 1
            ]
        );
        assert_eq!(
            &bytes[1144..1160],
            &[0, 0, 0, 0, 0x80, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]
        );
        let parsed = crate::pack::index::Index::parse(&bytes, 0x1_0000_0001).unwrap();
        assert_eq!(parsed.entries[2].offset, 0x8000_0000);
        assert_eq!(parsed.entries[3].offset, 0x1_0000_0000);
    }

    #[rstest]
    #[case::tree(ObjectKind::Tree)]
    #[case::commit(ObjectKind::Commit)]
    #[case::tag(ObjectKind::Tag)]
    fn payload_syntax_is_preserved_without_parsing(#[case] kind: ObjectKind) {
        let data = b"not a structured object\0\xff";
        let id = ObjectId::for_object(kind.as_str(), data);
        let (mut pack, mut index) = (vec![], vec![]);
        write_pack(
            &[PackObject { id, kind, data }],
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
        )
        .unwrap();
        let reader = crate::pack::Pack::open(&index, pack).unwrap();
        let restored = reader
            .read(reader.find(id).unwrap(), crate::ReadLimits::default())
            .unwrap();
        assert_eq!(restored.kind(), kind);
        assert_eq!(restored.data(), data);
    }

    #[test]
    fn empty_set_accepts_zero_input_budgets() {
        let limits = PackWriteLimits {
            max_objects: 0,
            max_object_bytes: 0,
            max_input_bytes: 0,
            max_pack_bytes: 32,
            max_index_bytes: 1072,
        };
        let written = write_pack(&[], &mut vec![], &mut vec![], limits).unwrap();
        assert_eq!(written.objects, 0);
        assert_eq!(written.pack_bytes, 32);
        assert_eq!(written.index_bytes, 1072);
    }

    #[test]
    fn zero_progress_sink_returns_write_zero() {
        let mut empty = &mut [][..];
        let error =
            write_pack(&[], &mut empty, &mut vec![], PackWriteLimits::default()).unwrap_err();
        assert!(
            matches!(error, PackWriteError::Io(error) if error.kind() == io::ErrorKind::WriteZero)
        );
    }

    #[test]
    fn output_counter_overflow_fails_before_sink() {
        let mut bytes = vec![];
        let mut output = Output::new(&mut bytes, u64::MAX, "pack bytes");
        output.bytes = u64::MAX;
        assert!(matches!(
            output.put(b"a"),
            Err(PackWriteError::Limit("pack bytes"))
        ));
        assert!(bytes.is_empty());
    }
}
