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
    pub fn read(
        format: crate::ObjectFormat,
        data: &[u8],
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, FetchError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "fetch.import",
            object_format = %format,
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
            pack_bytes = data.len(),
            decoded_bytes = tracing::field::Empty,
            objects = tracing::field::Empty,
        );

        let operation = || {
            check_cancelled(cancel)?;
            if data.len() > limits.max_pack_bytes {
                return Err(FetchError::Limit("pack bytes"));
            }
            if data.len() < 12 + format.digest_len() || &data[..4] != b"PACK" {
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
            verify_hash(format, data, "received pack checksum")?;
            let end = data.len() - format.digest_len();
            let checksum = ObjectId::from_bytes(format, &data[end..]).unwrap();
            let mut input = &data[12..end];
            let mut entries = Vec::new();
            let mut remaining = limits.max_decode_bytes;
            for _ in 0..count {
                check_cancelled(cancel)?;
                let offset = end - input.len();
                let header = byte(&mut input)?;
                let kind = (header >> 4) & 7;
                let length = size(&mut input, (header & 15) as usize, 4, header & 128 != 0)?;
                let base = base(format, &mut input, kind, offset)?;
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
            let objects = resolve(format, &mut entries, limits, &mut remaining, cancel)?;
            #[cfg(feature = "tracing")]
            span.record("decoded_bytes", limits.max_decode_bytes - remaining)
                .record("objects", objects.len());
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
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::fetch);

        result
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

fn base(
    format: crate::ObjectFormat,
    input: &mut &[u8],
    kind: u8,
    offset: usize,
) -> Result<Base, Error> {
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
                .get(..format.digest_len())
                .ok_or(Error::Corrupt("truncated base identity"))?;
            let id = ObjectId::from_bytes(format, raw).unwrap();
            *input = &input[format.digest_len()..];
            Base::Id(id)
        }
        other => return Err(Error::ObjectType(other)),
    })
}

