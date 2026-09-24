//! Imports received pack bytes for fetch validation, retaining decoded objects until
//! connectivity is checked. Storage-format primitives remain in the pack module.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

use super::{FetchError, FetchLimits, check_cancelled};
use crate::pack::delta::{apply, byte, charge, size};
use crate::pack::index::{verify_hash, word};
use crate::pack::reader::inflate_prefix;
use crate::pack::write::{Entry, encode_index};
use crate::{Object, ObjectId, ObjectKind, ObjectReadError as Error};

/// Fully decoded objects are retained until connectivity has been checked. All inflation and
/// reconstruction is charged to one budget, so both live payload memory and delta work are bounded.
pub(crate) struct Imported {
    pub objects: HashMap<ObjectId, Object>,
    pub index: Vec<u8>,
    pub checksum: ObjectId,
}

impl Imported {
    pub fn read(data: &[u8], limits: FetchLimits, cancel: &AtomicBool) -> Result<Self, FetchError> {
        check_cancelled(cancel)?;
        if data.len() > limits.max_pack_bytes {
            return Err(FetchError::Limit("pack bytes"));
        }
        if data.len() < 32 || &data[..4] != b"PACK" {
            return Err(Error::Corrupt("pack header").into());
        }
        let version = word(data, 4)?;
        if version != 2 {
            return Err(Error::PackVersion(version).into());
        }
        let count = word(data, 8)? as usize;
        if count > limits.max_objects {
            return Err(FetchError::Limit("pack objects"));
        }
        verify_hash(data, "received pack checksum")?;
        let end = data.len() - 20;
        let checksum = ObjectId::from_bytes(data[end..].try_into().unwrap());
        let mut input = &data[12..end];
        let mut entries = Vec::new();
        let mut remaining = limits.max_decode_bytes;
        for _ in 0..count {
            check_cancelled(cancel)?;
            let offset = end - input.len();
            let header = byte(&mut input)?;
            let kind = (header >> 4) & 7;
            let length = size(&mut input, (header & 15) as usize, 4, header & 128 != 0)?;
            let base = base(&mut input, kind, offset)?;
            let maximum = if matches!(base, Base::Kind(_)) {
                limits.max_object_bytes
            } else {
                limits.max_delta_bytes
            };
            if length > maximum {
                return Err(FetchError::Limit("inflated entry bytes"));
            }
            charge(&mut remaining, length)?;
            let (payload, consumed) = inflate_prefix(input, length)?;
            input = &input[consumed..];
            let entry_end = end - input.len();
            entries.push(ReceivedEntry {
                base,
                payload,
                offset,
                crc: crc32fast::hash(&data[offset..entry_end]),
                resolved: None,
            });
        }
        if !input.is_empty() {
            return Err(Error::Corrupt("trailing pack entries").into());
        }
        let objects = resolve(&mut entries, limits, &mut remaining, cancel)?;
        let mut index_entries: Vec<_> = entries
            .iter()
            .map(|entry| Entry {
                id: entry.resolved.unwrap().0,
                offset: entry.offset as u64,
                crc: entry.crc,
            })
            .collect();
        index_entries.sort_unstable_by_key(|entry| entry.id);
        let index = encode_index(&index_entries, checksum)?;
        Ok(Self {
            objects,
            index,
            checksum,
        })
    }
}

struct ReceivedEntry {
    base: Base,
    payload: Vec<u8>,
    offset: usize,
    crc: u32,
    // Identity and number of delta edges, once reconstructed.
    resolved: Option<(ObjectId, usize)>,
}

enum Base {
    Kind(ObjectKind),
    Offset(usize),
    Id(ObjectId),
}

fn base(input: &mut &[u8], kind: u8, offset: usize) -> Result<Base, Error> {
    Ok(match kind {
        1 => Base::Kind(ObjectKind::Commit),
        2 => Base::Kind(ObjectKind::Tree),
        3 => Base::Kind(ObjectKind::Blob),
        4 => Base::Kind(ObjectKind::Tag),
        6 => {
            let mut next = byte(input)?;
            let mut distance = (next & 127) as usize;
            while next & 128 != 0 {
                next = byte(input)?;
                distance = distance
                    .checked_add(1)
                    .and_then(|n| n.checked_mul(128))
                    .and_then(|n| n.checked_add((next & 127) as usize))
                    .ok_or(Error::Corrupt("delta offset overflow"))?;
            }
            if distance == 0 {
                return Err(Error::Corrupt("zero delta distance"));
            }
            Base::Offset(
                offset
                    .checked_sub(distance)
                    .ok_or(Error::Corrupt("delta before pack"))?,
            )
        }
        7 => {
            let raw = input
                .get(..20)
                .ok_or(Error::Corrupt("truncated base identity"))?;
            let id = ObjectId::from_bytes(raw.try_into().unwrap());
            *input = &input[20..];
            Base::Id(id)
        }
        other => return Err(Error::ObjectType(other)),
    })
}

