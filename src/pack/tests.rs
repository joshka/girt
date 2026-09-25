//! Original binary fixtures built from the documented format, never Git source or test code.
use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use rstest::rstest;
use sha1::{Digest, Sha1};

use super::delta;
use super::reader::Pack;
use crate::{ObjectId, ObjectKind, ObjectReadError as Error, ReadLimits};

fn seal(bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(&Sha1::digest(&*bytes));
}

fn reseal(bytes: &mut Vec<u8>) {
    bytes.truncate(bytes.len() - 20);
    seal(bytes);
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

fn fixture(entries: &[(ObjectId, Vec<u8>)]) -> (Vec<u8>, Vec<u8>) {
    let mut pack = b"PACK\0\0\0\x02".to_vec();
    pack.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    let mut records = Vec::new();
    for (id, data) in entries {
        records.push((*id, pack.len() as u32, crc32fast::hash(data)));
        pack.extend_from_slice(data);
    }
    seal(&mut pack);
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
    index.extend_from_slice(&pack[pack.len() - 20..]);
    seal(&mut index);
    (index, pack)
}

fn ordinary() -> (Vec<u8>, Vec<u8>) {
    fixture(&[(
        ObjectId::for_blob(crate::ObjectFormat::Sha1, b"hello"),
        entry(3, 5, b"", b"hello"),
    )])
}

fn open_read(
    entries: &[(ObjectId, Vec<u8>)],
    id: ObjectId,
    limits: ReadLimits,
) -> Result<crate::Object, Error> {
    let (index, data) = fixture(entries);
    let pack = Pack::open(&index, data)?;
    pack.read(pack.find(id).unwrap(), limits)
}

#[rstest]
#[case::blob(3, ObjectKind::Blob)]
#[case::tree(2, ObjectKind::Tree)]
#[case::commit(1, ObjectKind::Commit)]
#[case::tag(4, ObjectKind::Tag)]
fn returns_exact_kind_and_payload(#[case] code: u8, #[case] kind: ObjectKind) {
    let id = ObjectId::for_object(kind.as_str(), b"raw payload");
    let object = open_read(
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
#[case::ofs(6, vec![entry(3, 5, b"", b"hello").len() as u8])]
#[case::reference(7, ObjectId::for_blob(crate::ObjectFormat::Sha1, b"hello").as_bytes().to_vec())]
fn reconstructs_delta(#[case] code: u8, #[case] reference: Vec<u8>) {
    let base_id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"hello");
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"hello!");
    let base = entry(3, 5, b"", b"hello");
    let program = [5, 6, 0x90, 5, 1, b'!'];
    let object = open_read(
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

#[test]
fn resolves_forward_reference_and_nested_deltas() {
    let base = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"a");
    let middle = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"ab");
    let tip = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"abc");
    let entries = [
        (
            tip,
            entry(7, 6, middle.as_bytes(), &[2, 3, 3, b'a', b'b', b'c']),
        ),
        (middle, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
        (base, entry(3, 1, b"", b"a")),
    ];
    let object = open_read(&entries, tip, ReadLimits::default()).unwrap();
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
#[case::short_header(3)]
#[case::short_version(7)]
#[case::short_fanout(1031)]
#[case::short_ids(1040)]
#[case::short_trailer(1099)]
fn rejects_truncated_indexes(#[case] length: usize) {
    let (mut index, data) = ordinary();
    index.truncate(length);
    assert!(Pack::open(&index, data).is_err());
}

#[rstest]
#[case::fanout(8, 1, "index fanout")]
#[case::offset_header(1056, 11, "offset outside pack entries")]
#[case::offset_past_pack(1056, 9999, "offset outside pack entries")]
#[case::offset_gap(1056, 13, "unindexed pack bytes")]
#[case::large_slot(1056, 0x8000_0000, "large offset index")]
#[case::crc(1052, 0, "entry CRC")]
fn rejects_invalid_index_fields(#[case] offset: usize, #[case] value: u32, #[case] reason: &str) {
    let (mut index, data) = ordinary();
    index[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    reseal(&mut index);
    let error = Pack::open(&index, data).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("corrupt packed storage: {reason}")
    );
}

#[test]
fn supports_index_large_offset_table() {
    let (mut index, data) = ordinary();
    index[1056..1060].copy_from_slice(&0x8000_0000u32.to_be_bytes());
    index.splice(1060..1060, 12u64.to_be_bytes());
    reseal(&mut index);
    let pack = Pack::open(&index, data).unwrap();
    assert_eq!(
        pack.read(0, ReadLimits::default()).unwrap().data(),
        b"hello"
    );
}

#[test]
fn rejects_index_checksum() {
    let (mut index, data) = ordinary();
    index[1040] ^= 1;
    assert!(matches!(
        Pack::open(&index, data),
        Err(Error::Corrupt("index checksum"))
    ));
}

#[test]
fn rejects_pack_checksum() {
    let (index, mut data) = ordinary();
    data[14] ^= 1;
    assert!(matches!(
        Pack::open(&index, data),
        Err(Error::Corrupt("pack checksum"))
    ));
}

#[test]
fn rejects_index_pack_disagreement() {
    let (mut index, data) = ordinary();
    index[1060] ^= 1;
    reseal(&mut index);
    assert!(matches!(
        Pack::open(&index, data),
        Err(Error::Corrupt("index/pack checksum disagreement"))
    ));
}

#[rstest]
#[case::legacy(1)]
#[case::future(3)]
fn rejects_unsupported_index_versions(#[case] version: u32) {
    let (mut index, data) = ordinary();
    index[4..8].copy_from_slice(&version.to_be_bytes());
    assert!(
        matches!(Pack::open(&index, data), Err(Error::IndexVersion(found)) if found == version)
    );
}

#[rstest]
#[case::legacy(1)]
#[case::recognized_but_unsupported(3)]
fn rejects_unsupported_pack_versions(#[case] version: u32) {
    let (index, mut data) = ordinary();
    data[4..8].copy_from_slice(&version.to_be_bytes());
    assert!(matches!(Pack::open(&index, data), Err(Error::PackVersion(found)) if found == version));
}

#[test]
fn rejects_headerless_index_v1() {
    let (_, data) = ordinary();
    assert!(matches!(
        Pack::open(&[0; 1088], data),
        Err(Error::IndexVersion(1))
    ));
}

#[rstest]
#[case::zero_distance(&[0], "zero delta distance")]
#[case::before_pack(&[127], "delta offset before pack")]
#[case::inside_entry(&[1], "base offset is not an indexed entry")]
#[case::overflow(&[255; 20], "delta offset overflow")]
fn rejects_bad_ofs_bases(#[case] base: &[u8], #[case] reason: &str) {
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"x");
    let error = open_read(
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

#[test]
fn rejects_missing_ref_base() {
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"x");
    let base = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"absent");
    let error = open_read(
        &[(id, entry(7, 3, base.as_bytes(), &[1, 1, 0x80]))],
        id,
        ReadLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(error, Error::MissingBase(found) if found == base));
}

#[test]
fn detects_ref_cycles_without_recursion() {
    let first = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"first");
    let second = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"second");
    let entries = [
        (first, entry(7, 0, second.as_bytes(), b"")),
        (second, entry(7, 0, first.as_bytes(), b"")),
    ];
    assert!(matches!(
        open_read(&entries, first, ReadLimits::default()),
        Err(Error::DeltaCycle)
    ));
}

