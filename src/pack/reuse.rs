//! Pack writing that copies existing entries verbatim and searches deltas only for new objects.
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::atomic::AtomicBool;

use super::compression::{DeltaStats, PackCompression};
use super::file::{CopyError, FilePack};
use super::reader::{Source, entry_header};
use super::write::{
    Entry, Output, PackObject, PackWriteError, PackWriteLimits, PackWritten, entry_header_code,
    output_error, select_entry, validate, write_index, write_ordinary,
};
use crate::{ObjectId, ObjectReadError, ReadLimits};

/// One retained object already stored at `position` in `packs[pack]`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Packed {
    pub id: ObjectId,
    pub pack: usize,
    pub position: usize,
}

/// Failure while reading a source entry or writing the new pair.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReuseError {
    #[error("cannot read packed object {id}")]
    Read {
        id: ObjectId,
        #[source]
        source: ObjectReadError,
    },
    #[error(transparent)]
    Write(#[from] PackWriteError),
}

/// Writes a pack v2 and index v2 holding `packed` entries followed by `fresh` objects.
///
/// Packed entries keep their source order. A whole-object entry is copied byte for byte. A delta
/// entry is copied with its zlib stream unchanged when its base entry was written earlier from the
/// same source pack; its header is rewritten as an OFS_DELTA with the new distance. Any other delta
/// is reconstructed, identity-checked, and written whole. Raw copies check available source CRCs
/// over the bytes streamed, including original delta framing, without re-inflating. Legacy v1
/// indexes have no entry CRCs; retirement must still decode the replacement. Fresh objects are
/// identity-checked and may become
/// OFS_DELTA entries against earlier fresh objects under `compression`. Identities in `fresh`
/// must not repeat identities in `packed`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_reusing(
    format: crate::ObjectFormat,
    packs: &[&FilePack],
    packed: &[Packed],
    fresh: &[PackObject<'_>],
    outputs: (&mut impl Write, &mut impl Write),
    limits: PackWriteLimits,
    compression: PackCompression,
    read: (ReadLimits, &AtomicBool),
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
) -> Result<PackWritten, ReuseError> {
    let (pack_sink, index_sink) = outputs;
    let (read_limits, cancelled) = read;
    let fresh = validate(format, fresh, limits, check)?;
    let mut order = Vec::with_capacity(packed.len());
    let mut seen = HashSet::with_capacity(packed.len());
    for item in packed {
        item.id
            .require_format(format)
            .map_err(PackWriteError::from)?;
        if seen.insert(item.id) {
            let source = packs[item.pack];
            let entry = source
                .entry(item.position)
                .map_err(|source| ReuseError::Read {
                    id: item.id,
                    source,
                })?;
            order.push((item.pack, entry.offset, *item));
        }
    }
    for object in &fresh {
        if seen.contains(&object.id) {
            return Err(PackWriteError::ConflictingDuplicate(object.id).into());
        }
    }
    let count = u32::try_from(order.len() + fresh.len())
        .ok()
        .filter(|&count| count <= limits.max_objects)
        .ok_or(PackWriteError::Limit("object count"))?;
    order.sort_unstable_by_key(|&(pack, offset, _)| (pack, offset));
    check()?;

    let mut pack = Output::new(format, pack_sink, limits.max_pack_bytes, "pack bytes");
    pack.put(b"PACK")?;
    pack.put(&2u32.to_be_bytes())?;
    pack.put(&count.to_be_bytes())?;
    let mut entries = Vec::with_capacity(count as usize);
    let mut written: HashMap<(usize, usize), (u64, usize)> = HashMap::with_capacity(order.len());
    let mut deltas = DeltaStats::default();
    for &(source_index, _, item) in &order {
        check()?;
        let source = packs[source_index];
        let read_error = |source| ReuseError::Read {
            id: item.id,
            source,
        };
        let copy_error = |error| match error {
            CopyError::Read(source) => read_error(source),
            CopyError::Write(error) => ReuseError::Write(output_error(error)),
        };
        let entry = source.entry(item.position).map_err(read_error)?;
        let header = entry_header(source, &entry).map_err(read_error)?;
        let offset = pack.bytes;
        pack.crc = crc32fast::Hasher::new();
        let mut depth = 0;
        match header.base {
            None => source
                .copy_raw(&entry, &header, &mut pack)
                .map_err(copy_error)?,
            Some(base) => match written.get(&(source_index, base)) {
                Some(&(base_offset, base_depth)) => {
                    depth = base_depth + 1;
                    pack.put(&entry_header_code(6, header.length as u64))?;
                    pack.put(&offset_distance(offset - base_offset))?;
                    source
                        .copy_raw(&entry, &header, &mut pack)
                        .map_err(copy_error)?;
                    deltas.entries += 1;
                }
                None => {
                    let object = source
                        .read(item.position, read_limits, cancelled)
                        .map_err(read_error)?;
                    if object.data().len() as u64 > limits.max_object_bytes {
                        return Err(PackWriteError::Limit("object bytes").into());
                    }
                    write_ordinary(&mut pack, object.kind(), object.data())?;
                }
            },
        }
        deltas.max_depth = deltas.max_depth.max(depth);
        written.insert((source_index, item.position), (offset, depth));
        entries.push(Entry {
            id: item.id,
            offset,
            crc: pack.crc.clone().finalize(),
        });
    }

    let mut depths = Vec::with_capacity(fresh.len());
    let mut fresh_offsets = Vec::with_capacity(fresh.len());
    for position in 0..fresh.len() {
        check()?;
        let object = fresh[position];
        let offset = pack.bytes;
        pack.crc = crc32fast::Hasher::new();
        let selected = match compression {
            PackCompression::Ordinary => None,
            PackCompression::Delta(options) => {
                select_entry(&fresh, position, &depths, options, &mut deltas, check)?
            }
        };
        if let Some(selected) = selected {
            if let Some(base) = selected.base {
                // Fresh deltas use the OFS framing that later repacks preserve verbatim.
                let header_end = selected.data_start - format.digest_len();
                let mut header = selected.bytes[..header_end].to_vec();
                header[0] = (header[0] & !0x70) | 0x60;
                pack.put(&header)?;
                pack.put(&offset_distance(offset - fresh_offsets[base]))?;
                pack.put(&selected.bytes[selected.data_start..])?;
            } else {
                pack.put(&selected.bytes)?;
            }
            if selected.depth != 0 {
                deltas.entries += 1;
                deltas.max_depth = deltas.max_depth.max(selected.depth);
            }
            depths.push(selected.depth);
        } else {
            write_ordinary(&mut pack, object.kind, object.data)?;
            depths.push(0);
        }
        fresh_offsets.push(offset);
        entries.push(Entry {
            id: object.id,
            offset,
            crc: pack.crc.clone().finalize(),
        });
    }
    check()?;
    let checksum = pack.finish()?;
    let pack_bytes = pack.bytes;
    entries.sort_unstable_by_key(|entry| entry.id);
    let mut index = Output::new(format, index_sink, limits.max_index_bytes, "index bytes");
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

/// Encodes an OFS_DELTA base distance: big-endian 7-bit groups, each continuation biased by one.
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
mod tests {
    use std::sync::atomic::AtomicBool;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    use super::*;
    use crate::pack::write::{encode_index, write_pack_with_compression};
    use crate::pack::{DeltaOptions, PackCompression};
    use crate::{ObjectFormat, ObjectKind, PackLimits};

    fn versions() -> Vec<Vec<u8>> {
        let base: Vec<u8> = (0..4096u32)
            .flat_map(|n| format!("line {n}\n").into_bytes())
            .collect();
        (0..4u8)
            .map(|n| {
                let mut data = base.clone();
                data.extend_from_slice(&[b'v', b'0' + n, b'\n']);
                data
            })
            .collect()
    }

    fn open(root: &std::path::Path, name: &str, pack: &[u8], index: &[u8]) -> FilePack {
        let path = root.join(format!("{name}.idx"));
        std::fs::write(&path, index).unwrap();
        std::fs::write(path.with_extension("pack"), pack).unwrap();
        FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    fn rewrite(
        packs: &[&FilePack],
        packed: &[Packed],
        fresh: &[PackObject<'_>],
    ) -> Result<(PackWritten, Vec<u8>, Vec<u8>), ReuseError> {
        let (mut pack, mut index) = (Vec::new(), Vec::new());
        let written = write_reusing(
            ObjectFormat::Sha1,
            packs,
            packed,
            fresh,
            (&mut pack, &mut index),
            PackWriteLimits::default(),
            PackCompression::Delta(DeltaOptions::default()),
            (ReadLimits::default(), &AtomicBool::new(false)),
            &mut || Ok(()),
        )?;
        Ok((written, pack, index))
    }

    fn source(root: &std::path::Path, data: &[Vec<u8>]) -> (FilePack, Vec<ObjectId>) {
        let objects: Vec<_> = data
            .iter()
            .map(|data| PackObject {
                id: ObjectId::for_blob(ObjectFormat::Sha1, data),
                kind: ObjectKind::Blob,
                data,
            })
            .collect();
        let (mut pack, mut index) = (Vec::new(), Vec::new());
        let written = write_pack_with_compression(
            ObjectFormat::Sha1,
            &objects,
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
            PackCompression::Delta(DeltaOptions::default()),
        )
        .unwrap();
        assert!(written.deltas.entries > 0);
        (
            open(root, "source", &pack, &index),
            objects.iter().map(|o| o.id).collect(),
        )
    }

    fn packed(pack: &FilePack, ids: &[ObjectId]) -> Vec<Packed> {
        ids.iter()
            .map(|&id| Packed {
                id,
                pack: 0,
                position: pack.find(id).unwrap().unwrap(),
            })
            .collect()
    }

    fn assert_readable(root: &std::path::Path, pack: &[u8], index: &[u8], data: &[Vec<u8>]) {
        let output = open(root, "output", pack, index);
        for data in data {
            let id = ObjectId::for_blob(ObjectFormat::Sha1, data);
            let position = output.find(id).unwrap().unwrap();
            let object = output
                .read(position, ReadLimits::default(), &AtomicBool::new(false))
                .unwrap();
            assert_eq!(object.data(), data.as_slice());
        }
    }

    #[rstest::rstest]
    #[case::ordinary(0)]
    #[case::delta(1)]
    fn raw_reuse_rejects_source_crc_mismatch(#[case] damaged: usize) {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (_, ids) = source(root.path(), &data);
        let pack_bytes = std::fs::read(root.path().join("source.pack")).unwrap();
        let mut index = std::fs::read(root.path().join("source.idx")).unwrap();
        let count = ids.len();
        let identities = &index[1032..1032 + count * 20];
        let slot = identities
            .as_chunks::<20>()
            .0
            .iter()
            .position(|bytes| bytes.as_slice() == ids[damaged].as_bytes())
            .unwrap();
        index[1032 + count * 20 + slot * 4] ^= 1;
        let end = index.len() - 20;
        let checksum = ObjectFormat::Sha1.checksum(&index[..end]);
        index[end..].copy_from_slice(checksum.as_bytes());
        let pack = open(root.path(), "damaged", &pack_bytes, &index);
        let result = rewrite(&[&pack], &packed(&pack, &ids), &[]);
        assert!(
            matches!(result, Err(ReuseError::Read { id, source: ObjectReadError::Corrupt("entry CRC") }) if id == ids[damaged])
        );
    }

    #[test]
    fn copies_every_entry_and_keeps_deltas_when_bases_are_kept() {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (pack, ids) = source(root.path(), &data);
        let (written, bytes, index) = rewrite(&[&pack], &packed(&pack, &ids), &[]).unwrap();
        assert_eq!(written.objects, 4);
        assert_eq!(written.deltas.entries, 3);
        assert_eq!(written.deltas.candidates, 0);
        assert_readable(root.path(), &bytes, &index, &data);
    }

    #[test]
    fn rebuilds_delta_whose_base_is_not_kept() {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (pack, ids) = source(root.path(), &data);
        let mut sorted = ids.clone();
        sorted.sort();
        // The writer orders by ID, so the smallest ID is a whole-object base.
        let kept: Vec<_> = sorted[1..].to_vec();
        let (written, bytes, index) = rewrite(&[&pack], &packed(&pack, &kept), &[]).unwrap();
        assert_eq!(written.objects, 3);
        let kept_data: Vec<_> = data
            .iter()
            .filter(|d| kept.contains(&ObjectId::for_blob(ObjectFormat::Sha1, d)))
            .cloned()
            .collect();
        assert_readable(root.path(), &bytes, &index, &kept_data);
    }

    #[test]
    fn fresh_objects_are_appended_and_may_delta_against_each_other() {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (pack, ids) = source(root.path(), &data[..2]);
        let fresh: Vec<_> = data[2..]
            .iter()
            .map(|data| PackObject {
                id: ObjectId::for_blob(ObjectFormat::Sha1, data),
                kind: ObjectKind::Blob,
                data,
            })
            .collect();
        let (written, bytes, index) = rewrite(&[&pack], &packed(&pack, &ids), &fresh).unwrap();
        assert_eq!(written.objects, 4);
        assert_eq!(written.deltas.entries, 2);
        assert_readable(root.path(), &bytes, &index, &data);
    }

    #[test]
    fn rejects_fresh_object_that_repeats_a_packed_one() {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (pack, ids) = source(root.path(), &data);
        let fresh = [PackObject {
            id: ids[0],
            kind: ObjectKind::Blob,
            data: &data[0],
        }];
        assert!(matches!(
            rewrite(&[&pack], &packed(&pack, &ids), &fresh),
            Err(ReuseError::Write(PackWriteError::ConflictingDuplicate(_)))
        ));
    }

    #[test]
    fn reports_delta_with_base_outside_its_pack() {
        let root = tempfile::tempdir().unwrap();
        let missing = ObjectId::for_blob(ObjectFormat::Sha1, b"absent base");
        let mut entry = entry_header_code(7, 2);
        entry.extend_from_slice(missing.as_bytes());
        let mut encoder = ZlibEncoder::new(entry, Compression::new(6));
        encoder.write_all(&[11, 0]).unwrap();
        let entry = encoder.finish().unwrap();
        let mut pack = Vec::new();
        let mut output = Output::new(ObjectFormat::Sha1, &mut pack, u64::MAX, "pack bytes");
        output.put(b"PACK\0\0\0\x02\0\0\0\x01").unwrap();
        output.crc = crc32fast::Hasher::new();
        output.put(&entry).unwrap();
        let crc = output.crc.clone().finalize();
        let checksum = output.finish().unwrap();
        let id = ObjectId::for_blob(ObjectFormat::Sha1, b"thin target");
        let index = encode_index(
            &[Entry {
                id,
                offset: 12,
                crc,
            }],
            checksum,
        )
        .unwrap();
        let source = open(root.path(), "thin", &pack, &index);
        let result = rewrite(
            &[&source],
            &[Packed {
                id,
                pack: 0,
                position: 0,
            }],
            &[],
        );
        assert!(matches!(
            result,
            Err(ReuseError::Read {
                source: ObjectReadError::MissingBase(base),
                ..
            }) if base == missing
        ));
    }

    #[test]
    fn offset_distance_uses_biased_continuation_groups() {
        assert_eq!(offset_distance(1), [1]);
        assert_eq!(offset_distance(127), [127]);
        assert_eq!(offset_distance(128), [0x80, 0]);
        assert_eq!(offset_distance(16511), [0xff, 0x7f]);
        assert_eq!(offset_distance(16512), [0x80, 0x80, 0]);
    }
    #[rstest::rstest]
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
    #[test]
    fn truncation_of_pinned_source_is_rejected_during_copy() {
        let root = tempfile::tempdir().unwrap();
        let data = versions();
        let (source, ids) = source(root.path(), &data);
        let selected = packed(&source, &ids);
        std::fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("source.pack"))
            .unwrap()
            .set_len(16)
            .unwrap();
        assert!(rewrite(&[&source], &selected, &[]).is_err());
    }
}
