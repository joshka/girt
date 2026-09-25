use std::io::{self, Write};

use flate2::Compression;
use flate2::write::ZlibEncoder;

use super::compression::{DeltaOptions, DeltaStats, PackCompression, instructions};
use crate::object::Hasher;
use crate::{ObjectId, ObjectKind};

/// One format-bearing object to export, borrowing its exact uncompressed payload.
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
/// borrowed and compressed directly to the sink by the ordinary policy. Delta compression adds
/// the scratch/work bounds documented on [`DeltaOptions`]. These are not total process heap limits.
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
    /// Repository-format checksum of the pack before its trailer, suitable for a `pack-<hash>`
    /// basename.
    pub checksum: ObjectId,
    /// Number of distinct objects emitted.
    pub objects: u32,
    /// Total pack bytes, including its checksum.
    pub pack_bytes: u64,
    /// Total index bytes, including both checksums.
    pub index_bytes: u64,
    /// Delta search and dependency accounting; zero for the ordinary path.
    pub deltas: DeltaStats,
}

/// Failure to validate inputs or finish a caller-owned artifact pair.
#[derive(Debug, thiserror::Error)]
pub enum PackWriteError {
    /// A supplied identity differs from the selected pack format.
    #[error(transparent)]
    ObjectFormat(#[from] crate::ObjectFormatError),
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

/// Writes a pack v2 and matching index v2 in the explicitly selected object format.
///
/// `format` selects object IDs, REF_DELTA bases and both artifacts' checksums, including for an
/// empty set. Every input must use that format; no format inference or translation occurs.
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
/// Input count, payload bounds, identity formats/digests, and conflicting duplicates are checked
/// before either sink is touched. Output bounds, I/O, or flush failures can leave partial artifacts
/// (or a complete pack and incomplete index). Discard both on any error. Output counters use
/// checked `u64` arithmetic; index offsets at or above 2 GiB use the v2 large-offset table. The
/// number of large offsets cannot exceed 2^31. Allocation failure follows Rust's allocator
/// behavior.
///
/// ```
/// use girt::{ObjectId, ObjectKind, PackObject, PackWriteLimits, write_pack};
/// let input = PackObject {
///     id: ObjectId::for_blob(girt::ObjectFormat::Sha1, b"hello"),
///     kind: ObjectKind::Blob,
///     data: b"hello",
/// };
/// let (mut pack, mut index) = (Vec::new(), Vec::new());
/// let written = write_pack(
///     girt::ObjectFormat::Sha1,
///     &[input, input],
///     &mut pack,
///     &mut index,
///     PackWriteLimits::default(),
/// )?;
/// assert_eq!(written.objects, 1);
/// # Ok::<(), girt::PackWriteError>(())
/// ```
pub fn write_pack(
    format: crate::ObjectFormat,
    objects: &[PackObject<'_>],
    pack: &mut impl Write,
    index: &mut impl Write,
    limits: PackWriteLimits,
) -> Result<PackWritten, PackWriteError> {
    write_pack_with_compression(
        format,
        objects,
        pack,
        index,
        limits,
        PackCompression::Ordinary,
    )
}

/// Writes a pack/index pair with explicit bounded compression selection.
///
/// See [`write_pack`] for validation, ownership, output bounds, and partial-artifact errors, and
/// [`DeltaOptions`] for candidate ordering, memory/work bounds, reproducibility, and wire encoding.
/// The ordinary policy produces exactly [`write_pack`]'s bytes. Delta search exhaustion falls back
/// to the best completed entry, including ordinary encoding; it is not an error.
///
/// # Errors
///
/// Returns the same validation, resource, and I/O errors as [`write_pack`].
///
/// ```
/// use girt::{
///     DeltaOptions, ObjectId, ObjectKind, PackCompression, PackObject, PackWriteLimits,
///     write_pack_with_compression,
/// };
/// let base: Vec<u8> = (0..=255).cycle().take(4096).collect();
/// let mut edited = base.clone();
/// edited[1000] ^= 255;
/// let inputs: Vec<_> = [&base, &edited]
///     .into_iter()
///     .map(|data| PackObject {
///         id: ObjectId::for_blob(girt::ObjectFormat::Sha1, data),
///         kind: ObjectKind::Blob,
///         data,
///     })
///     .collect();
/// let (mut pack, mut index) = (Vec::new(), Vec::new());
/// let policy = PackCompression::Delta(DeltaOptions {
///     max_depth: 2,
///     ..DeltaOptions::default()
/// });
/// let written = write_pack_with_compression(
///     girt::ObjectFormat::Sha1,
///     &inputs,
///     &mut pack,
///     &mut index,
///     PackWriteLimits::default(),
///     policy,
/// )?;
/// assert_eq!(written.objects, 2);
/// assert_eq!(written.deltas.entries, 1);
/// # Ok::<(), girt::PackWriteError>(())
/// ```
pub fn write_pack_with_compression(
    format: crate::ObjectFormat,
    objects: &[PackObject<'_>],
    pack: &mut impl Write,
    index: &mut impl Write,
    limits: PackWriteLimits,
    compression: PackCompression,
) -> Result<PackWritten, PackWriteError> {
    write_controlled(
        format,
        objects,
        pack,
        index,
        limits,
        compression,
        &mut || Ok(()),
    )
}

pub(crate) fn write_controlled(
    format: crate::ObjectFormat,
    objects: &[PackObject<'_>],
    pack: &mut impl Write,
    index: &mut impl Write,
    limits: PackWriteLimits,
    compression: PackCompression,
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
) -> Result<PackWritten, PackWriteError> {
    let objects = validate(format, objects, limits, check)?;
    check()?;
    let count = objects.len() as u32;
    let mut pack = Output::new(format, pack, limits.max_pack_bytes, "pack bytes");
    pack.put(b"PACK")?;
    pack.put(&2u32.to_be_bytes())?;
    pack.put(&count.to_be_bytes())?;
    let mut entries = Vec::with_capacity(objects.len());
    let mut depths = Vec::with_capacity(objects.len());
    let mut deltas = DeltaStats::default();
    for (position, object) in objects.iter().enumerate() {
        check()?;
        let selected = match compression {
            PackCompression::Ordinary => None,
            PackCompression::Delta(options) => {
                select_entry(&objects, position, &depths, options, &mut deltas, check)?
            }
        };
        let offset = pack.bytes;
        pack.crc = crc32fast::Hasher::new();
        if let Some(selected) = selected {
            pack.put(&selected.bytes)?;
            depths.push(selected.depth);
            if selected.depth != 0 {
                deltas.entries += 1;
                deltas.max_depth = deltas.max_depth.max(selected.depth);
            }
        } else {
            pack.put(&entry_header(object.kind, object.data.len() as u64))?;
            // Finish explicitly so buffered-write failures are reported on the success path.
            let mut encoder = ZlibEncoder::new(&mut pack, Compression::new(6));
            encoder.write_all(object.data).map_err(output_error)?;
            encoder.finish().map_err(output_error)?;
            depths.push(0);
        }
        entries.push(Entry {
            id: object.id,
            offset,
            crc: pack.crc.clone().finalize(),
        });
    }
    let checksum = pack.finish()?;
    let pack_bytes = pack.bytes;
    let mut index = Output::new(format, index, limits.max_index_bytes, "index bytes");
    write_index(&entries, checksum, &mut index)?;
    index.finish()?;
    Ok(PackWritten {
        checksum,
        objects: count,
        pack_bytes,
        index_bytes: index.bytes,
        deltas,
    })
}

struct SelectedEntry {
    bytes: Vec<u8>,
    depth: usize,
}

fn select_entry(
    objects: &[PackObject<'_>],
    position: usize,
    depths: &[usize],
    options: DeltaOptions,
    stats: &mut DeltaStats,
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
) -> Result<Option<SelectedEntry>, PackWriteError> {
    let object = objects[position];
    let length = object.data.len();
    if length < 8
        || length > options.max_object_bytes
        || length > u32::MAX as usize
        || options.max_depth == 0
        || options.max_candidates == 0
        || options.max_work == 0
        || options.window == 0
    {
        return Ok(None);
    }
    let candidates = (position.saturating_sub(options.window)..position)
        .rev()
        .filter(|&i| {
            let base = objects[i];
            base.kind == object.kind
                && depths[i] < options.max_depth
                && base.data.len() <= options.max_object_bytes
                && base.data.len() <= u32::MAX as usize
                && base.data.len() >= length.div_ceil(2)
                && base.data.len().div_ceil(2) <= length
        })
        .take(options.max_candidates);
    let mut best: Option<SelectedEntry> = None;
    let mut ordinary_cost = 0;
    let mut remaining = options.max_work;
    for i in candidates {
        check()?;
        if best.is_none() {
            let bytes = compressed(entry_header(object.kind, length as u64), object.data)?;
            ordinary_cost = bytes.len();
            best = Some(SelectedEntry { bytes, depth: 0 });
            // Even before zlib bytes, a REF_DELTA needs a header and a format-sized base ID.
            if ordinary_cost.saturating_sub(1 + object.id.format().digest_len())
                <= options.min_savings
            {
                break;
            }
        }
        stats.candidates += 1;
        let before = remaining;
        let program = instructions(objects[i].data, object.data, &mut remaining, check)?;
        stats.work = stats.work.saturating_add(before - remaining);
        let Some(program) = program else { break };
        check()?;
        let mut header = entry_header_code(7, program.len() as u64);
        header.extend_from_slice(objects[i].id.as_bytes());
        let bytes = compressed(header, &program)?;
        let current = best.as_ref().unwrap();
        if bytes.len() < current.bytes.len()
            && ordinary_cost.saturating_sub(bytes.len()) >= options.min_savings
        {
            best = Some(SelectedEntry {
                bytes,
                depth: depths[i] + 1,
            });
        }
    }
    Ok(best)
}

fn compressed(header: Vec<u8>, data: &[u8]) -> Result<Vec<u8>, PackWriteError> {
    let mut encoder = ZlibEncoder::new(header, Compression::new(6));
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

fn validate<'a>(
    format: crate::ObjectFormat,
    objects: &[PackObject<'a>],
    limits: PackWriteLimits,
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
) -> Result<Vec<PackObject<'a>>, PackWriteError> {
    check()?;
    if objects.len() as u64 > u64::from(limits.max_objects) {
        return Err(PackWriteError::Limit("input occurrences"));
    }
    let mut total = 0u64;
    for object in objects {
        object.id.require_format(format)?;
        check()?;
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
    check()?;
    let mut sorted = objects.to_vec();
    sorted.sort_unstable_by_key(|object| object.id);
    for pair in sorted.windows(2) {
        check()?;
        if pair[0].id == pair[1].id
            && (pair[0].kind != pair[1].kind || pair[0].data != pair[1].data)
        {
            return Err(PackWriteError::ConflictingDuplicate(pair[0].id));
        }
    }
    sorted.dedup_by_key(|object| object.id);
    for object in &sorted {
        check()?;
        let actual = format.hash_object(object.kind, object.data);
        if actual != object.id {
            return Err(PackWriteError::Identity {
                expected: object.id,
                actual,
            });
        }
    }
    Ok(sorted)
}

fn entry_header(kind: ObjectKind, size: u64) -> Vec<u8> {
    let code = match kind {
        ObjectKind::Commit => 1,
        ObjectKind::Tree => 2,
        ObjectKind::Blob => 3,
        ObjectKind::Tag => 4,
    };
    entry_header_code(code, size)
}

fn entry_header_code(code: u8, mut size: u64) -> Vec<u8> {
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

pub(crate) struct Entry {
    pub id: ObjectId,
    pub offset: u64,
    pub crc: u32,
}

pub(crate) fn encode_index(
    entries: &[Entry],
    checksum: ObjectId,
) -> Result<Vec<u8>, PackWriteError> {
    let mut bytes = Vec::new();
    let mut out = Output::new(checksum.format(), &mut bytes, u64::MAX, "index bytes");
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
    hash: Hasher,
    crc: crc32fast::Hasher,
}

impl<'a, W: Write> Output<'a, W> {
    fn new(format: crate::ObjectFormat, sink: &'a mut W, limit: u64, label: &'static str) -> Self {
        Self {
            sink,
            bytes: 0,
            limit,
            label,
            hash: Hasher::new(format),
            crc: crc32fast::Hasher::new(),
        }
    }
    fn put(&mut self, bytes: &[u8]) -> Result<(), PackWriteError> {
        self.write_all(bytes).map_err(output_error)
    }
    fn finish(&mut self) -> Result<ObjectId, PackWriteError> {
        let checksum = self.hash.clone().finalize();
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
            id: ObjectId::for_blob(crate::ObjectFormat::Sha1, data),
            kind: ObjectKind::Blob,
            data,
        }
    }

    #[test]
    fn order_and_exact_duplicates_do_not_change_artifacts() {
        let (a, b) = (blob(b"a"), blob(b"b"));
        let (mut pack, mut idx, mut other_pack, mut other_idx) = (vec![], vec![], vec![], vec![]);
        let result = write_pack(
            crate::ObjectFormat::Sha1,
            &[a, b, a],
            &mut pack,
            &mut idx,
            PackWriteLimits::default(),
        )
        .unwrap();
        write_pack(
            crate::ObjectFormat::Sha1,
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
    #[case::wrong_id(PackObject { id: ObjectId::Sha1([0;20]), ..blob(b"x") })]
    #[case::wrong_kind(PackObject { kind: ObjectKind::Tree, ..blob(b"x") })]
    fn invalid_identity_leaves_outputs_untouched(#[case] input: PackObject<'_>) {
        let (mut pack, mut idx) = (vec![], vec![]);
        assert!(matches!(
            write_pack(
                crate::ObjectFormat::Sha1,
                &[input],
                &mut pack,
                &mut idx,
                PackWriteLimits::default()
            ),
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
                crate::ObjectFormat::Sha1,
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
            write_pack(
                crate::ObjectFormat::Sha1,
                &[blob(b"a"), blob(b"a")],
                &mut pack,
                &mut idx,
                limits
            ),
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
            matches!(write_pack(crate::ObjectFormat::Sha1, &[blob(b"a")], &mut pack, &mut idx, limits), Err(PackWriteError::Limit(actual)) if actual == label)
        );
        assert!(pack.len() as u64 <= pack_limit);
        assert!(idx.len() as u64 <= index_limit);
    }

    #[test]
    fn exact_limits_succeed() {
        let (mut pack, mut idx) = (vec![], vec![]);
        let first = write_pack(
            crate::ObjectFormat::Sha1,
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
        let result = write_pack(
            crate::ObjectFormat::Sha1,
            &[blob(b"a")],
            &mut vec![],
            &mut vec![],
            limits,
        )
        .unwrap();
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
                crate::ObjectFormat::Sha1,
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
            crate::ObjectFormat::Sha1,
            &[blob(b"abc")],
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
        )
        .unwrap();
        write_pack(
            crate::ObjectFormat::Sha1,
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
                id: ObjectId::Sha1([0; 20]),
                offset: 12,
                crc: 0x12345678,
            },
            Entry {
                id: ObjectId::Sha1([1; 20]),
                offset: 0x7fff_ffff,
                crc: 1,
            },
            Entry {
                id: ObjectId::Sha1([2; 20]),
                offset: 0x8000_0000,
                crc: 2,
            },
            Entry {
                id: ObjectId::Sha1([3; 20]),
                offset: 0x1_0000_0000,
                crc: 3,
            },
        ];
        let mut bytes = vec![];
        let mut output = Output::new(
            crate::ObjectFormat::Sha1,
            &mut bytes,
            u64::MAX,
            "index bytes",
        );
        write_index(&entries, ObjectId::Sha1([9; 20]), &mut output).unwrap();
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
        let parsed =
            crate::pack::index::Index::parse(crate::ObjectFormat::Sha1, &bytes, 0x1_0000_0001)
                .unwrap();
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
            crate::ObjectFormat::Sha1,
            &[PackObject { id, kind, data }],
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
        )
        .unwrap();
        let reader = crate::pack::Pack::open(crate::ObjectFormat::Sha1, &index, pack).unwrap();
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
        let written = write_pack(
            crate::ObjectFormat::Sha1,
            &[],
            &mut vec![],
            &mut vec![],
            limits,
        )
        .unwrap();
        assert_eq!(written.objects, 0);
        assert_eq!(written.pack_bytes, 32);
        assert_eq!(written.index_bytes, 1072);
    }

    #[test]
    fn zero_progress_sink_returns_write_zero() {
        let mut empty = &mut [][..];
        let error = write_pack(
            crate::ObjectFormat::Sha1,
            &[],
            &mut empty,
            &mut vec![],
            PackWriteLimits::default(),
        )
        .unwrap_err();
        assert!(
            matches!(error, PackWriteError::Io(error) if error.kind() == io::ErrorKind::WriteZero)
        );
    }

    #[test]
    fn output_counter_overflow_fails_before_sink() {
        let mut bytes = vec![];
        let mut output = Output::new(
            crate::ObjectFormat::Sha1,
            &mut bytes,
            u64::MAX,
            "pack bytes",
        );
        output.bytes = u64::MAX;
        assert!(matches!(
            output.put(b"a"),
            Err(PackWriteError::Limit("pack bytes"))
        ));
        assert!(bytes.is_empty());
    }
}

#[cfg(test)]
mod compression_tests {
    use rstest::rstest;

    use super::*;

    fn payloads() -> Vec<Vec<u8>> {
        (0..12)
            .map(|variant| {
                let mut state = 123456789u64;
                let mut bytes: Vec<u8> = (0..16384)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        state as u8
                    })
                    .collect();
                bytes[variant * 100] ^= 255;
                bytes
            })
            .collect()
    }
    fn export(data: &[Vec<u8>], options: DeltaOptions) -> (PackWritten, Vec<u8>, Vec<u8>) {
        let inputs: Vec<_> = data
            .iter()
            .map(|data| PackObject {
                id: ObjectId::for_blob(crate::ObjectFormat::Sha1, data),
                kind: ObjectKind::Blob,
                data,
            })
            .collect();
        let (mut pack, mut index) = (vec![], vec![]);
        let written = write_pack_with_compression(
            crate::ObjectFormat::Sha1,
            &inputs,
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
            PackCompression::Delta(options),
        )
        .unwrap();
        (written, pack, index)
    }

    #[rstest]
    #[case::window(DeltaOptions { window: 0, ..DeltaOptions::default() })]
    #[case::candidates(DeltaOptions { max_candidates: 0, ..DeltaOptions::default() })]
    #[case::depth(DeltaOptions { max_depth: 0, ..DeltaOptions::default() })]
    #[case::size(DeltaOptions { max_object_bytes: 0, ..DeltaOptions::default() })]
    #[case::work(DeltaOptions { max_work: 0, ..DeltaOptions::default() })]
    #[case::exhausted(DeltaOptions { max_work: 1, ..DeltaOptions::default() })]
    #[case::savings(DeltaOptions { min_savings: usize::MAX, ..DeltaOptions::default() })]
    fn ordinary_fallback_is_byte_identical(#[case] options: DeltaOptions) {
        let data = payloads();
        let (written, pack, index) = export(&data, options);
        let (_, ordinary, ordinary_index) = export(
            &data,
            DeltaOptions {
                window: 0,
                ..options
            },
        );
        assert_eq!(written.deltas.entries, 0);
        assert_eq!(pack, ordinary);
        assert_eq!(index, ordinary_index);
    }

    #[rstest]
    #[case::one(1, 1, 1)]
    #[case::two(3, 2, 2)]
    #[case::default(16, 4, 4)]
    fn bounded_deterministic_internal_dependencies(
        #[case] window: usize,
        #[case] candidates: usize,
        #[case] depth: usize,
    ) {
        let mut data = payloads();
        let options = DeltaOptions {
            window,
            max_candidates: candidates,
            max_depth: depth,
            ..DeltaOptions::default()
        };
        let (written, pack, index) = export(&data, options);
        data.reverse();
        data.push(data[0].clone());
        let (_, other, other_index) = export(&data, options);
        assert_eq!(pack, other);
        assert_eq!(index, other_index);
        assert!(written.deltas.entries > 0);
        assert!(written.deltas.max_depth <= depth);
        assert!(written.deltas.candidates <= 12 * candidates.min(window) as u64);
        assert!(written.deltas.work <= 12 * options.max_work);
        let reader = crate::pack::Pack::open(crate::ObjectFormat::Sha1, &index, pack).unwrap();
        verify_payloads(&reader, &data, written.deltas.max_depth);
    }
    fn verify_payloads(reader: &crate::pack::Pack, data: &[Vec<u8>], max_delta_depth: usize) {
        for expected in data {
            let restored = reader
                .read(
                    reader
                        .find(ObjectId::for_blob(crate::ObjectFormat::Sha1, expected))
                        .unwrap(),
                    crate::ReadLimits {
                        max_delta_depth,
                        ..crate::ReadLimits::default()
                    },
                )
                .unwrap();
            assert_eq!(restored.data(), expected);
        }
    }

    #[rstest]
    #[case::blob(ObjectKind::Blob)]
    #[case::tree(ObjectKind::Tree)]
    #[case::commit(ObjectKind::Commit)]
    #[case::tag(ObjectKind::Tag)]
    fn matching_is_byte_oriented_for_every_kind(#[case] kind: ObjectKind) {
        let data = payloads();
        let inputs: Vec<_> = data
            .iter()
            .map(|data| PackObject {
                id: ObjectId::for_object(kind.as_str(), data),
                kind,
                data,
            })
            .collect();
        let (mut pack, mut index) = (vec![], vec![]);
        let written = write_pack_with_compression(
            crate::ObjectFormat::Sha1,
            &inputs,
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
            PackCompression::Delta(DeltaOptions::default()),
        )
        .unwrap();
        assert!(written.deltas.entries > 0);
        let reader = crate::pack::Pack::open(crate::ObjectFormat::Sha1, &index, pack).unwrap();
        let restored = reader
            .read(
                reader.find(inputs[0].id).unwrap(),
                crate::ReadLimits::default(),
            )
            .unwrap();
        assert_eq!(restored.kind(), kind);
        assert_eq!(restored.data(), data[0]);
    }

    #[test]
    fn different_kinds_are_never_candidates() {
        let data = payloads();
        let inputs = [
            PackObject {
                id: ObjectId::for_blob(crate::ObjectFormat::Sha1, &data[0]),
                kind: ObjectKind::Blob,
                data: &data[0],
            },
            PackObject {
                id: ObjectId::for_object("tree", &data[1]),
                kind: ObjectKind::Tree,
                data: &data[1],
            },
        ];
        let written = write_pack_with_compression(
            crate::ObjectFormat::Sha1,
            &inputs,
            &mut vec![],
            &mut vec![],
            PackWriteLimits::default(),
            PackCompression::Delta(DeltaOptions::default()),
        )
        .unwrap();
        assert_eq!(written.deltas.candidates, 0);
    }

    #[test]
    fn delta_output_bound_includes_base_id_and_trailer() {
        let data = payloads();
        let (written, _, _) = export(&data, DeltaOptions::default());
        let inputs: Vec<_> = data
            .iter()
            .map(|data| PackObject {
                id: ObjectId::for_blob(crate::ObjectFormat::Sha1, data),
                kind: ObjectKind::Blob,
                data,
            })
            .collect();
        let limits = PackWriteLimits {
            max_pack_bytes: written.pack_bytes - 1,
            ..PackWriteLimits::default()
        };
        let (mut pack, mut index) = (vec![], vec![]);
        let result = write_pack_with_compression(
            crate::ObjectFormat::Sha1,
            &inputs,
            &mut pack,
            &mut index,
            limits,
            PackCompression::Delta(DeltaOptions::default()),
        );
        assert!(matches!(result, Err(PackWriteError::Limit("pack bytes"))));
        assert!(pack.len() as u64 <= limits.max_pack_bytes);
        assert!(index.is_empty());
    }

    #[test]
    fn exhausted_later_candidate_retains_completed_delta() {
        let data = payloads();
        let options = DeltaOptions {
            max_candidates: 4,
            max_work: 19000,
            ..DeltaOptions::default()
        };
        let (written, pack, index) = export(&data, options);
        assert!(written.deltas.entries > 0);
        assert!(written.deltas.candidates > u64::from(written.deltas.entries));
        assert!(written.deltas.work <= 12 * options.max_work);
        let reader = crate::pack::Pack::open(crate::ObjectFormat::Sha1, &index, pack).unwrap();
        verify_payloads(&reader, &data, written.deltas.max_depth);
    }

    #[test]
    fn objects_above_search_size_remain_streaming() {
        let data = payloads();
        let (written, _, _) = export(
            &data,
            DeltaOptions {
                max_object_bytes: 16383,
                ..DeltaOptions::default()
            },
        );
        assert_eq!(written.deltas.entries, 0);
        assert_eq!(written.deltas.candidates, 0);
        assert_eq!(written.deltas.work, 0);
    }

    #[test]
    fn short_objects_cannot_recover_ref_delta_overhead() {
        let data = vec![b"abcdefgh".to_vec(), b"abcdefgi".to_vec()];
        let (written, _, _) = export(&data, DeltaOptions::default());
        assert_eq!(written.deltas.candidates, 0);
    }

    #[test]
    fn invalid_identity_precedes_delta_output() {
        let input = PackObject {
            id: ObjectId::Sha1([0; 20]),
            kind: ObjectKind::Blob,
            data: &[1; 100],
        };
        let (mut pack, mut index) = (vec![], vec![]);
        let result = write_pack_with_compression(
            crate::ObjectFormat::Sha1,
            &[input],
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
            PackCompression::Delta(DeltaOptions::default()),
        );
        assert!(matches!(result, Err(PackWriteError::Identity { .. })));
        assert!(pack.is_empty());
        assert!(index.is_empty());
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[test]
    fn prevalidation_cancellation_leaves_both_outputs_untouched() {
        let objects = [
            PackObject {
                id: ObjectId::for_blob(crate::ObjectFormat::Sha1, b"first"),
                kind: ObjectKind::Blob,
                data: b"first",
            },
            PackObject {
                id: ObjectId::Sha1([255; 20]),
                kind: ObjectKind::Blob,
                data: b"invalid identity",
            },
        ];
        let mut checks = 0;
        let (mut pack, mut index) = (vec![], vec![]);
        let result = write_controlled(
            crate::ObjectFormat::Sha1,
            &objects,
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
            PackCompression::Ordinary,
            &mut || {
                checks += 1;
                if checks == 7 {
                    Err(PackWriteError::Io(io::Error::other("cancelled")))
                } else {
                    Ok(())
                }
            },
        );
        assert!(
            matches!(result, Err(PackWriteError::Io(error)) if error.to_string() == "cancelled")
        );
        assert!(pack.is_empty());
        assert!(index.is_empty());
    }
}

#[cfg(test)]
mod format_boundary_tests {
    use super::*;

    #[test]
    fn rejects_sha256_before_writing_pack_or_index() {
        let object = PackObject {
            id: ObjectId::Sha256([1; 32]),
            kind: ObjectKind::Blob,
            data: b"a",
        };
        let mut pack = vec![];
        let mut index = vec![];
        let result = write_pack(
            crate::ObjectFormat::Sha1,
            &[object],
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
        );
        assert!(matches!(result, Err(PackWriteError::ObjectFormat(_))));
        assert!(pack.is_empty());
        assert!(index.is_empty());
    }
}

#[cfg(test)]
mod dual_format_tests {
    use rstest::rstest;

    use super::*;
    use crate::ObjectFormat;

    fn blob(format: ObjectFormat) -> PackObject<'static> {
        PackObject {
            id: ObjectId::for_blob(format, b"blob"),
            kind: ObjectKind::Blob,
            data: b"blob",
        }
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1, ObjectFormat::Sha256)]
    #[case::sha256(ObjectFormat::Sha256, ObjectFormat::Sha1)]
    fn rejects_mixed_inputs_before_either_sink(
        #[case] format: ObjectFormat,
        #[case] other: ObjectFormat,
    ) {
        let (mut pack, mut index) = (vec![], vec![]);
        let result = write_pack(
            format,
            &[blob(format), blob(other)],
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
        );
        assert!(matches!(result, Err(PackWriteError::ObjectFormat(_))));
        assert!(pack.is_empty());
        assert!(index.is_empty());
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn empty_artifact_bounds_include_both_trailers(#[case] format: ObjectFormat) {
        let limits = PackWriteLimits {
            max_objects: 0,
            max_object_bytes: 0,
            max_input_bytes: 0,
            max_pack_bytes: (12 + format.digest_len()) as u64,
            max_index_bytes: (1032 + 2 * format.digest_len()) as u64,
        };
        let (mut pack, mut index) = (vec![], vec![]);
        let written = write_pack(format, &[], &mut pack, &mut index, limits).unwrap();
        assert_eq!(written.pack_bytes, limits.max_pack_bytes);
        assert_eq!(written.index_bytes, limits.max_index_bytes);
        assert_eq!(written.checksum.format(), format);
        assert!(crate::pack::Pack::open(format, &index, pack).is_ok());
        let short = PackWriteLimits {
            max_index_bytes: limits.max_index_bytes - 1,
            ..limits
        };
        let (mut pack, mut index) = (vec![], vec![]);
        assert!(matches!(
            write_pack(format, &[], &mut pack, &mut index, short),
            Err(PackWriteError::Limit("index bytes"))
        ));
        assert_eq!(pack.len() as u64, limits.max_pack_bytes);
        assert!(index.len() as u64 <= short.max_index_bytes);
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn cancellation_during_validation_has_no_output(#[case] format: ObjectFormat) {
        let (mut pack, mut index) = (vec![], vec![]);
        let mut checks = 0;
        let result = write_controlled(
            format,
            &[blob(format)],
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
            PackCompression::Ordinary,
            &mut || {
                checks += 1;
                if checks == 2 {
                    Err(PackWriteError::Limit("injected cancellation"))
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(
            result,
            Err(PackWriteError::Limit("injected cancellation"))
        ));
        assert!(pack.is_empty());
        assert!(index.is_empty());
    }

    struct FlushFailure(Vec<u8>);
    impl Write for FlushFailure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("injected flush failure"))
        }
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn failed_pack_flush_leaves_index_untouched(#[case] format: ObjectFormat) {
        let mut pack = FlushFailure(vec![]);
        let mut index = vec![];
        assert!(matches!(
            write_pack(
                format,
                &[],
                &mut pack,
                &mut index,
                PackWriteLimits::default()
            ),
            Err(PackWriteError::Io(_))
        ));
        assert_eq!(pack.0.len(), 12 + format.digest_len());
        assert!(index.is_empty());
    }
}