#[rstest]
#[case::wrong_length(3, 4, "inflated entry exceeds declared size")]
#[case::too_long(3, 6, "packed entry length or trailing data")]
#[case::wrong_identity(3, 5, "object identity")]
fn rejects_bad_entry_payloads(#[case] kind: u8, #[case] size: usize, #[case] reason: &str) {
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"other");
    let error = open_read(
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
#[case::invalid(0)]
#[case::reserved(5)]
fn rejects_reserved_kinds(#[case] kind: u8) {
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"");
    assert!(
        matches!(open_read(&[(id, entry(kind, 0, b"", b""))], id, ReadLimits::default()), Err(Error::ObjectType(found)) if found == kind)
    );
}

#[rstest]
#[case::payload(ReadLimits { max_object_bytes: 0, ..ReadLimits::default() }, "object bytes")]
#[case::program(ReadLimits { max_delta_bytes: 0, ..ReadLimits::default() }, "delta program bytes")]
#[case::depth(ReadLimits { max_delta_depth: 0, ..ReadLimits::default() }, "delta depth")]
#[case::cumulative(ReadLimits { max_decode_bytes: 5, ..ReadLimits::default() }, "cumulative decode bytes")]
fn enforces_read_limits(#[case] limits: ReadLimits, #[case] reason: &str) {
    let base = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"a");
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"ab");
    let entries = [
        (base, entry(3, 1, b"", b"a")),
        (id, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
    ];
    let error = open_read(&entries, id, limits).unwrap_err();
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

#[test]
fn validates_intermediate_identity() {
    let base = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"wrong");
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"ab");
    let entries = [
        (base, entry(3, 1, b"", b"a")),
        (id, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
    ];
    assert!(matches!(
        open_read(&entries, id, ReadLimits::default()),
        Err(Error::Corrupt("object identity"))
    ));
}

#[rstest]
#[case::zero(0)]
#[case::header(11)]
#[case::trailer(31)]
#[case::entry(35)]
fn rejects_truncated_packs(#[case] length: usize) {
    let (index, mut data) = ordinary();
    data.truncate(length);
    assert!(Pack::open(&index, data).is_err());
}

#[test]
fn accepts_empty_pack_and_reports_index_miss() {
    let (index, data) = fixture(&[]);
    let pack = Pack::open(&index, data).unwrap();
    assert_eq!(
        pack.find(ObjectId::for_blob(crate::ObjectFormat::Sha1, b"")),
        None
    );
}

#[test]
fn rejects_pack_count_mismatch() {
    let (index, mut data) = ordinary();
    data[8..12].copy_from_slice(&2u32.to_be_bytes());
    assert!(matches!(
        Pack::open(&index, data),
        Err(Error::Corrupt("pack object count"))
    ));
}