fn resolve(
    format: crate::ObjectFormat,
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
                        format,
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
                        format,
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

    use super::*;
    use crate::ObjectFormat;

    fn entry(kind: u8, base: &[u8], data: &[u8]) -> Vec<u8> {
        assert!(data.len() < 16);
        let mut result = vec![(kind << 4) | data.len() as u8];
        result.extend(base);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        result.extend(encoder.finish().unwrap());
        result
    }
    fn pack(format: ObjectFormat, entries: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = b"PACK\0\0\0\x02".to_vec();
        bytes.extend((entries.len() as u32).to_be_bytes());
        bytes.extend(entries.concat());
        bytes.extend_from_slice(format.checksum(&bytes).as_bytes());
        bytes
    }
    fn read(
        format: ObjectFormat,
        bytes: &[u8],
        limits: FetchLimits,
    ) -> Result<Imported, FetchError> {
        Imported::read(format, bytes, limits, &AtomicBool::new(false))
    }
    fn forward_ref_pack(format: ObjectFormat) -> Vec<u8> {
        let id = ObjectId::for_blob(format, b"one");
        pack(
            format,
            &[
                entry(7, id.as_bytes(), b"\x03\x03\x03two"),
                entry(3, b"", b"one"),
            ],
        )
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn resolves_forward_ref_base_and_generates_readable_index(#[case] format: ObjectFormat) {
        let bytes = forward_ref_pack(format);
        let imported = read(format, &bytes, FetchLimits::default()).unwrap();
        let reader = crate::pack::Pack::open(format, &imported.index, bytes).unwrap();
        let id = ObjectId::for_blob(format, b"two");
        assert_eq!(
            reader
                .read(reader.find(id).unwrap(), crate::ReadLimits::default())
                .unwrap()
                .data(),
            b"two"
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn resolves_ofs_base(#[case] format: ObjectFormat) {
        let ordinary = entry(3, b"", b"one");
        let delta = entry(6, &[ordinary.len() as u8], b"\x03\x03\x03two");
        let imported = read(
            format,
            &pack(format, &[ordinary, delta]),
            FetchLimits::default(),
        )
        .unwrap();
        assert_eq!(
            imported.objects[&ObjectId::for_blob(format, b"two")].data(),
            b"two"
        );
    }

    #[rstest]
    #[case::depth_sha1(ObjectFormat::Sha1, FetchLimits { max_delta_depth: 0, ..FetchLimits::default() })]
    #[case::depth_sha256(ObjectFormat::Sha256, FetchLimits { max_delta_depth: 0, ..FetchLimits::default() })]
    #[case::program_sha1(ObjectFormat::Sha1, FetchLimits { max_delta_bytes: 5, ..FetchLimits::default() })]
    #[case::program_sha256(ObjectFormat::Sha256, FetchLimits { max_delta_bytes: 5, ..FetchLimits::default() })]
    #[case::reconstructed_bytes_sha1(ObjectFormat::Sha1, FetchLimits { max_decode_bytes: 11, ..FetchLimits::default() })]
    #[case::reconstructed_bytes_sha256(ObjectFormat::Sha256, FetchLimits { max_decode_bytes: 11, ..FetchLimits::default() })]
    #[case::resolution_work_sha1(ObjectFormat::Sha1, FetchLimits { max_resolution_steps: 2, ..FetchLimits::default() })]
    #[case::resolution_work_sha256(ObjectFormat::Sha256, FetchLimits { max_resolution_steps: 2, ..FetchLimits::default() })]
    fn delta_limits(#[case] format: ObjectFormat, #[case] limits: FetchLimits) {
        assert!(matches!(
            read(format, &forward_ref_pack(format), limits),
            Err(FetchError::Limit(_) | FetchError::Pack(Error::Limit(_)))
        ));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn exact_delta_limits_succeed(#[case] format: ObjectFormat) {
        let limits = FetchLimits {
            max_delta_depth: 1,
            max_delta_bytes: 6,
            max_decode_bytes: 12,
            max_object_bytes: 3,
            max_resolution_steps: 4,
            ..FetchLimits::default()
        };
        assert_eq!(
            read(format, &forward_ref_pack(format), limits)
                .unwrap()
                .objects
                .len(),
            2
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn rejects_external_base(#[case] format: ObjectFormat) {
        let id = ObjectId::for_blob(format, b"external");
        let bytes = pack(format, &[entry(7, id.as_bytes(), b"\x08\x03\x03two")]);
        assert!(
            matches!(read(format, &bytes, FetchLimits::default()), Err(FetchError::Pack(Error::MissingBase(actual))) if actual == id)
        );
    }

    #[rstest]
    #[case::before_pack_sha1(ObjectFormat::Sha1, 127)]
    #[case::before_pack_sha256(ObjectFormat::Sha256, 127)]
    #[case::self_reference_sha1(ObjectFormat::Sha1, 0)]
    #[case::self_reference_sha256(ObjectFormat::Sha256, 0)]
    #[case::not_entry_boundary_sha1(ObjectFormat::Sha1, 1)]
    #[case::not_entry_boundary_sha256(ObjectFormat::Sha256, 1)]
    fn rejects_bad_offset_base(#[case] format: ObjectFormat, #[case] distance: u8) {
        let bytes = pack(format, &[entry(6, &[distance], b"\x03\x03\x03two")]);
        assert!(matches!(
            read(format, &bytes, FetchLimits::default()),
            Err(FetchError::Pack(Error::Corrupt(_)))
        ));
    }

    #[rstest]
    #[case::reserved_sha1(ObjectFormat::Sha1, 0)]
    #[case::reserved_sha256(ObjectFormat::Sha256, 0)]
    #[case::unused_sha1(ObjectFormat::Sha1, 5)]
    #[case::unused_sha256(ObjectFormat::Sha256, 5)]
    fn rejects_reserved_type(#[case] format: ObjectFormat, #[case] kind: u8) {
        assert!(matches!(
            read(
                format,
                &pack(format, &[entry(kind, b"", b"one")]),
                FetchLimits::default()
            ),
            Err(FetchError::Pack(Error::ObjectType(_)))
        ));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn rejects_duplicate_identities(#[case] format: ObjectFormat) {
        let bytes = pack(format, &[entry(3, b"", b"one"), entry(3, b"", b"one")]);
        assert!(matches!(
            read(format, &bytes, FetchLimits::default()),
            Err(FetchError::Pack(Error::Corrupt(
                "duplicate received identity"
            )))
        ));
    }

    #[rstest]
    #[case::reserved_instruction_sha1(ObjectFormat::Sha1, b"\x03\x03\0")]
    #[case::reserved_instruction_sha256(ObjectFormat::Sha256, b"\x03\x03\0")]
    #[case::wrong_base_length_sha1(ObjectFormat::Sha1, b"\x04\x03\x03two")]
    #[case::wrong_base_length_sha256(ObjectFormat::Sha256, b"\x04\x03\x03two")]
    #[case::short_result_sha1(ObjectFormat::Sha1, b"\x03\x04\x03two")]
    #[case::short_result_sha256(ObjectFormat::Sha256, b"\x03\x04\x03two")]
    #[case::copy_outside_sha1(ObjectFormat::Sha1, b"\x03\x03\x91\x04\x03")]
    #[case::copy_outside_sha256(ObjectFormat::Sha256, b"\x03\x03\x91\x04\x03")]
    fn rejects_invalid_delta_program(#[case] format: ObjectFormat, #[case] program: &[u8]) {
        let bytes = pack(
            format,
            &[
                entry(3, b"", b"one"),
                entry(7, ObjectId::for_blob(format, b"one").as_bytes(), program),
            ],
        );
        assert!(matches!(
            read(format, &bytes, FetchLimits::default()),
            Err(FetchError::Pack(Error::Corrupt(_)))
        ));
    }

    #[rstest]
    #[case::bad_zlib_sha1(ObjectFormat::Sha1, vec![0x33, 1, 2, 3])]
    #[case::bad_zlib_sha256(ObjectFormat::Sha256, vec![0x33, 1, 2, 3])]
    #[case::truncated_zlib_sha1(ObjectFormat::Sha1, vec![0x33, 0x78, 0x9c])]
    #[case::truncated_zlib_sha256(ObjectFormat::Sha256, vec![0x33, 0x78, 0x9c])]
    #[case::wrong_length_sha1(ObjectFormat::Sha1, {let mut e = entry(3, b"", b"one"); e[0] = 0x34; e})]
    #[case::wrong_length_sha256(ObjectFormat::Sha256, {let mut e = entry(3, b"", b"one"); e[0] = 0x34; e})]
    #[case::trailing_sha1(ObjectFormat::Sha1, {let mut e = entry(3, b"", b"one"); e.push(0); e})]
    #[case::trailing_sha256(ObjectFormat::Sha256, {let mut e = entry(3, b"", b"one"); e.push(0); e})]
    fn rejects_entry_framing_even_with_valid_checksum(
        #[case] format: ObjectFormat,
        #[case] raw: Vec<u8>,
    ) {
        assert!(matches!(
            read(format, &pack(format, &[raw]), FetchLimits::default()),
            Err(FetchError::Pack(Error::Corrupt(_)))
        ));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn cancellation_precedes_pack_validation(#[case] format: ObjectFormat) {
        assert!(matches!(
            Imported::read(format, b"", FetchLimits::default(), &AtomicBool::new(true)),
            Err(FetchError::Cancelled)
        ));
    }
}

#[cfg(test)]
mod format_tests {
    use rstest::rstest;

    use super::*;
    use crate::ObjectFormat;

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1, ObjectFormat::Sha256)]
    #[case::sha256(ObjectFormat::Sha256, ObjectFormat::Sha1)]
    fn validates_empty_and_rejects_wrong_format_and_truncation(
        #[case] format: ObjectFormat,
        #[case] other: ObjectFormat,
    ) {
        let mut pack = b"PACK\0\0\0\x02\0\0\0\0".to_vec();
        pack.extend_from_slice(format.checksum(&pack).as_bytes());
        let cancel = AtomicBool::new(false);
        let limits = FetchLimits::default();
        let imported = Imported::read(format, &pack, limits, &cancel).unwrap();
        assert!(imported.objects.is_empty());
        assert_eq!(imported.checksum.format(), format);
        assert!(crate::pack::Pack::open(format, &imported.index, pack.clone()).is_ok());
        assert!(Imported::read(other, &pack, limits, &cancel).is_err());
        assert!(
            (0..pack.len())
                .all(|end| Imported::read(format, &pack[..end], limits, &cancel).is_err())
        );
        pack[12] ^= 1;
        assert!(matches!(
            Imported::read(format, &pack, limits, &cancel),
            Err(FetchError::Pack(Error::Corrupt("received pack checksum")))
        ));
    }
}

#[cfg(test)]
mod git_format_tests {
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    use rstest::rstest;

    use super::*;
    use crate::{InitKind, ObjectFormat, Repository};

    /// Original CLI observations; no upstream implementation or test source is used.
    fn git(path: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let mut child = command
            .current_dir(path)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", path.join("absent-config"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    }

    fn received(repo: &Repository, delta_option: &str) -> (Vec<u8>, Vec<(ObjectId, Vec<u8>)>) {
        let mut names = Vec::new();
        let mut records = Vec::new();
        for variant in 0..8 {
            let mut state = 123456789u64;
            let mut bytes: Vec<_> = (0..16384)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state as u8
                })
                .collect();
            bytes[variant * 100] ^= 255;
            let name = git(repo.git_dir(), &["hash-object", "-w", "--stdin"], &bytes);
            records.push((
                std::str::from_utf8(&name).unwrap().trim().parse().unwrap(),
                bytes,
            ));
            names.extend(name);
        }
        let pack = git(
            repo.git_dir(),
            &[
                "pack-objects",
                "--stdout",
                "--no-reuse-object",
                delta_option,
            ],
            &names,
        );
        (pack, records)
    }

    #[rstest]
    #[case::sha1_ref(ObjectFormat::Sha1, "--window=8", 7)]
    #[case::sha256_ref(ObjectFormat::Sha256, "--window=8", 7)]
    #[case::sha1_ofs(ObjectFormat::Sha1, "--delta-base-offset", 6)]
    #[case::sha256_ofs(ObjectFormat::Sha256, "--delta-base-offset", 6)]
    fn imports_git_deltas_and_git_verifies_generated_index(
        #[case] format: ObjectFormat,
        #[case] option: &str,
        #[case] delta_type: u8,
    ) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
        let (pack, records) = received(&repo, option);
        let imported = Imported::read(
            format,
            &pack,
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(imported.objects.len(), records.len());
        assert!(
            records
                .iter()
                .all(|(id, bytes)| imported.objects[id].data() == bytes)
        );
        let base = repo
            .object_dir()
            .join("pack")
            .join(format!("pack-{}", imported.checksum));
        std::fs::write(base.with_extension("pack"), &pack).unwrap();
        std::fs::write(base.with_extension("idx"), &imported.index).unwrap();
        let report = git(
            repo.git_dir(),
            &[
                "verify-pack",
                "-v",
                base.with_extension("idx").to_str().unwrap(),
            ],
            b"",
        );
        let report = String::from_utf8(report).unwrap();
        let delta_offsets: Vec<usize> = report
            .lines()
            .filter_map(|line| {
                let words: Vec<_> = line.split_whitespace().collect();
                (words.len() == 7).then(|| words[4].parse().unwrap())
            })
            .collect();
        assert!(!delta_offsets.is_empty());
        assert!(
            delta_offsets
                .iter()
                .all(|offset| (pack[*offset] >> 4) & 7 == delta_type)
        );
        git(
            repo.git_dir(),
            &[
                "index-pack",
                "--index-version=2",
                "-o",
                "independent.idx",
                base.with_extension("pack").to_str().unwrap(),
            ],
            b"",
        );
        assert_eq!(
            std::fs::read(repo.git_dir().join("independent.idx")).unwrap(),
            imported.index
        );
    }
}