fn resolve(
    entries: &mut [ReceivedEntry],
    limits: FetchLimits,
    remaining: &mut usize,
    cancel: &AtomicBool,
) -> Result<HashMap<ObjectId, Object>, FetchError> {
    let offsets: HashMap<_, _> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.offset, i))
        .collect();
    let mut identities = HashMap::new();
    let mut objects: HashMap<ObjectId, Object> = HashMap::new();
    let mut work = limits.max_resolution_steps;
    while objects.len() < entries.len() {
        let before = objects.len();
        for i in 0..entries.len() {
            check_cancelled(cancel)?;
            work = work
                .checked_sub(1)
                .ok_or(FetchError::Limit("delta resolution steps"))?;
            if entries[i].resolved.is_some() {
                continue;
            }
            let base = match entries[i].base {
                Base::Kind(_) => None,
                Base::Offset(offset) => {
                    let position = offsets
                        .get(&offset)
                        .ok_or(Error::Corrupt("delta base offset"))?;
                    let Some(base) = entries[*position].resolved else {
                        continue;
                    };
                    Some(base)
                }
                Base::Id(id) => {
                    let Some(&depth) = identities.get(&id) else {
                        continue;
                    };
                    Some((id, depth))
                }
            };
            let (object, depth) = if let Some((id, depth)) = base {
                if depth >= limits.max_delta_depth {
                    return Err(FetchError::Limit("delta depth"));
                }
                let base = &objects[&id];
                let data = apply(
                    &base.data,
                    &entries[i].payload,
                    limits.max_object_bytes,
                    remaining,
                )?;
                (
                    Object {
                        kind: base.kind,
                        data,
                    },
                    depth + 1,
                )
            } else {
                let Base::Kind(kind) = entries[i].base else {
                    unreachable!()
                };
                (
                    Object {
                        kind,
                        data: std::mem::take(&mut entries[i].payload),
                    },
                    0,
                )
            };
            let id = object.id();
            if objects.insert(id, object).is_some() {
                return Err(Error::Corrupt("duplicate received identity").into());
            }
            identities.insert(id, depth);
            entries[i].resolved = Some((id, depth));
            entries[i].payload.clear();
        }
        if before == objects.len() {
            let missing = entries.iter().find_map(|entry| match entry.base {
                Base::Id(id) if entry.resolved.is_none() && !objects.contains_key(&id) => Some(id),
                _ => None,
            });
            return Err(missing.map_or(Error::DeltaCycle, Error::MissingBase).into());
        }
    }
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use rstest::rstest;
    use sha1::{Digest, Sha1};

    use super::*;

    fn entry(kind: u8, base: &[u8], data: &[u8]) -> Vec<u8> {
        assert!(data.len() < 16);
        let mut result = vec![(kind << 4) | data.len() as u8];
        result.extend(base);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        result.extend(encoder.finish().unwrap());
        result
    }
    fn pack(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = b"PACK\0\0\0\x02".to_vec();
        bytes.extend((entries.len() as u32).to_be_bytes());
        bytes.extend(entries.concat());
        bytes.extend(Sha1::digest(&bytes));
        bytes
    }
    fn read(bytes: &[u8], limits: FetchLimits) -> Result<Imported, FetchError> {
        Imported::read(bytes, limits, &AtomicBool::new(false))
    }
    fn forward_ref_pack() -> Vec<u8> {
        let id = ObjectId::for_blob(b"one");
        pack(&[
            entry(7, id.as_bytes(), b"\x03\x03\x03two"),
            entry(3, b"", b"one"),
        ])
    }

    #[test]
    fn resolves_forward_ref_base_and_generates_readable_index() {
        let bytes = forward_ref_pack();
        let imported = read(&bytes, FetchLimits::default()).unwrap();
        let reader = crate::pack::Pack::open(&imported.index, bytes).unwrap();
        let id = ObjectId::for_blob(b"two");
        assert_eq!(
            reader
                .read(reader.find(id).unwrap(), crate::ReadLimits::default())
                .unwrap()
                .data(),
            b"two"
        );
    }

    #[test]
    fn resolves_ofs_base() {
        let ordinary = entry(3, b"", b"one");
        let delta = entry(6, &[ordinary.len() as u8], b"\x03\x03\x03two");
        let imported = read(&pack(&[ordinary, delta]), FetchLimits::default()).unwrap();
        assert_eq!(imported.objects[&ObjectId::for_blob(b"two")].data(), b"two");
    }

    #[rstest]
    #[case::depth(FetchLimits { max_delta_depth: 0, ..FetchLimits::default() })]
    #[case::program(FetchLimits { max_delta_bytes: 5, ..FetchLimits::default() })]
    #[case::reconstructed_bytes(FetchLimits { max_decode_bytes: 11, ..FetchLimits::default() })]
    #[case::resolution_work(FetchLimits { max_resolution_steps: 2, ..FetchLimits::default() })]
    fn delta_limits(#[case] limits: FetchLimits) {
        assert!(matches!(
            read(&forward_ref_pack(), limits),
            Err(FetchError::Limit(_) | FetchError::Pack(Error::Limit(_)))
        ));
    }

    #[test]
    fn exact_delta_limits_succeed() {
        let limits = FetchLimits {
            max_delta_depth: 1,
            max_delta_bytes: 6,
            max_decode_bytes: 12,
            max_object_bytes: 3,
            max_resolution_steps: 4,
            ..FetchLimits::default()
        };
        assert_eq!(read(&forward_ref_pack(), limits).unwrap().objects.len(), 2);
    }

    #[test]
    fn rejects_external_base() {
        let id = ObjectId::for_blob(b"external");
        let bytes = pack(&[entry(7, id.as_bytes(), b"\x08\x03\x03two")]);
        assert!(
            matches!(read(&bytes, FetchLimits::default()), Err(FetchError::Pack(Error::MissingBase(actual))) if actual == id)
        );
    }

    #[rstest]
    #[case::before_pack(127)]
    #[case::self_reference(0)]
    #[case::not_entry_boundary(1)]
    fn rejects_bad_offset_base(#[case] distance: u8) {
        let bytes = pack(&[entry(6, &[distance], b"\x03\x03\x03two")]);
        assert!(matches!(
            read(&bytes, FetchLimits::default()),
            Err(FetchError::Pack(Error::Corrupt(_)))
        ));
    }

    #[rstest]
    #[case::reserved(0)]
    #[case::unused(5)]
    fn rejects_reserved_type(#[case] kind: u8) {
        assert!(matches!(
            read(&pack(&[entry(kind, b"", b"one")]), FetchLimits::default()),
            Err(FetchError::Pack(Error::ObjectType(_)))
        ));
    }

    #[test]
    fn rejects_duplicate_identities() {
        let bytes = pack(&[entry(3, b"", b"one"), entry(3, b"", b"one")]);
        assert!(matches!(
            read(&bytes, FetchLimits::default()),
            Err(FetchError::Pack(Error::Corrupt(
                "duplicate received identity"
            )))
        ));
    }

    #[rstest]
    #[case::reserved_instruction(b"\x03\x03\0")]
    #[case::wrong_base_length(b"\x04\x03\x03two")]
    #[case::short_result(b"\x03\x04\x03two")]
    #[case::copy_outside(b"\x03\x03\x91\x04\x03")]
    fn rejects_invalid_delta_program(#[case] program: &[u8]) {
        let bytes = pack(&[
            entry(3, b"", b"one"),
            entry(7, ObjectId::for_blob(b"one").as_bytes(), program),
        ]);
        assert!(matches!(
            read(&bytes, FetchLimits::default()),
            Err(FetchError::Pack(Error::Corrupt(_)))
        ));
    }

    #[rstest]
    #[case::bad_zlib(vec![0x33, 1, 2, 3])]
    #[case::truncated_zlib(vec![0x33, 0x78, 0x9c])]
    #[case::wrong_length({let mut e = entry(3, b"", b"one"); e[0] = 0x34; e})]
    #[case::trailing({let mut e = entry(3, b"", b"one"); e.push(0); e})]
    fn rejects_entry_framing_even_with_valid_checksum(#[case] raw: Vec<u8>) {
        assert!(matches!(
            read(&pack(&[raw]), FetchLimits::default()),
            Err(FetchError::Pack(Error::Corrupt(_)))
        ));
    }

    #[test]
    fn cancellation_precedes_pack_validation() {
        assert!(matches!(
            Imported::read(b"", FetchLimits::default(), &AtomicBool::new(true)),
            Err(FetchError::Cancelled)
        ));
    }
}
