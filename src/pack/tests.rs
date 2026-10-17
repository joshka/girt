//! Original binary fixtures built from the documented format, never Git source or test code.
use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use rstest::rstest;

use super::delta;
use super::reader::Pack;
use crate::{ObjectFormat, ObjectId, ObjectKind, ObjectReadError as Error, ReadLimits};

fn seal(format: ObjectFormat, bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(format.checksum(bytes).as_bytes());
}

fn reseal(format: ObjectFormat, bytes: &mut Vec<u8>) {
    bytes.truncate(bytes.len() - format.digest_len());
    seal(format, bytes);
}

fn compressed(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn entry(kind: u8, size: usize, base: &[u8], data: &[u8]) -> Vec<u8> {
    let mut remaining = size >> 4;
    let mut header = vec![(kind << 4) | (size as u8 & 15) | if remaining != 0 { 128 } else { 0 }];
    while remaining != 0 {
        let next = remaining as u8 & 127;
        remaining >>= 7;
        header.push(next | if remaining != 0 { 128 } else { 0 });
    }
    header.extend_from_slice(base);
    header.extend_from_slice(&compressed(data));
    header
}

fn fixture(format: ObjectFormat, entries: &[(ObjectId, Vec<u8>)]) -> (Vec<u8>, Vec<u8>) {
    let mut pack = b"PACK\0\0\0\x02".to_vec();
    pack.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    let mut records = Vec::new();
    for (id, data) in entries {
        records.push((*id, pack.len() as u32, crc32fast::hash(data)));
        pack.extend_from_slice(data);
    }
    seal(format, &mut pack);
    records.sort_by_key(|record| record.0);
    let mut index = b"\xfftOc\0\0\0\x02".to_vec();
    for first in 0..=255 {
        let count = records
            .iter()
            .filter(|r| r.0.as_bytes()[0] <= first)
            .count() as u32;
        index.extend_from_slice(&count.to_be_bytes());
    }
    for (id, _, _) in &records {
        index.extend_from_slice(id.as_bytes());
    }
    for (_, _, crc) in &records {
        index.extend_from_slice(&crc.to_be_bytes());
    }
    for (_, offset, _) in &records {
        index.extend_from_slice(&offset.to_be_bytes());
    }
    index.extend_from_slice(&pack[pack.len() - format.digest_len()..]);
    seal(format, &mut index);
    (index, pack)
}

fn ordinary(format: ObjectFormat) -> (Vec<u8>, Vec<u8>) {
    fixture(
        format,
        &[(
            ObjectId::for_blob(format, b"hello"),
            entry(3, 5, b"", b"hello"),
        )],
    )
}

fn open_read(
    format: ObjectFormat,
    entries: &[(ObjectId, Vec<u8>)],
    id: ObjectId,
    limits: ReadLimits,
) -> Result<crate::Object, Error> {
    let (index, data) = fixture(format, entries);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.idx");
    std::fs::write(&path, index).unwrap();
    std::fs::write(path.with_extension("pack"), data).unwrap();
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let pack = super::FilePack::open(format, &path, &mut crate::PackLimits::default(), &cancelled)?;
    pack.read(pack.find(id)?.unwrap(), limits, &cancelled)
}

#[rstest]
#[case::blob_sha1(ObjectFormat::Sha1, 3, ObjectKind::Blob)]
#[case::blob_sha256(ObjectFormat::Sha256, 3, ObjectKind::Blob)]
#[case::tree_sha1(ObjectFormat::Sha1, 2, ObjectKind::Tree)]
#[case::tree_sha256(ObjectFormat::Sha256, 2, ObjectKind::Tree)]
#[case::commit_sha1(ObjectFormat::Sha1, 1, ObjectKind::Commit)]
#[case::commit_sha256(ObjectFormat::Sha256, 1, ObjectKind::Commit)]
#[case::tag_sha1(ObjectFormat::Sha1, 4, ObjectKind::Tag)]
#[case::tag_sha256(ObjectFormat::Sha256, 4, ObjectKind::Tag)]
fn returns_exact_kind_and_payload(
    #[case] format: ObjectFormat,
    #[case] code: u8,
    #[case] kind: ObjectKind,
) {
    let id = format.hash_object(kind, b"raw payload");
    let object = open_read(
        format,
        &[(id, entry(code, 11, b"", b"raw payload"))],
        id,
        ReadLimits::default(),
    )
    .unwrap();
    assert_eq!(object.kind(), kind);
    assert_eq!(object.data(), b"raw payload");
    assert_eq!(object.id(), id);
}

#[rstest]
#[case::ofs_sha1(ObjectFormat::Sha1, 6, |_: ObjectId, length| vec![length as u8])]
#[case::ofs_sha256(ObjectFormat::Sha256, 6, |_: ObjectId, length| vec![length as u8])]
#[case::reference_sha1(ObjectFormat::Sha1, 7, |id: ObjectId, _| id.as_bytes().to_vec())]
#[case::reference_sha256(ObjectFormat::Sha256, 7, |id: ObjectId, _| id.as_bytes().to_vec())]
fn reconstructs_delta(
    #[case] format: ObjectFormat,
    #[case] code: u8,
    #[case] reference: fn(ObjectId, usize) -> Vec<u8>,
) {
    let base_id = ObjectId::for_blob(format, b"hello");
    let id = ObjectId::for_blob(format, b"hello!");
    let base = entry(3, 5, b"", b"hello");
    let reference = reference(base_id, base.len());
    let program = [5, 6, 0x90, 5, 1, b'!'];
    let object = open_read(
        format,
        &[
            (base_id, base),
            (id, entry(code, program.len(), &reference, &program)),
        ],
        id,
        ReadLimits::default(),
    )
    .unwrap();
    assert_eq!(object.data(), b"hello!");
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn resolves_forward_reference_and_nested_deltas(#[case] format: ObjectFormat) {
    let base = ObjectId::for_blob(format, b"a");
    let middle = ObjectId::for_blob(format, b"ab");
    let tip = ObjectId::for_blob(format, b"abc");
    let entries = [
        (
            tip,
            entry(7, 6, middle.as_bytes(), &[2, 3, 3, b'a', b'b', b'c']),
        ),
        (middle, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
        (base, entry(3, 1, b"", b"a")),
    ];
    let object = open_read(format, &entries, tip, ReadLimits::default()).unwrap();
    assert_eq!(object.data(), b"abc");
}

#[rstest]
#[case::reserved(&[3, 3, 0], "reserved delta instruction")]
#[case::base_length(&[2, 3, 0x90, 3], "delta base size")]
#[case::copy_bounds(&[3, 3, 0x91, 2, 3], "delta copy outside base")]
#[case::truncated_copy(&[3, 3, 0x91], "truncated delta or entry")]
#[case::truncated_insert(&[3, 3, 3, b'a'], "truncated delta insertion")]
#[case::short_result(&[3, 3, 1, b'a'], "delta result size")]
#[case::long_result(&[3, 1, 2, b'a', b'b'], "delta exceeds result size")]
#[case::size_overflow(&[255; 20], "size overflow")]
fn rejects_invalid_delta_programs(#[case] program: &[u8], #[case] reason: &str) {
    let error = delta::apply(b"abc", program, 100, &mut 1000).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("corrupt packed storage: {reason}")
    );
}

#[test]
fn decodes_sparse_copy_operands_and_default_copy_length() {
    let base = vec![42; 65536];
    let result = delta::apply(
        &base,
        &[0x80, 0x80, 4, 0x80, 0x80, 4, 0x80],
        65536,
        &mut 65536,
    )
    .unwrap();
    assert_eq!(result, base);
}

#[test]
fn decodes_copy_offset_without_low_byte() {
    let mut base = vec![0; 257];
    base[256] = 42;
    let result = delta::apply(&base, &[0x81, 2, 1, 0x92, 1, 1], 1, &mut 1).unwrap();
    assert_eq!(result, [42]);
}

#[rstest]
#[case::short_header_sha1(ObjectFormat::Sha1, 3)]
#[case::short_header_sha256(ObjectFormat::Sha256, 3)]
#[case::short_version_sha1(ObjectFormat::Sha1, 7)]
#[case::short_version_sha256(ObjectFormat::Sha256, 7)]
#[case::short_fanout_sha1(ObjectFormat::Sha1, 1031)]
#[case::short_fanout_sha256(ObjectFormat::Sha256, 1031)]
#[case::short_ids_sha1(ObjectFormat::Sha1, 1040)]
#[case::short_ids_sha256(ObjectFormat::Sha256, 1040)]
#[case::short_trailer_sha1(ObjectFormat::Sha1, 1099)]
#[case::short_trailer_sha256(ObjectFormat::Sha256, 1099)]
fn rejects_truncated_indexes(#[case] format: ObjectFormat, #[case] length: usize) {
    let (mut index, data) = ordinary(format);
    index.truncate(length);
    assert!(Pack::open(format, &index, data).is_err());
}

#[rstest]
#[case::fanout_sha1(ObjectFormat::Sha1, |_| 8, 1, "index fanout")]
#[case::fanout_sha256(ObjectFormat::Sha256, |_| 8, 1, "index fanout")]
#[case::offset_header_sha1(ObjectFormat::Sha1, |f: ObjectFormat| 1036 + f.digest_len(), 11, "offset outside pack entries")]
#[case::offset_header_sha256(ObjectFormat::Sha256, |f: ObjectFormat| 1036 + f.digest_len(), 11, "offset outside pack entries")]
#[case::offset_past_pack_sha1(ObjectFormat::Sha1, |f: ObjectFormat| 1036 + f.digest_len(), 9999, "offset outside pack entries")]
#[case::offset_past_pack_sha256(ObjectFormat::Sha256, |f: ObjectFormat| 1036 + f.digest_len(), 9999, "offset outside pack entries")]
#[case::offset_gap_sha1(ObjectFormat::Sha1, |f: ObjectFormat| 1036 + f.digest_len(), 13, "unindexed pack bytes")]
#[case::offset_gap_sha256(ObjectFormat::Sha256, |f: ObjectFormat| 1036 + f.digest_len(), 13, "unindexed pack bytes")]
#[case::large_slot_sha1(ObjectFormat::Sha1, |f: ObjectFormat| 1036 + f.digest_len(), 0x8000_0000, "large offset index")]
#[case::large_slot_sha256(ObjectFormat::Sha256, |f: ObjectFormat| 1036 + f.digest_len(), 0x8000_0000, "large offset index")]
#[case::crc_sha1(ObjectFormat::Sha1, |f: ObjectFormat| 1032 + f.digest_len(), 0, "entry CRC")]
#[case::crc_sha256(ObjectFormat::Sha256, |f: ObjectFormat| 1032 + f.digest_len(), 0, "entry CRC")]
fn rejects_invalid_index_fields(
    #[case] format: ObjectFormat,
    #[case] offset: fn(ObjectFormat) -> usize,
    #[case] value: u32,
    #[case] reason: &str,
) {
    let (mut index, data) = ordinary(format);
    let offset = offset(format);
    index[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    reseal(format, &mut index);
    let error = Pack::open(format, &index, data).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("corrupt packed storage: {reason}")
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn supports_index_large_offset_table(#[case] format: ObjectFormat) {
    let records = [b"hello", b"world"].map(|data| (ObjectId::for_blob(format, data), data));
    let entries = records.map(|(id, data)| (id, entry(3, 5, b"", data)));
    let (mut index, data) = fixture(format, &entries);
    // The entry after the first moves to the one large offset slot two entries allow.
    let table = 1032 + 2 * (format.digest_len() + 4);
    let position = (0..2)
        .find(|position| index[table + 4 * position..][..4] != 12u32.to_be_bytes())
        .unwrap();
    let at = table + 4 * position;
    let offset = u32::from_be_bytes(index[at..at + 4].try_into().unwrap());
    index[at..at + 4].copy_from_slice(&0x8000_0000u32.to_be_bytes());
    index.splice(table + 8..table + 8, u64::from(offset).to_be_bytes());
    reseal(format, &mut index);
    let pack = Pack::open(format, &index, data).unwrap();
    for (id, data) in records {
        let position = pack.find(id).unwrap();
        assert_eq!(
            pack.read(position, ReadLimits::default()).unwrap().data(),
            data
        );
    }
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rejects_index_checksum(#[case] format: ObjectFormat) {
    let (mut index, data) = ordinary(format);
    index[1040] ^= 1;
    assert!(matches!(
        Pack::open(format, &index, data),
        Err(Error::Corrupt("index checksum"))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rejects_pack_checksum(#[case] format: ObjectFormat) {
    let (index, mut data) = ordinary(format);
    data[14] ^= 1;
    assert!(matches!(
        Pack::open(format, &index, data),
        Err(Error::Corrupt("pack checksum"))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rejects_index_pack_disagreement(#[case] format: ObjectFormat) {
    let (mut index, data) = ordinary(format);
    index[1040 + format.digest_len()] ^= 1;
    reseal(format, &mut index);
    assert!(matches!(
        Pack::open(format, &index, data),
        Err(Error::Corrupt("index/pack checksum disagreement"))
    ));
}

#[rstest]
#[case::legacy_sha1(ObjectFormat::Sha1, 1)]
#[case::legacy_sha256(ObjectFormat::Sha256, 1)]
#[case::future_sha1(ObjectFormat::Sha1, 3)]
#[case::future_sha256(ObjectFormat::Sha256, 3)]
fn rejects_unsupported_index_versions(#[case] format: ObjectFormat, #[case] version: u32) {
    let (mut index, data) = ordinary(format);
    index[4..8].copy_from_slice(&version.to_be_bytes());
    assert!(
        matches!(Pack::open(format, &index, data), Err(Error::IndexVersion(found)) if found == version)
    );
}

#[rstest]
#[case::legacy_sha1(ObjectFormat::Sha1, 1)]
#[case::legacy_sha256(ObjectFormat::Sha256, 1)]
#[case::future_sha1(ObjectFormat::Sha1, 4)]
#[case::future_sha256(ObjectFormat::Sha256, 4)]
fn rejects_unsupported_pack_versions(#[case] format: ObjectFormat, #[case] version: u32) {
    let (index, mut data) = ordinary(format);
    data[4..8].copy_from_slice(&version.to_be_bytes());
    assert!(
        matches!(Pack::open(format, &index, data), Err(Error::PackVersion(found)) if found == version)
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rejects_malformed_headerless_index(#[case] format: ObjectFormat) {
    let (_, data) = ordinary(format);
    assert!(matches!(
        Pack::open(format, &[0; 1088], data),
        Err(Error::Corrupt(_))
    ));
}

#[rstest]
#[case::zero_distance_sha1(ObjectFormat::Sha1, &[0], "zero delta distance")]
#[case::zero_distance_sha256(ObjectFormat::Sha256, &[0], "zero delta distance")]
#[case::before_pack_sha1(ObjectFormat::Sha1, &[127], "delta offset before pack")]
#[case::before_pack_sha256(ObjectFormat::Sha256, &[127], "delta offset before pack")]
#[case::inside_header_sha1(ObjectFormat::Sha1, &[1], "offset outside pack entries")]
#[case::inside_header_sha256(ObjectFormat::Sha256, &[1], "offset outside pack entries")]
#[case::overflow_sha1(ObjectFormat::Sha1, &[255; 20], "delta offset overflow")]
#[case::overflow_sha256(ObjectFormat::Sha256, &[255; 20], "delta offset overflow")]
fn rejects_bad_ofs_bases(#[case] format: ObjectFormat, #[case] base: &[u8], #[case] reason: &str) {
    let id = ObjectId::for_blob(format, b"x");
    let error = open_read(
        format,
        &[(id, entry(6, 3, base, &[1, 1, 0x80]))],
        id,
        ReadLimits::default(),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("corrupt packed storage: {reason}")
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rejects_missing_ref_base(#[case] format: ObjectFormat) {
    let id = ObjectId::for_blob(format, b"x");
    let base = ObjectId::for_blob(format, b"absent");
    let error = open_read(
        format,
        &[(id, entry(7, 3, base.as_bytes(), &[1, 1, 0x80]))],
        id,
        ReadLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(error, Error::MissingBase(found) if found == base));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn detects_ref_cycles_without_recursion(#[case] format: ObjectFormat) {
    let first = ObjectId::for_blob(format, b"first");
    let second = ObjectId::for_blob(format, b"second");
    let entries = [
        (first, entry(7, 0, second.as_bytes(), b"")),
        (second, entry(7, 0, first.as_bytes(), b"")),
    ];
    assert!(matches!(
        open_read(format, &entries, first, ReadLimits::default()),
        Err(Error::DeltaCycle)
    ));
}

#[rstest]
#[case::wrong_length_sha1(ObjectFormat::Sha1, 3, 4, "inflated entry exceeds declared size")]
#[case::wrong_length_sha256(ObjectFormat::Sha256, 3, 4, "inflated entry exceeds declared size")]
#[case::too_long_sha1(ObjectFormat::Sha1, 3, 6, "packed entry length or trailing data")]
#[case::too_long_sha256(ObjectFormat::Sha256, 3, 6, "packed entry length or trailing data")]
#[case::wrong_identity_sha1(ObjectFormat::Sha1, 3, 5, "object identity")]
#[case::wrong_identity_sha256(ObjectFormat::Sha256, 3, 5, "object identity")]
fn rejects_bad_entry_payloads(
    #[case] format: ObjectFormat,
    #[case] kind: u8,
    #[case] size: usize,
    #[case] reason: &str,
) {
    let id = ObjectId::for_blob(format, b"other");
    let error = open_read(
        format,
        &[(id, entry(kind, size, b"", b"hello"))],
        id,
        ReadLimits::default(),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("corrupt packed storage: {reason}")
    );
}

#[rstest]
#[case::invalid_sha1(ObjectFormat::Sha1, 0)]
#[case::invalid_sha256(ObjectFormat::Sha256, 0)]
#[case::reserved_sha1(ObjectFormat::Sha1, 5)]
#[case::reserved_sha256(ObjectFormat::Sha256, 5)]
fn rejects_reserved_kinds(#[case] format: ObjectFormat, #[case] kind: u8) {
    let id = ObjectId::for_blob(format, b"");
    assert!(
        matches!(open_read(format, &[(id, entry(kind, 0, b"", b""))], id, ReadLimits::default()), Err(Error::ObjectType(found)) if found == kind)
    );
}

#[rstest]
#[case::payload_sha1(ObjectFormat::Sha1, ReadLimits { max_object_bytes: 0, ..ReadLimits::default() }, "object bytes")]
#[case::payload_sha256(ObjectFormat::Sha256, ReadLimits { max_object_bytes: 0, ..ReadLimits::default() }, "object bytes")]
#[case::program_sha1(ObjectFormat::Sha1, ReadLimits { max_delta_bytes: 0, ..ReadLimits::default() }, "delta program bytes")]
#[case::program_sha256(ObjectFormat::Sha256, ReadLimits { max_delta_bytes: 0, ..ReadLimits::default() }, "delta program bytes")]
#[case::depth_sha1(ObjectFormat::Sha1, ReadLimits { max_delta_depth: 0, ..ReadLimits::default() }, "delta depth")]
#[case::depth_sha256(ObjectFormat::Sha256, ReadLimits { max_delta_depth: 0, ..ReadLimits::default() }, "delta depth")]
#[case::cumulative_sha1(ObjectFormat::Sha1, ReadLimits { max_decode_bytes: 5, ..ReadLimits::default() }, "cumulative decode bytes")]
#[case::cumulative_sha256(ObjectFormat::Sha256, ReadLimits { max_decode_bytes: 5, ..ReadLimits::default() }, "cumulative decode bytes")]
fn enforces_read_limits(
    #[case] format: ObjectFormat,
    #[case] limits: ReadLimits,
    #[case] reason: &str,
) {
    let base = ObjectId::for_blob(format, b"a");
    let id = ObjectId::for_blob(format, b"ab");
    let entries = [
        (base, entry(3, 1, b"", b"a")),
        (id, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
    ];
    let error = open_read(format, &entries, id, limits).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("object storage limit exceeded: {reason}")
    );
}

#[test]
fn enforces_reconstructed_size_before_allocation() {
    assert!(matches!(
        delta::apply(b"a", &[1, 2, 2, b'a', b'b'], 1, &mut 99),
        Err(Error::Limit("object bytes"))
    ));
}

#[test]
fn charges_reconstruction_against_cumulative_limit() {
    assert!(matches!(
        delta::apply(b"a", &[1, 2, 2, b'a', b'b'], 2, &mut 1),
        Err(Error::Limit("cumulative decode bytes"))
    ));
}

/// A base cached while reconstructing a dependent is still checked when it's read itself.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn cached_base_is_checked_when_requested(#[case] format: ObjectFormat) {
    let base = ObjectId::for_blob(format, b"wrong");
    let id = ObjectId::for_blob(format, b"ab");
    let entries = [
        (base, entry(3, 1, b"", b"a")),
        (id, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
    ];
    let (index, data) = fixture(format, &entries);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.idx");
    std::fs::write(&path, index).unwrap();
    std::fs::write(path.with_extension("pack"), data).unwrap();
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let pack = super::FilePack::open(format, &path, &mut crate::PackLimits::default(), &cancelled)
        .unwrap();
    let read = |id| {
        pack.read(
            pack.find(id).unwrap().unwrap(),
            ReadLimits::default(),
            &cancelled,
        )
    };
    assert_eq!(read(id).unwrap().data(), b"ab");
    assert!(matches!(read(base), Err(Error::Corrupt("object identity"))));
    assert_eq!(read(id).unwrap().data(), b"ab");
}

/// Only the requested object's identity is checked, as with Git's reads: a mislabelled base
/// fails when read itself, but doesn't hide a dependent whose reconstructed content is correct.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn checks_identity_of_requested_object(#[case] format: ObjectFormat) {
    let base = ObjectId::for_blob(format, b"wrong");
    let id = ObjectId::for_blob(format, b"ab");
    let entries = [
        (base, entry(3, 1, b"", b"a")),
        (id, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
    ];
    assert!(matches!(
        open_read(format, &entries, base, ReadLimits::default()),
        Err(Error::Corrupt("object identity"))
    ));
    assert_eq!(
        open_read(format, &entries, id, ReadLimits::default())
            .unwrap()
            .data(),
        b"ab"
    );
}

#[rstest]
#[case::zero_sha1(ObjectFormat::Sha1, 0)]
#[case::zero_sha256(ObjectFormat::Sha256, 0)]
#[case::header_sha1(ObjectFormat::Sha1, 11)]
#[case::header_sha256(ObjectFormat::Sha256, 11)]
#[case::trailer_sha1(ObjectFormat::Sha1, 31)]
#[case::trailer_sha256(ObjectFormat::Sha256, 31)]
#[case::entry_sha1(ObjectFormat::Sha1, 35)]
#[case::entry_sha256(ObjectFormat::Sha256, 35)]
fn rejects_truncated_packs(#[case] format: ObjectFormat, #[case] length: usize) {
    let (index, mut data) = ordinary(format);
    data.truncate(length);
    assert!(Pack::open(format, &index, data).is_err());
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn accepts_empty_pack_and_reports_index_miss(#[case] format: ObjectFormat) {
    let (index, data) = fixture(format, &[]);
    let pack = Pack::open(format, &index, data).unwrap();
    assert_eq!(pack.find(ObjectId::for_blob(format, b"")), None);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rejects_pack_count_mismatch(#[case] format: ObjectFormat) {
    let (index, mut data) = ordinary(format);
    data[8..12].copy_from_slice(&2u32.to_be_bytes());
    assert!(matches!(
        Pack::open(format, &index, data),
        Err(Error::Corrupt("pack object count"))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rejects_duplicate_index_ids(#[case] format: ObjectFormat) {
    let id = ObjectId::for_blob(format, b"a");
    let (index, data) = fixture(
        format,
        &[(id, entry(3, 1, b"", b"a")), (id, entry(3, 1, b"", b"a"))],
    );
    assert!(matches!(
        Pack::open(format, &index, data),
        Err(Error::Corrupt("unsorted or duplicate index identities"))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rejects_unsorted_index_ids(#[case] format: ObjectFormat) {
    let (mut index, data) = fixture(
        format,
        &[
            (ObjectId::for_blob(format, b"a"), entry(3, 1, b"", b"a")),
            (ObjectId::for_blob(format, b"b"), entry(3, 1, b"", b"b")),
        ],
    );
    index[1032..1032 + 2 * format.digest_len()].rotate_left(format.digest_len());
    reseal(format, &mut index);
    assert!(matches!(
        Pack::open(format, &index, data),
        Err(Error::Corrupt("unsorted or duplicate index identities"))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rejects_duplicate_pack_offsets(#[case] format: ObjectFormat) {
    let (mut index, data) = fixture(
        format,
        &[
            (ObjectId::for_blob(format, b"a"), entry(3, 1, b"", b"a")),
            (ObjectId::for_blob(format, b"b"), entry(3, 1, b"", b"b")),
        ],
    );
    let offset: [u8; 4] = index[1040 + 2 * format.digest_len()..1044 + 2 * format.digest_len()]
        .try_into()
        .unwrap();
    index[1044 + 2 * format.digest_len()..1048 + 2 * format.digest_len()].copy_from_slice(&offset);
    reseal(format, &mut index);
    assert!(matches!(
        Pack::open(format, &index, data),
        Err(Error::Corrupt("duplicate pack offset"))
    ));
}

fn truncated_zlib() -> Vec<u8> {
    let mut data = entry(3, 5, b"", b"hello");
    data.pop();
    data
}

fn trailing_zlib() -> Vec<u8> {
    let mut data = entry(3, 5, b"", b"hello");
    data.push(0);
    data
}

fn corrupt_zlib() -> Vec<u8> {
    let mut data = entry(3, 5, b"", b"hello");
    *data.last_mut().unwrap() ^= 1;
    data
}

#[rstest]
#[case::truncated_sha1(ObjectFormat::Sha1, truncated_zlib, "truncated packed zlib stream")]
#[case::truncated_sha256(ObjectFormat::Sha256, truncated_zlib, "truncated packed zlib stream")]
// A read finds an entry's end where its zlib stream ends, so bytes after the stream fall outside
// the bytes it checks against the CRC the index recorded for the whole entry.
#[case::trailing_sha1(ObjectFormat::Sha1, trailing_zlib, "entry CRC")]
#[case::trailing_sha256(ObjectFormat::Sha256, trailing_zlib, "entry CRC")]
#[case::checksum_sha1(ObjectFormat::Sha1, corrupt_zlib, "invalid packed zlib stream")]
#[case::checksum_sha256(ObjectFormat::Sha256, corrupt_zlib, "invalid packed zlib stream")]
fn validates_zlib_even_with_valid_pack_checksums(
    #[case] format: ObjectFormat,

    #[case] data: fn() -> Vec<u8>,
    #[case] reason: &str,
) {
    let id = ObjectId::for_blob(format, b"hello");
    let error = open_read(format, &[(id, data())], id, ReadLimits::default()).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("corrupt packed storage: {reason}")
    );
}

#[rstest]
#[case::truncated_size_sha1(ObjectFormat::Sha1, vec![0xb0], "truncated delta or entry")]
#[case::truncated_size_sha256(ObjectFormat::Sha256, vec![0xb0], "truncated delta or entry")]
#[case::overflowing_size_sha1(ObjectFormat::Sha1, vec![0xff; 20], "size overflow")]
#[case::overflowing_size_sha256(ObjectFormat::Sha256, vec![0xff; 20], "size overflow")]
#[case::truncated_reference_sha1(ObjectFormat::Sha1, vec![0x70, 1, 2], "truncated base identity")]
#[case::truncated_reference_sha256(ObjectFormat::Sha256, vec![0x70, 1, 2], "truncated base identity")]
#[case::truncated_offset_sha1(ObjectFormat::Sha1, vec![0x60, 0x80], "truncated delta or entry")]
#[case::truncated_offset_sha256(ObjectFormat::Sha256, vec![0x60, 0x80], "truncated delta or entry")]
fn rejects_truncated_entry_metadata(
    #[case] format: ObjectFormat,
    #[case] data: Vec<u8>,
    #[case] reason: &str,
) {
    let id = ObjectId::for_blob(format, b"hello");
    let error = open_read(format, &[(id, data)], id, ReadLimits::default()).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("corrupt packed storage: {reason}")
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn permits_exact_depth_and_decode_budget(#[case] format: ObjectFormat) {
    let base = ObjectId::for_blob(format, b"a");
    let id = ObjectId::for_blob(format, b"ab");
    let entries = [
        (base, entry(3, 1, b"", b"a")),
        (id, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
    ];
    let limits = ReadLimits {
        max_object_bytes: 2,
        max_delta_bytes: 5,
        max_decode_bytes: 8,
        max_delta_depth: 1,
        ..ReadLimits::default()
    };
    assert_eq!(
        open_read(format, &entries, id, limits).unwrap().data(),
        b"ab"
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rejects_external_base_even_when_it_exists_in_another_pack(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("pack");
    std::fs::create_dir(&directory).unwrap();
    let base = ObjectId::for_blob(format, b"a");
    let id = ObjectId::for_blob(format, b"ab");
    let (base_index, base_data) = fixture(format, &[(base, entry(3, 1, b"", b"a"))]);
    let (index, data) = fixture(
        format,
        &[(id, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b']))],
    );
    std::fs::write(directory.join("base.idx"), base_index).unwrap();
    std::fs::write(directory.join("base.pack"), base_data).unwrap();
    std::fs::write(directory.join("delta.idx"), index).unwrap();
    std::fs::write(directory.join("delta.pack"), data).unwrap();
    let objects = crate::Objects::open(
        format,
        root.path(),
        crate::PackLimits::default(),
        crate::AlternateLimits::default(),
    )
    .unwrap();
    assert!(
        matches!(objects.read(id, ReadLimits::default()), Err(Error::MissingBase(found)) if found == base)
    );
    assert_eq!(
        objects
            .read(base, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        b"a"
    );
}

#[rstest]
#[case::tree_sha1(ObjectFormat::Sha1, 2, ObjectKind::Tree)]
#[case::tree_sha256(ObjectFormat::Sha256, 2, ObjectKind::Tree)]
#[case::commit_sha1(ObjectFormat::Sha1, 1, ObjectKind::Commit)]
#[case::commit_sha256(ObjectFormat::Sha256, 1, ObjectKind::Commit)]
#[case::tag_sha1(ObjectFormat::Sha1, 4, ObjectKind::Tag)]
#[case::tag_sha256(ObjectFormat::Sha256, 4, ObjectKind::Tag)]
fn delta_inherits_base_kind(
    #[case] format: ObjectFormat,
    #[case] code: u8,
    #[case] kind: ObjectKind,
) {
    let base = format.hash_object(kind, b"a");
    let id = format.hash_object(kind, b"ab");
    let entries = [
        (base, entry(code, 1, b"", b"a")),
        (id, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
    ];
    let object = open_read(format, &entries, id, ReadLimits::default()).unwrap();
    assert_eq!(object.kind(), kind);
    assert_eq!(object.data(), b"ab");
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn bounds_long_chains_without_recursive_calls(#[case] format: ObjectFormat) {
    let ids: Vec<_> = (0u64..1025)
        .map(|number| ObjectId::for_blob(format, &number.to_be_bytes()))
        .collect();
    let entries: Vec<_> = ids
        .windows(2)
        .map(|pair| (pair[0], entry(7, 2, pair[1].as_bytes(), &[0, 0])))
        .chain(std::iter::once((ids[1024], entry(3, 0, b"", b""))))
        .collect();
    let limits = ReadLimits {
        max_delta_depth: 512,
        ..ReadLimits::default()
    };
    assert!(matches!(
        open_read(format, &entries, ids[0], limits),
        Err(Error::Limit("delta depth"))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1, ObjectFormat::Sha256)]
#[case::sha256(ObjectFormat::Sha256, ObjectFormat::Sha1)]
fn rejects_wrong_format_even_for_empty_pack(
    #[case] format: ObjectFormat,
    #[case] other: ObjectFormat,
) {
    let (index, data) = fixture(format, &[]);
    assert!(Pack::open(other, &index, data).is_err());
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn verifies_delta_result_identity_after_valid_base(#[case] format: ObjectFormat) {
    let base = ObjectId::for_blob(format, b"a");
    let wrong = ObjectId::for_blob(format, b"wrong");
    let entries = [
        (base, entry(3, 1, b"", b"a")),
        (wrong, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
    ];
    assert!(matches!(
        open_read(format, &entries, wrong, ReadLimits::default()),
        Err(Error::Corrupt("object identity"))
    ));
}

#[test]
fn overflowing_word_offset_returns_corruption() {
    assert!(matches!(
        super::index::word(&[], usize::MAX),
        Err(Error::Corrupt("truncated integer"))
    ));
}

#[test]
fn three_byte_word_is_truncated() {
    assert!(matches!(
        super::index::word(&[0, 0, 0], 0),
        Err(Error::Corrupt("truncated integer"))
    ));
    assert!(matches!(super::index::word(&[0, 0, 0, 7], 0), Ok(7)));
}

fn legacy_index(format: ObjectFormat) -> (Vec<u8>, Vec<u8>) {
    let (v2, data) = ordinary(format);
    let width = format.digest_len();
    let mut index = v2[8..1032].to_vec();
    index.extend_from_slice(&12u32.to_be_bytes());
    index.extend_from_slice(&v2[1032..1032 + width]);
    index.extend_from_slice(&data[data.len() - width..]);
    seal(format, &mut index);
    (index, data)
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn reads_headerless_index(#[case] format: ObjectFormat) {
    let (index, data) = legacy_index(format);
    let pack = Pack::open(format, &index, data).unwrap();
    assert_eq!(
        pack.read(0, ReadLimits::default()).unwrap().data(),
        b"hello"
    );
}

#[rstest]
#[case::fanout_sha1(ObjectFormat::Sha1, 0, 1, "index fanout")]
#[case::fanout_sha256(ObjectFormat::Sha256, 0, 1, "index fanout")]
#[case::offset_sha1(ObjectFormat::Sha1, 1024, 0, "offset outside pack entries")]
#[case::offset_sha256(ObjectFormat::Sha256, 1024, 0, "offset outside pack entries")]
#[case::count_sha1(ObjectFormat::Sha1, 1020, u32::MAX, "index table length")]
#[case::count_sha256(ObjectFormat::Sha256, 1020, u32::MAX, "index table length")]
fn rejects_invalid_legacy_index(
    #[case] format: ObjectFormat,
    #[case] offset: usize,
    #[case] value: u32,
    #[case] reason: &str,
) {
    let (mut index, data) = legacy_index(format);
    index[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    reseal(format, &mut index);
    assert!(
        matches!(Pack::open(format, &index, data), Err(Error::Corrupt(found)) if found == reason)
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rejects_legacy_index_checksum_damage(#[case] format: ObjectFormat) {
    let (mut index, data) = legacy_index(format);
    let last = index.len() - 1;
    index[last] ^= 1;
    assert!(matches!(
        Pack::open(format, &index, data),
        Err(Error::Corrupt("index checksum"))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rejects_truncated_legacy_index(#[case] format: ObjectFormat) {
    let (mut index, data) = legacy_index(format);
    index.pop();
    assert!(matches!(
        Pack::open(format, &index, data),
        Err(Error::Corrupt("index table length"))
    ));
}

fn open_file_pair(
    format: ObjectFormat,
    index: &[u8],
    data: &[u8],
) -> Result<super::FilePack, Error> {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.idx");
    std::fs::write(&path, index).unwrap();
    std::fs::write(path.with_extension("pack"), data).unwrap();
    super::FilePack::open(
        format,
        &path,
        &mut crate::PackLimits::default(),
        &std::sync::atomic::AtomicBool::new(false),
    )
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn explicit_file_verification_rejects_entry_crc(#[case] format: ObjectFormat) {
    let (mut index, data) = ordinary(format);
    index[1032 + format.digest_len()] ^= 1;
    reseal(format, &mut index);
    assert!(matches!(
        open_file_pair(format, &index, &data)
            .unwrap()
            .verify(&std::sync::atomic::AtomicBool::new(false)),
        Err(Error::Corrupt("entry CRC"))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn file_open_rejects_damaged_pack_trailer(#[case] format: ObjectFormat) {
    let (index, mut data) = ordinary(format);
    *data.last_mut().unwrap() ^= 1;
    assert!(matches!(
        open_file_pair(format, &index, &data),
        Err(Error::Corrupt("index/pack checksum disagreement"))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn file_validation_rejects_pair_disagreement(#[case] format: ObjectFormat) {
    let (mut index, data) = ordinary(format);
    let position = index.len() - 2 * format.digest_len();
    index[position] ^= 1;
    reseal(format, &mut index);
    assert!(matches!(
        open_file_pair(format, &index, &data),
        Err(Error::Corrupt("index/pack checksum disagreement"))
    ));
}

fn open_file(
    format: ObjectFormat,
    entries: &[(ObjectId, Vec<u8>)],
    damage: Option<usize>,
) -> (tempfile::TempDir, super::FilePack) {
    let (index, mut data) = fixture(format, entries);
    if let Some(offset) = damage {
        data[offset] ^= 0x55;
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.idx");
    std::fs::write(&path, index).unwrap();
    std::fs::write(path.with_extension("pack"), data).unwrap();
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let pack = super::FilePack::open(format, &path, &mut crate::PackLimits::default(), &cancelled)
        .unwrap();
    (root, pack)
}

/// A read that skips the identity hash still rejects a damaged entry through its CRC, before any
/// bytes are returned.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn read_detects_corrupt_entry_without_identity_hash(#[case] format: ObjectFormat) {
    let payload = b"100644 file\0abcdefghijklmnopqrst".to_vec();
    let id = format.hash_object(ObjectKind::Tree, &payload);
    let entries = [(id, entry(2, payload.len(), b"", &payload))];
    // Damage the last zlib byte, inside the entry, so framing still parses.
    let (_root, pack) = open_file(format, &entries, Some(12 + entries[0].1.len() - 1));
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let position = pack.find(id).unwrap().unwrap();
    assert!(matches!(
        pack.read(position, ReadLimits::default(), &cancelled),
        Err(Error::Corrupt(_))
    ));
    assert!(matches!(
        pack.read_unverified(position, ReadLimits::default(), &cancelled),
        Err(Error::Corrupt(_))
    ));
}

/// An index entry naming the CRC-valid bytes of another object fails a verifying read, while an
/// unverified walk read returns the stored bytes. Git's `index-pack` would never write it.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn read_of_mislabelled_entry_under_verifying_policy_fails(#[case] format: ObjectFormat) {
    let claimed = ObjectId::for_blob(format, b"claimed");
    let entries = [(claimed, entry(3, 6, b"", b"stored"))];
    let (_root, pack) = open_file(format, &entries, None);
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let position = pack.find(claimed).unwrap().unwrap();
    assert!(matches!(
        pack.read(position, ReadLimits::default(), &cancelled),
        Err(Error::Corrupt("object identity"))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn read_of_mislabelled_entry_under_trusting_policy_returns_bytes(#[case] format: ObjectFormat) {
    let claimed = ObjectId::for_blob(format, b"claimed");
    let base = ObjectId::for_blob(format, b"also claimed");
    let entries = [
        (base, entry(3, 1, b"", b"a")),
        (
            claimed,
            entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b']),
        ),
    ];
    let (_root, pack) = open_file(format, &entries, None);
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let position = pack.find(claimed).unwrap().unwrap();
    let base_position = pack.find(base).unwrap().unwrap();
    let object = pack
        .read_unverified(position, ReadLimits::default(), &cancelled)
        .unwrap();
    assert_eq!(
        (object.kind(), object.data()),
        (ObjectKind::Blob, &b"ab"[..])
    );
    // The unverified read cached both entries; verifying reads must still check them.
    for position in [position, base_position] {
        assert!(matches!(
            pack.read(position, ReadLimits::default(), &cancelled),
            Err(Error::Corrupt("object identity"))
        ));
    }
}

/// A valid zlib stream may delay its first output past the bytes read with the entry header, for
/// example with empty stored blocks; delta sizes then come from streaming the rest of the entry.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn delta_size_prefix_beyond_header_read_ahead(#[case] format: ObjectFormat) {
    let program = [1, 2, 2, b'a', b'b'];
    let mut stream = vec![0x78, 0x01];
    for _ in 0..1000 {
        stream.extend_from_slice(&[0, 0, 0, 0xff, 0xff]);
    }
    stream.push(1);
    stream.extend_from_slice(&(program.len() as u16).to_le_bytes());
    stream.extend_from_slice(&(!(program.len() as u16)).to_le_bytes());
    stream.extend_from_slice(&program);
    let (mut a, mut b) = (1u32, 0u32);
    for byte in program {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    stream.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let base = ObjectId::for_blob(format, b"a");
    let id = ObjectId::for_blob(format, b"ab");
    let mut delta = vec![0x70 | program.len() as u8];
    delta.extend_from_slice(base.as_bytes());
    delta.extend_from_slice(&stream);
    assert!(delta.len() > 4096 + 64);
    let entries = [(base, entry(3, 1, b"", b"a")), (id, delta)];
    let (_root, pack) = open_file(format, &entries, None);
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let position = pack.find(id).unwrap().unwrap();
    let limits = ReadLimits::default();
    assert_eq!(
        pack.object_header(position, limits).unwrap(),
        (ObjectKind::Blob, 2)
    );
    assert_eq!(
        pack.locate(position, limits, &cancelled).unwrap(),
        (ObjectKind::Blob, 2)
    );
    assert_eq!(
        pack.read(position, limits, &cancelled).unwrap().data(),
        b"ab"
    );
}

/// A tiny entry declaring a 1 TiB payload must fail on its stream, not reserve the declared size.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn oversized_whole_entry_declaration_does_not_reserve_it(#[case] format: ObjectFormat) {
    let declared = 1usize << 40;
    let id = ObjectId::for_blob(format, b"x");
    let entries = [(id, entry(3, declared, b"", b"x"))];
    let (_root, pack) = open_file(format, &entries, None);
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let position = pack.find(id).unwrap().unwrap();
    assert!(matches!(
        pack.read(position, ReadLimits::trusted(), &cancelled),
        Err(Error::Corrupt(_))
    ));
}

/// A tiny delta declaring a 1 TiB result must fail on its program, not reserve the result.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn oversized_delta_result_declaration_does_not_reserve_it(#[case] format: ObjectFormat) {
    let base = ObjectId::for_blob(format, b"a");
    let id = ObjectId::for_blob(format, b"ab");
    // Base size 1, result size 2^40, then a one-byte insertion.
    let program = [1, 0x80, 0x80, 0x80, 0x80, 0x80, 0x20, 1, b'b'];
    let entries = [
        (base, entry(3, 1, b"", b"a")),
        (id, entry(7, program.len(), base.as_bytes(), &program)),
    ];
    let (_root, pack) = open_file(format, &entries, None);
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let position = pack.find(id).unwrap().unwrap();
    assert!(matches!(
        pack.read(position, ReadLimits::trusted(), &cancelled),
        Err(Error::Corrupt(_))
    ));
}

/// Writes `entries` as an index/pack pair, letting `edit` change the index, and opens it.
fn open_file_pack(
    format: ObjectFormat,
    entries: &[(ObjectId, Vec<u8>)],
    edit: impl FnOnce(&mut Vec<u8>),
) -> (tempfile::TempDir, super::FilePack) {
    let (mut index, data) = fixture(format, entries);
    edit(&mut index);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.idx");
    std::fs::write(&path, index).unwrap();
    std::fs::write(path.with_extension("pack"), data).unwrap();
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let pack = super::FilePack::open(format, &path, &mut crate::PackLimits::default(), &cancelled)
        .unwrap();
    (root, pack)
}

/// A damaged offset can make a second identity name an entry another identity already read and
/// cached. The cached payload proves only the identity it hashed to, so the second read fails
/// rather than returning the first object.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn cached_entry_does_not_prove_another_identity(#[case] format: ObjectFormat) {
    let tree = format.hash_object(ObjectKind::Tree, b"");
    let blob = ObjectId::for_blob(format, b"x");
    let entries = [
        (tree, entry(2, 0, b"", b"")),
        (blob, entry(3, 1, b"", b"x")),
    ];
    let (_root, pack) = open_file_pack(format, &entries, |index| {
        // Point the blob's offset at the tree's entry, at offset 12.
        let table = 1032 + 2 * (format.digest_len() + 4);
        let position = usize::from(blob > tree);
        index[table + 4 * position..table + 4 * position + 4].copy_from_slice(&12u32.to_be_bytes());
    });
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let read = |id| {
        pack.read(
            pack.find(id).unwrap().unwrap(),
            ReadLimits::default(),
            &cancelled,
        )
    };
    assert_eq!(read(tree).unwrap().kind, ObjectKind::Tree);
    assert!(matches!(read(blob), Err(Error::Corrupt(_))));
}

/// A base read through an offset delta has no index position, so its CRC is unchecked when it
/// is cached; a later read of that base by identity in the same store still checks its CRC.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn base_cached_by_offset_still_checks_crc_when_requested(#[case] format: ObjectFormat) {
    let base = format.hash_object(ObjectKind::Tree, b"a");
    let child = format.hash_object(ObjectKind::Tree, b"ab");
    let base_entry = entry(2, 1, b"", b"a");
    let distance = [u8::try_from(base_entry.len()).unwrap()];
    let entries = [
        (base, base_entry),
        (child, entry(6, 5, &distance, &[1, 2, 2, b'a', b'b'])),
    ];
    let (_root, pack) = open_file_pack(format, &entries, |index| {
        let crcs = 1032 + 2 * format.digest_len();
        let position = usize::from(base > child);
        index[crcs + 4 * position] ^= 1;
    });
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let read = |id| {
        pack.read(
            pack.find(id).unwrap().unwrap(),
            ReadLimits::default(),
            &cancelled,
        )
    };
    assert_eq!(read(child).unwrap().data(), b"ab");
    assert!(matches!(read(base), Err(Error::Corrupt("entry CRC"))));
}

/// An input budget that ends inside a reference delta's base identity is a limit, not damage:
/// the same store reads the entry once the budget allows.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn input_budget_inside_base_identity_is_a_limit(#[case] format: ObjectFormat) {
    let base = ObjectId::for_blob(format, b"a");
    let child = ObjectId::for_blob(format, b"ab");
    let entries = [
        (base, entry(3, 1, b"", b"a")),
        (child, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
    ];
    let (_root, pack) = open_file_pack(format, &entries, |_| {});
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let position = pack.find(child).unwrap().unwrap();
    // One header byte and half of the base identity.
    let limits = ReadLimits::default().with_max_input_bytes(1 + format.digest_len() / 2);
    assert!(matches!(
        pack.read_candidate(position, limits, &cancelled),
        Err(super::ReadFailure::Other(Error::Limit(
            "compressed entry bytes"
        )))
    ));
    let object = pack.read_candidate(position, ReadLimits::default(), &cancelled);
    assert_eq!(object.unwrap().data(), b"ab");
}

/// Content reads, an offset delta's included, need no offset order: they find each entry's end
/// from its zlib stream. Explicit verification builds the order. The index is mapped where the
/// platform maps files.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn content_reads_do_not_build_the_offset_order(#[case] format: ObjectFormat) {
    let base = ObjectId::for_blob(format, b"a");
    let child = ObjectId::for_blob(format, b"ab");
    let base_entry = entry(3, 1, b"", b"a");
    let distance = [u8::try_from(base_entry.len()).unwrap()];
    let entries = [
        (base, base_entry),
        (child, entry(6, 5, &distance, &[1, 2, 2, b'a', b'b'])),
    ];
    let (index, data) = fixture(format, &entries);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.idx");
    std::fs::write(&path, index).unwrap();
    std::fs::write(path.with_extension("pack"), data).unwrap();
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let pack = super::FilePack::open(format, &path, &mut crate::PackLimits::default(), &cancelled)
        .unwrap();
    let mapped = cfg!(any(target_os = "macos", target_os = "linux"));
    assert_eq!(pack.index_state(), (mapped, false));
    for (id, expected) in [(child, &b"ab"[..]), (base, b"a")] {
        let position = pack.find(id).unwrap().unwrap();
        let object = pack.read(position, ReadLimits::default(), &cancelled);
        assert_eq!(object.unwrap().data(), expected);
    }
    assert_eq!(pack.index_state(), (mapped, false));
    pack.verify(&cancelled).unwrap();
    assert_eq!(pack.index_state(), (mapped, true));
}

/// Bases and requested commits, trees and tags stay cached; a requested blob does not, so a
/// large payload cannot evict bases that later reads share.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn caches_bases_and_requested_non_blobs(#[case] format: ObjectFormat) {
    let base = ObjectId::for_blob(format, b"a");
    let child = ObjectId::for_blob(format, b"ab");
    let tree = format.hash_object(ObjectKind::Tree, b"");
    let entries = [
        (base, entry(3, 1, b"", b"a")),
        (child, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
        (tree, entry(2, 0, b"", b"")),
    ];
    let (index, data) = fixture(format, &entries);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.idx");
    std::fs::write(&path, index).unwrap();
    std::fs::write(path.with_extension("pack"), data).unwrap();
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let pack = super::FilePack::open(format, &path, &mut crate::PackLimits::default(), &cancelled)
        .unwrap();
    let read = |id| {
        pack.read(
            pack.find(id).unwrap().unwrap(),
            ReadLimits::default(),
            &cancelled,
        )
        .unwrap()
        .into_data()
    };
    let inflations = |id| {
        let offset = pack.offset(pack.find(id).unwrap().unwrap()).unwrap();
        super::counters::INFLATIONS.with(|map| map.borrow().get(&offset).copied().unwrap_or(0))
    };
    super::counters::reset();
    for _ in 0..2 {
        assert_eq!(read(child), b"ab");
        assert_eq!(read(base), b"a");
        assert_eq!(read(tree), b"");
    }
    assert_eq!(inflations(base), 1, "base is cached while reconstructing");
    assert_eq!(inflations(child), 2, "requested blob is not cached");
    assert_eq!(inflations(tree), 1, "requested tree is cached");
}

/// A payload cached by a read with loose limits is still held to a later read's object bound,
/// whether it is the requested object or a base.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn cached_payloads_respect_later_object_limits(#[case] format: ObjectFormat) {
    let tree = format.hash_object(ObjectKind::Tree, b"xyz");
    let base = ObjectId::for_blob(format, b"abc");
    let child = ObjectId::for_blob(format, b"ab");
    let entries = [
        (tree, entry(2, 3, b"", b"xyz")),
        (base, entry(3, 3, b"", b"abc")),
        (child, entry(7, 4, base.as_bytes(), &[3, 2, 0x90, 2])),
    ];
    let (index, data) = fixture(format, &entries);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.idx");
    std::fs::write(&path, index).unwrap();
    std::fs::write(path.with_extension("pack"), data).unwrap();
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let pack = super::FilePack::open(format, &path, &mut crate::PackLimits::default(), &cancelled)
        .unwrap();
    let read = |id, max_object_bytes| {
        let limits = ReadLimits {
            max_object_bytes,
            ..ReadLimits::default()
        };
        pack.read(pack.find(id).unwrap().unwrap(), limits, &cancelled)
    };
    assert_eq!(read(tree, 3).unwrap().data(), b"xyz");
    assert_eq!(read(child, 3).unwrap().data(), b"ab");
    assert!(matches!(read(tree, 2), Err(Error::Limit("object bytes"))));
    assert!(matches!(read(child, 2), Err(Error::Limit("object bytes"))));
    assert_eq!(read(tree, 3).unwrap().data(), b"xyz");
}
