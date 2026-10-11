//! Repacking retains compressed payloads; only relocated OFS_DELTA framing changes.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::reader::{Header, Source, entry_header};
use super::write::{Entry, Output, entry_header_code, select_entry, validate, write_index};
use super::{
    DeltaStats, FilePack, PackCompression, PackObject, PackWriteError, PackWriteLimits, PackWritten,
};
use crate::{ObjectFormat, ObjectId};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReuseError {
    #[error(transparent)]
    Read(#[from] crate::ObjectReadError),
    #[error(transparent)]
    Write(#[from] PackWriteError),
}

pub(crate) struct ReusedEntry {
    pub pack: Arc<FilePack>,
    pub entry: super::index::Entry,
    pub header: Header,
    pub base: Option<ObjectId>,
}

impl ReusedEntry {
    pub(crate) fn new(
        pack: Arc<FilePack>,
        position: usize,
    ) -> Result<Self, crate::ObjectReadError> {
        let entry = pack.entry(position)?;
        let header = entry_header(pack.as_ref(), &entry, &pack.prefix(&entry)?)?;
        let base = header
            .base
            .map(|position| pack.entry(position).map(|entry| entry.id))
            .transpose()?;
        Ok(Self {
            pack,
            entry,
            header,
            base,
        })
    }
}

pub(crate) fn write_reused(
    format: ObjectFormat,
    reused: &mut [ReusedEntry],
    decoded: (&[PackObject<'_>], &[PackObject<'_>]),
    outputs: (&mut impl Write, &mut impl Write),
    limits: PackWriteLimits,
    compression: PackCompression,
    cancel: &AtomicBool,
) -> Result<PackWritten, ReuseError> {
    let mut check = || {
        if cancel.load(Ordering::Relaxed) {
            Err(PackWriteError::Io(std::io::ErrorKind::Interrupted.into()))
        } else {
            Ok(())
        }
    };
    let fallback = validate(format, decoded.0, limits, &mut check)?;
    let loose = validate(format, decoded.1, limits, &mut check)?;
    let ordinary = fallback.len();
    let mut objects = fallback;
    objects.extend(loose);
    let count = u32::try_from(reused.len() + objects.len())
        .map_err(|_| PackWriteError::Limit("object count"))?;
    if count > limits.max_objects {
        return Err(PackWriteError::Limit("object count").into());
    }
    // Keeping source order usually preserves backward bases and short offset encodings.
    reused.sort_unstable_by(|a, b| {
        a.pack
            .path()
            .cmp(b.pack.path())
            .then(a.entry.offset.cmp(&b.entry.offset))
    });
    let maximum_depth = reused_depth(reused, &objects, &mut check)?;
    let (pack, index) = outputs;
    let mut pack = Output::new(format, pack, limits.max_pack_bytes, "pack bytes");
    pack.put(b"PACK")?;
    pack.put(&2u32.to_be_bytes())?;
    pack.put(&count.to_be_bytes())?;
    let mut entries = Vec::with_capacity(count as usize);
    let mut offsets = HashMap::with_capacity(count as usize);
    let mut deltas = DeltaStats {
        max_depth: maximum_depth,
        ..Default::default()
    };
    for object in reused {
        check()?;
        let offset = pack.bytes;
        pack.crc = crc32fast::Hasher::new();
        let start = if object.header.kind == 6 {
            let base = object.base.expect("offset delta has a base");
            if let Some(base_offset) = offsets.get(&base) {
                pack.put(&entry_header_code(6, object.header.length as u64))?;
                pack.put(&offset_distance(offset - base_offset))?;
            } else {
                // A duplicate base selected from a later pack needs an identity reference.
                pack.put(&entry_header_code(7, object.header.length as u64))?;
                pack.put(base.as_bytes())?;
            }
            object.entry.offset + object.header.bytes
        } else {
            object.entry.offset
        };
        object
            .pack
            .copy_range(start, object.entry.end, &mut pack, cancel)?;
        if object.base.is_some() {
            deltas.entries += 1;
        }
        entries.push(Entry {
            id: object.entry.id,
            offset,
            crc: pack.crc.clone().finalize(),
        });
        offsets.insert(object.entry.id, offset);
    }
    let mut depths = Vec::with_capacity(objects.len());
    for (position, object) in objects.iter().enumerate() {
        check()?;
        let offset = pack.bytes;
        pack.crc = crc32fast::Hasher::new();
        let selected = match compression {
            PackCompression::Ordinary => None,
            PackCompression::Delta(options) if position >= ordinary => select_entry(
                &objects[ordinary..],
                position - ordinary,
                &depths[ordinary..],
                options,
                &mut deltas,
                &mut check,
            )?,
            PackCompression::Delta(_) => None,
        };
        if let Some(selected) = selected {
            pack.put(&selected.bytes)?;
            depths.push(selected.depth);
            if selected.depth != 0 {
                deltas.entries += 1;
                deltas.max_depth = deltas.max_depth.max(selected.depth);
            }
        } else {
            pack.put(&super::write::entry_header(
                object.kind,
                object.data.len() as u64,
            ))?;
            let mut encoder =
                flate2::write::ZlibEncoder::new(&mut pack, flate2::Compression::new(6));
            encoder
                .write_all(object.data)
                .map_err(super::write::output_error)?;
            encoder.finish().map_err(super::write::output_error)?;
            depths.push(0);
        }
        entries.push(Entry {
            id: object.id,
            offset,
            crc: pack.crc.clone().finalize(),
        });
    }
    check()?;
    let checksum = pack.finish()?;
    entries.sort_unstable_by_key(|entry| entry.id);
    let mut index = Output::new(format, index, limits.max_index_bytes, "index bytes");
    write_index(&entries, checksum, &mut index)?;
    index.finish()?;
    Ok(PackWritten {
        checksum,
        objects: count,
        pack_bytes: pack.bytes,
        index_bytes: index.bytes,
        deltas,
    })
}

// Verify the selected representation graph, since deduplication can select bases from other packs.
fn reused_depth(
    reused: &[ReusedEntry],
    decoded: &[PackObject<'_>],
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
) -> Result<usize, ReuseError> {
    let bases: HashMap<_, _> = reused
        .iter()
        .map(|entry| (entry.entry.id, entry.base))
        .collect();
    let mut depths: HashMap<_, usize> = decoded.iter().map(|object| (object.id, 0)).collect();
    let mut maximum = 0;
    for entry in reused {
        check()?;
        let mut chain = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut id = entry.entry.id;
        let mut depth = loop {
            check()?;
            if let Some(&depth) = depths.get(&id) {
                break depth;
            }
            if !seen.insert(id) {
                return Err(crate::ObjectReadError::DeltaCycle.into());
            }
            match bases.get(&id) {
                Some(Some(base)) => {
                    chain.push(id);
                    id = *base;
                }
                Some(None) => {
                    depths.insert(id, 0);
                    break 0;
                }
                None => return Err(crate::ObjectReadError::MissingBase(id).into()),
            }
        };
        while let Some(id) = chain.pop() {
            depth += 1;
            depths.insert(id, depth);
        }
        maximum = maximum.max(depth);
    }
    Ok(maximum)
}

// The offset encoding subtracts one from each remaining high group (pack-format specification).
fn offset_distance(mut distance: u64) -> Vec<u8> {
    let mut bytes = vec![(distance & 127) as u8];
    distance >>= 7;
    while distance != 0 {
        distance -= 1;
        bytes.push(128 | (distance & 127) as u8);
        distance >>= 7;
    }
    bytes.reverse();
    bytes
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;

    use rstest::rstest;

    use super::*;
    use crate::{PackLimits, ReadLimits};

    fn encode_size(mut size: usize, bytes: &mut Vec<u8>) {
        while size >= 128 {
            bytes.push(128 | (size & 127) as u8);
            size >>= 7;
        }
        bytes.push(size as u8);
    }

    fn compressed(header: Vec<u8>, data: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::ZlibEncoder::new(header, flate2::Compression::new(6));
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    // Original pack-format fixture: a base, an unrelated entry, and a literal delta.
    // The gap lets selection change an OFS_DELTA's distance without changing its program.
    pub(crate) fn fixture(
        root: &std::path::Path,
        format: ObjectFormat,
        code: u8,
    ) -> (Arc<FilePack>, ObjectId, ObjectId) {
        let base = b"base payload";
        let target = b"target payload";
        let base_id = ObjectId::for_blob(format, base);
        let target_id = ObjectId::for_blob(format, target);
        let mut program = Vec::new();
        encode_size(base.len(), &mut program);
        encode_size(target.len(), &mut program);
        program.push(target.len() as u8);
        program.extend_from_slice(target);
        let mut bytes = b"PACK\0\0\0\x02\0\0\0\x03".to_vec();
        let mut entries = Vec::new();
        append(
            &mut bytes,
            &mut entries,
            base_id,
            compressed(entry_header_code(3, base.len() as u64), base),
        );
        let gap = b"an unrelated payload";
        append(
            &mut bytes,
            &mut entries,
            ObjectId::for_blob(format, gap),
            compressed(entry_header_code(3, gap.len() as u64), gap),
        );
        let mut header = entry_header_code(code, program.len() as u64);
        if code == 6 {
            header.extend_from_slice(&offset_distance(bytes.len() as u64 - 12));
        } else {
            header.extend_from_slice(base_id.as_bytes());
        }
        append(
            &mut bytes,
            &mut entries,
            target_id,
            compressed(header, &program),
        );
        let mut hash = crate::object::Hasher::new(format);
        hash.update(&bytes);
        let checksum = hash.finalize();
        bytes.extend_from_slice(checksum.as_bytes());
        entries.sort_unstable_by_key(|entry| entry.id);
        let index = super::super::write::encode_index(&entries, checksum).unwrap();
        fs::create_dir_all(root).unwrap();
        let path = root.join("a.idx");
        fs::write(&path, index).unwrap();
        fs::write(path.with_extension("pack"), bytes).unwrap();
        let pack = FilePack::open(
            format,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        (Arc::new(pack), base_id, target_id)
    }

    fn append(bytes: &mut Vec<u8>, entries: &mut Vec<Entry>, id: ObjectId, entry: Vec<u8>) {
        entries.push(Entry {
            id,
            offset: bytes.len() as u64,
            crc: crc32fast::hash(&entry),
        });
        bytes.extend(entry);
    }

    fn selected(pack: &Arc<FilePack>, id: ObjectId) -> ReusedEntry {
        ReusedEntry::new(Arc::clone(pack), pack.find(id).unwrap().unwrap()).unwrap()
    }

    #[rstest]
    #[case::sha1_offset(ObjectFormat::Sha1, 6)]
    #[case::sha1_reference(ObjectFormat::Sha1, 7)]
    #[case::sha256_offset(ObjectFormat::Sha256, 6)]
    #[case::sha256_reference(ObjectFormat::Sha256, 7)]
    fn relocating_delta_preserves_compressed_program(
        #[case] format: ObjectFormat,
        #[case] code: u8,
    ) {
        let root = tempfile::tempdir().unwrap();
        let (source, base, target) = fixture(root.path(), format, code);
        let original = selected(&source, target);
        let mut expected = Vec::new();
        source
            .copy_range(
                original.entry.offset + original.header.bytes,
                original.entry.end,
                &mut expected,
                &AtomicBool::new(false),
            )
            .unwrap();
        let mut reused = [selected(&source, target), selected(&source, base)];
        let (mut pack, mut index) = (Vec::new(), Vec::new());
        let written = write_reused(
            format,
            &mut reused,
            (&[], &[]),
            (&mut pack, &mut index),
            PackWriteLimits::default(),
            PackCompression::Ordinary,
            &AtomicBool::new(false),
        )
        .unwrap();
        let result = super::super::reader::Pack::open(format, &index, pack.clone()).unwrap();
        let position = result.find(target).unwrap();
        let entry = result.entry(position).unwrap();
        let header = entry_header(&result, &entry, &result.prefix(&entry).unwrap()).unwrap();
        assert_eq!(&pack[entry.offset + header.bytes..entry.end], expected);
        assert_eq!(
            result.read(position, ReadLimits::default()).unwrap().data(),
            b"target payload"
        );
        assert_eq!(header.kind, code);
        assert_eq!(written.deltas.entries, 1);
        assert_eq!(written.deltas.max_depth, 1);
        assert_eq!(written.deltas.candidates, 0);
    }

    #[test]
    fn duplicate_base_from_later_pack_uses_reference_delta() {
        let root = tempfile::tempdir().unwrap();
        let format = ObjectFormat::Sha1;
        let (first, base, target) = fixture(&root.path().join("a"), format, 6);
        let (second, _, _) = fixture(&root.path().join("b"), format, 6);
        let mut reused = [selected(&first, target), selected(&second, base)];
        let (mut pack, mut index) = (Vec::new(), Vec::new());
        write_reused(
            format,
            &mut reused,
            (&[], &[]),
            (&mut pack, &mut index),
            PackWriteLimits::default(),
            PackCompression::Ordinary,
            &AtomicBool::new(false),
        )
        .unwrap();
        let result = super::super::reader::Pack::open(format, &index, pack).unwrap();
        let position = result.find(target).unwrap();
        let entry = result.entry(position).unwrap();
        let header = entry_header(&result, &entry, &result.prefix(&entry).unwrap()).unwrap();
        assert_eq!(header.kind, 7);
        assert_eq!(
            result.read(position, ReadLimits::default()).unwrap().data(),
            b"target payload"
        );
    }

    #[test]
    fn missing_selected_base_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let (source, base, target) = fixture(root.path(), ObjectFormat::Sha1, 6);
        let mut reused = [selected(&source, target)];
        let result = write_reused(
            ObjectFormat::Sha1,
            &mut reused,
            (&[], &[]),
            (&mut Vec::new(), &mut Vec::new()),
            PackWriteLimits::default(),
            PackCompression::Ordinary,
            &AtomicBool::new(false),
        );
        assert!(
            matches!(result, Err(ReuseError::Read(crate::ObjectReadError::MissingBase(id))) if id == base)
        );
    }

    #[test]
    fn selected_delta_cycle_fails_before_output() {
        let root = tempfile::tempdir().unwrap();
        let (source, _, target) = fixture(root.path(), ObjectFormat::Sha1, 7);
        let mut entry = selected(&source, target);
        entry.base = Some(target);
        let (mut pack, mut index) = (Vec::new(), Vec::new());
        let result = write_reused(
            ObjectFormat::Sha1,
            &mut [entry],
            (&[], &[]),
            (&mut pack, &mut index),
            PackWriteLimits::default(),
            PackCompression::Ordinary,
            &AtomicBool::new(false),
        );
        assert!(matches!(
            result,
            Err(ReuseError::Read(crate::ObjectReadError::DeltaCycle))
        ));
        assert!(pack.is_empty());
        assert!(index.is_empty());
    }

    #[test]
    fn truncation_of_pinned_source_is_rejected_during_copy() {
        let root = tempfile::tempdir().unwrap();
        let (source, base, target) = fixture(root.path(), ObjectFormat::Sha1, 6);
        let mut reused = [selected(&source, base), selected(&source, target)];
        let end = reused[1].entry.end;
        fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("a.pack"))
            .unwrap()
            .set_len((end - 1) as u64)
            .unwrap();
        let result = write_reused(
            ObjectFormat::Sha1,
            &mut reused,
            (&[], &[]),
            (&mut Vec::new(), &mut Vec::new()),
            PackWriteLimits::default(),
            PackCompression::Ordinary,
            &AtomicBool::new(false),
        );
        assert!(
            matches!(result, Err(ReuseError::Read(crate::ObjectReadError::Path { source, .. })) if source.kind() == std::io::ErrorKind::UnexpectedEof)
        );
    }

    #[test]
    fn copying_respects_output_limit() {
        let root = tempfile::tempdir().unwrap();
        let (source, base, _) = fixture(root.path(), ObjectFormat::Sha1, 6);
        let mut pack = Vec::new();
        let limits = PackWriteLimits {
            max_pack_bytes: 13,
            ..Default::default()
        };
        let result = write_reused(
            ObjectFormat::Sha1,
            &mut [selected(&source, base)],
            (&[], &[]),
            (&mut pack, &mut Vec::new()),
            limits,
            PackCompression::Ordinary,
            &AtomicBool::new(false),
        );
        assert!(matches!(
            result,
            Err(ReuseError::Write(PackWriteError::Limit("pack bytes")))
        ));
        assert!(pack.len() <= 13);
    }

    #[rstest]
    #[case::one(1)]
    #[case::largest_single_byte(127)]
    #[case::two_bytes(128)]
    #[case::three_bytes(16512)]
    #[case::largest(u64::MAX)]
    fn offset_encoding_round_trips(#[case] distance: u64) {
        let bytes = offset_distance(distance);
        let mut input = bytes.as_slice();
        let mut next = super::super::delta::byte(&mut input).unwrap();
        let mut actual = u64::from(next & 127);
        while next & 128 != 0 {
            next = super::super::delta::byte(&mut input).unwrap();
            actual = (actual + 1) * 128 + u64::from(next & 127);
        }
        assert_eq!(actual, distance);
        assert!(input.is_empty());
    }
}