#[test]
fn rejects_duplicate_index_ids() {
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"a");
    let (index, data) = fixture(&[(id, entry(3, 1, b"", b"a")), (id, entry(3, 1, b"", b"a"))]);
    assert!(matches!(
        Pack::open(&index, data),
        Err(Error::Corrupt("unsorted or duplicate index identities"))
    ));
}

#[test]
fn rejects_unsorted_index_ids() {
    let (mut index, data) = fixture(&[
        (
            ObjectId::for_blob(crate::ObjectFormat::Sha1, b"a"),
            entry(3, 1, b"", b"a"),
        ),
        (
            ObjectId::for_blob(crate::ObjectFormat::Sha1, b"b"),
            entry(3, 1, b"", b"b"),
        ),
    ]);
    index[1032..1072].rotate_left(20);
    reseal(&mut index);
    assert!(matches!(
        Pack::open(&index, data),
        Err(Error::Corrupt("unsorted or duplicate index identities"))
    ));
}

#[test]
fn rejects_duplicate_pack_offsets() {
    let (mut index, data) = fixture(&[
        (
            ObjectId::for_blob(crate::ObjectFormat::Sha1, b"a"),
            entry(3, 1, b"", b"a"),
        ),
        (
            ObjectId::for_blob(crate::ObjectFormat::Sha1, b"b"),
            entry(3, 1, b"", b"b"),
        ),
    ]);
    let offset: [u8; 4] = index[1080..1084].try_into().unwrap();
    index[1084..1088].copy_from_slice(&offset);
    reseal(&mut index);
    assert!(matches!(
        Pack::open(&index, data),
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
#[case::truncated(truncated_zlib, "truncated packed zlib stream")]
#[case::trailing(trailing_zlib, "packed entry length or trailing data")]
#[case::checksum(corrupt_zlib, "invalid packed zlib stream")]
fn validates_zlib_even_with_valid_pack_checksums(
    #[case] data: fn() -> Vec<u8>,
    #[case] reason: &str,
) {
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"hello");
    let error = open_read(&[(id, data())], id, ReadLimits::default()).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("corrupt packed storage: {reason}")
    );
}

#[rstest]
#[case::truncated_size(vec![0xb0], "truncated delta or entry")]
#[case::overflowing_size(vec![0xff; 20], "size overflow")]
#[case::truncated_reference(vec![0x70, 1, 2], "truncated base identity")]
#[case::truncated_offset(vec![0x60, 0x80], "truncated delta or entry")]
fn rejects_truncated_entry_metadata(#[case] data: Vec<u8>, #[case] reason: &str) {
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"hello");
    let error = open_read(&[(id, data)], id, ReadLimits::default()).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("corrupt packed storage: {reason}")
    );
}

#[test]
fn permits_exact_depth_and_decode_budget() {
    let base = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"a");
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"ab");
    let entries = [
        (base, entry(3, 1, b"", b"a")),
        (id, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
    ];
    let limits = ReadLimits {
        max_object_bytes: 2,
        max_delta_bytes: 5,
        max_decode_bytes: 8,
        max_delta_depth: 1,
    };
    assert_eq!(open_read(&entries, id, limits).unwrap().data(), b"ab");
}

#[test]
fn rejects_external_base_even_when_it_exists_in_another_pack() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("pack");
    std::fs::create_dir(&directory).unwrap();
    let base = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"a");
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"ab");
    let (base_index, base_data) = fixture(&[(base, entry(3, 1, b"", b"a"))]);
    let (index, data) = fixture(&[(id, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b']))]);
    std::fs::write(directory.join("base.idx"), base_index).unwrap();
    std::fs::write(directory.join("base.pack"), base_data).unwrap();
    std::fs::write(directory.join("delta.idx"), index).unwrap();
    std::fs::write(directory.join("delta.pack"), data).unwrap();
    let objects = crate::Objects::open(
        crate::ObjectFormat::Sha1,
        root.path(),
        crate::PackLimits::default(),
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
#[case::tree(2, ObjectKind::Tree)]
#[case::commit(1, ObjectKind::Commit)]
#[case::tag(4, ObjectKind::Tag)]
fn delta_inherits_base_kind(#[case] code: u8, #[case] kind: ObjectKind) {
    let base = ObjectId::for_object(kind.as_str(), b"a");
    let id = ObjectId::for_object(kind.as_str(), b"ab");
    let entries = [
        (base, entry(code, 1, b"", b"a")),
        (id, entry(7, 5, base.as_bytes(), &[1, 2, 2, b'a', b'b'])),
    ];
    let object = open_read(&entries, id, ReadLimits::default()).unwrap();
    assert_eq!(object.kind(), kind);
    assert_eq!(object.data(), b"ab");
}

#[test]
fn bounds_long_chains_without_recursive_calls() {
    let ids: Vec<_> = (0u64..1025)
        .map(|number| ObjectId::for_blob(crate::ObjectFormat::Sha1, &number.to_be_bytes()))
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
        open_read(&entries, ids[0], limits),
        Err(Error::Limit("delta depth"))
    ));
}
