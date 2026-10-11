//! Pack output that copies stored entries verbatim and encodes only fresh objects.
use std::collections::HashMap;
use std::io::Write;

use super::FilePack;
use super::compression::{DeltaStats, PackCompression};
use super::reader::StoredBase;
use super::write::{
    Entry, Output, PackObject, PackWriteError, PackWriteLimits, PackWritten, entry_header,
    entry_header_code, put_object, validate, write_index,
};
use crate::{ObjectId, ObjectReadError};

/// A stored pack entry to copy, keeping its zlib stream and, for deltas, its program.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StoredCopy<'a> {
    /// Pinned source pack.
    pub pack: &'a FilePack,
    /// Index position of the entry in `pack`.
    pub position: usize,
    /// Identity of the delta base, which must already be in the output; `None` for whole objects.
    pub base: Option<ObjectId>,
}

/// Why a reusing write stopped.
#[derive(Debug)]
pub(crate) enum ReuseError {
    /// A stored entry could not be read; carries the entry's identity.
    Read(ObjectId, ObjectReadError),
    /// Validation, a bound, or output failed.
    Write(PackWriteError),
}

impl From<PackWriteError> for ReuseError {
    fn from(error: PackWriteError) -> Self {
        Self::Write(error)
    }
}

/// Writes fresh objects, then stored entries in the given order, as a pack v2 and index v2.
///
/// Fresh objects are validated as by [`super::write_pack`] and ordered by kind and decreasing
/// size, so delta search under `compression` compares similar objects. They may only use each
/// other as bases. Stored entries keep their compressed bytes and delta form: an OFS_DELTA gets
/// its base's new offset and a REF_DELTA keeps naming `base`. Either way `base` must name a fresh
/// object or an earlier stored entry, so the output never depends on a later or external base.
/// Stored identities must not repeat or overlap the fresh objects. `limits` bounds fresh
/// payloads as for [`super::write_pack`], every entry's count, and both outputs. Stored entries are
/// streamed, not held in memory. Returned delta statistics describe fresh entries only.
pub(crate) fn write_reusing(
    format: crate::ObjectFormat,
    fresh: &[PackObject<'_>],
    stored: &[StoredCopy<'_>],
    outputs: (&mut impl Write, &mut impl Write),
    limits: PackWriteLimits,
    compression: PackCompression,
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
) -> Result<PackWritten, ReuseError> {
    let (pack, index) = outputs;
    let total = fresh
        .len()
        .checked_add(stored.len())
        .filter(|&total| total as u64 <= u64::from(limits.max_objects))
        .ok_or(PackWriteError::Limit("input occurrences"))?;
    let mut fresh = validate(format, fresh, limits, check)?;
    fresh.sort_by(|a, b| {
        (a.kind as u8, std::cmp::Reverse(a.data.len()), a.id).cmp(&(
            b.kind as u8,
            std::cmp::Reverse(b.data.len()),
            b.id,
        ))
    });
    // Validation collapses exact duplicate fresh objects.
    let count = u32::try_from(fresh.len() + stored.len())
        .map_err(|_| PackWriteError::Limit("input occurrences"))?;
    let mut pack = Output::new(format, pack, limits.max_pack_bytes, "pack bytes");
    pack.put(b"PACK")?;
    pack.put(&2u32.to_be_bytes())?;
    pack.put(&count.to_be_bytes())?;
    let mut entries = Vec::with_capacity(total);
    let mut offsets = HashMap::with_capacity(total);
    let mut depths = Vec::with_capacity(fresh.len());
    let mut deltas = DeltaStats::default();
    for position in 0..fresh.len() {
        check()?;
        let entry = put_object(
            &mut pack,
            &fresh,
            position,
            &mut depths,
            compression,
            &mut deltas,
            check,
        )?;
        offsets.insert(entry.id, entry.offset);
        entries.push(entry);
    }
    let mut buffer = vec![0; 64 * 1024];
    for copy in stored {
        check()?;
        let entry = put_stored(&mut pack, copy, &offsets, &mut buffer, check)?;
        if offsets.insert(entry.id, entry.offset).is_some() {
            return Err(PackWriteError::ConflictingDuplicate(entry.id).into());
        }
        entries.push(entry);
    }
    let checksum = pack.finish()?;
    let pack_bytes = pack.bytes;
    entries.sort_unstable_by_key(|entry| entry.id);
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

fn put_stored(
    pack: &mut Output<'_, impl Write>,
    copy: &StoredCopy<'_>,
    offsets: &HashMap<ObjectId, u64>,
    buffer: &mut [u8],
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
) -> Result<Entry, ReuseError> {
    let source = copy.pack;
    let id = source.id(copy.position);
    let read = |error| ReuseError::Read(id, error);
    let header = source.header(copy.position).map_err(read)?;
    let offset = pack.bytes;
    let length = header.length as u64;
    let bytes = match (header.kind(), copy.base) {
        (Some(kind), None) => entry_header(kind, length),
        (None, Some(base)) => {
            let base_offset = *offsets
                .get(&base)
                .ok_or(ReuseError::Read(id, ObjectReadError::MissingBase(base)))?;
            if matches!(header.base, Some(StoredBase::Id(_))) {
                let mut bytes = entry_header_code(7, length);
                bytes.extend(base.as_bytes());
                bytes
            } else {
                let mut bytes = entry_header_code(6, length);
                bytes.extend(ofs_distance(offset - base_offset));
                bytes
            }
        }
        _ => {
            return Err(ReuseError::Read(
                id,
                ObjectReadError::Corrupt("stored entry and planned base disagree"),
            ));
        }
    };
    pack.crc = crc32fast::Hasher::new();
    pack.put(&bytes)?;
    let (_, end) = source.entry_range(copy.position);
    let mut position = header.data_start;
    while position < end {
        check()?;
        let count = buffer.len().min(end - position);
        source
            .read_stored(position, &mut buffer[..count])
            .map_err(read)?;
        pack.put(&buffer[..count])?;
        position += count;
    }
    Ok(Entry {
        id,
        offset,
        crc: pack.crc.clone().finalize(),
    })
}

/// Encodes an OFS_DELTA distance: big-endian 7-bit groups, each continuation adding one.
fn ofs_distance(mut distance: u64) -> Vec<u8> {
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

    use rstest::rstest;

    use super::*;
    use crate::pack::{DeltaOptions, write_pack_with_compression};
    use crate::{ObjectFormat, ObjectKind, PackLimits};

    // The reader adds one per continuation byte, so each group boundary shifts by 2^(7k).
    #[rstest]
    #[case::one_byte(127, &[0x7f])]
    #[case::two_bytes(128, &[0x80, 0x00])]
    #[case::two_bytes_max(16_511, &[0xff, 0x7f])]
    #[case::three_bytes(16_512, &[0x80, 0x80, 0x00])]
    fn offset_distance_vectors(#[case] distance: u64, #[case] expected: &[u8]) {
        assert_eq!(ofs_distance(distance), expected);
    }

    /// A pack holding a whole blob and a REF_DELTA against it.
    fn source(root: &std::path::Path) -> (FilePack, ObjectId, ObjectId) {
        let base: Vec<u8> = (0..=255).cycle().take(4096).collect();
        let mut edited = base.clone();
        edited[1000] ^= 255;
        let objects: Vec<_> = [&base, &edited]
            .into_iter()
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
        assert_eq!(written.deltas.entries, 1);
        let path = root.join("pack-source.idx");
        std::fs::write(&path, index).unwrap();
        std::fs::write(path.with_extension("pack"), pack).unwrap();
        let pack = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let (first, second) = (
            objects[0].id.min(objects[1].id),
            objects[0].id.max(objects[1].id),
        );
        (pack, first, second)
    }

    fn write(stored: &[StoredCopy<'_>]) -> Result<PackWritten, ReuseError> {
        write_reusing(
            ObjectFormat::Sha1,
            &[],
            stored,
            (&mut Vec::new(), &mut Vec::new()),
            PackWriteLimits::default(),
            PackCompression::Ordinary,
            &mut || Ok(()),
        )
    }

    #[test]
    fn delta_before_its_base_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let (pack, base, delta) = source(root.path());
        let copy = StoredCopy {
            pack: &pack,
            position: pack.find(delta).unwrap().unwrap(),
            base: Some(base),
        };
        let result = write(&[copy]);
        assert!(matches!(
            result,
            Err(ReuseError::Read(id, ObjectReadError::MissingBase(missing)))
                if id == delta && missing == base
        ));
    }

    #[test]
    fn whole_entry_given_a_base_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let (pack, base, delta) = source(root.path());
        let copy = StoredCopy {
            pack: &pack,
            position: pack.find(base).unwrap().unwrap(),
            base: Some(delta),
        };
        let result = write(&[copy]);
        assert!(matches!(
            result,
            Err(ReuseError::Read(_, ObjectReadError::Corrupt(_)))
        ));
    }

    #[test]
    fn delta_entry_without_a_base_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let (pack, _, delta) = source(root.path());
        let copy = StoredCopy {
            pack: &pack,
            position: pack.find(delta).unwrap().unwrap(),
            base: None,
        };
        let result = write(&[copy]);
        assert!(matches!(
            result,
            Err(ReuseError::Read(_, ObjectReadError::Corrupt(_)))
        ));
    }

    #[test]
    fn repeated_stored_identity_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let (pack, base, _) = source(root.path());
        let copy = StoredCopy {
            pack: &pack,
            position: pack.find(base).unwrap().unwrap(),
            base: None,
        };
        let result = write(&[copy, copy]);
        assert!(matches!(
            result,
            Err(ReuseError::Write(PackWriteError::ConflictingDuplicate(id))) if id == base
        ));
    }

    // Copying both entries reproduces readable objects, keeping the REF_DELTA form.
    #[test]
    fn copied_pack_reads_back() {
        let root = tempfile::tempdir().unwrap();
        let (pack, base, delta) = source(root.path());
        let stored = [
            StoredCopy {
                pack: &pack,
                position: pack.find(base).unwrap().unwrap(),
                base: None,
            },
            StoredCopy {
                pack: &pack,
                position: pack.find(delta).unwrap().unwrap(),
                base: Some(base),
            },
        ];
        let (mut bytes, mut index) = (Vec::new(), Vec::new());
        let written = write_reusing(
            ObjectFormat::Sha1,
            &[],
            &stored,
            (&mut bytes, &mut index),
            PackWriteLimits::default(),
            PackCompression::Ordinary,
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(written.objects, 2);
        let path = root.path().join("pack-copy.idx");
        std::fs::write(&path, index).unwrap();
        std::fs::write(path.with_extension("pack"), bytes).unwrap();
        let copy = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let position = copy.find(delta).unwrap().unwrap();
        assert_eq!(
            copy.header(position).unwrap().base,
            Some(StoredBase::Id(base))
        );
        let object = copy
            .read(
                position,
                crate::ReadLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(object.id(), delta);
    }
}
