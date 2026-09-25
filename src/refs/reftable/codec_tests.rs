use rstest::rstest;

use super::{Error, Limits, LogRecord, LogValue, RefRecord, Table};
use crate::refs::{RefName, Target};
use crate::{ObjectFormat, ObjectId};

fn table(format: ObjectFormat) -> Table {
    let id = ObjectId::from_bytes(format, &vec![1; format.digest_len()]).unwrap();
    Table {
        format,
        min_update_index: 1,
        max_update_index: 2,
        references: vec![
            RefRecord {
                name: name(b"HEAD").into(),
                update_index: 1,
                target: Some(Target::Symbolic(name(b"refs/heads/main"))),
                peeled: None,
            },
            RefRecord {
                name: name(b"refs/deleted").into(),
                update_index: 2,
                target: None,
                peeled: None,
            },
            RefRecord {
                name: name(b"refs/heads/main").into(),
                update_index: 2,
                target: Some(Target::Direct(id)),
                peeled: Some(id),
            },
        ],
        logs: vec![
            LogRecord {
                name: name(b"HEAD").into(),
                update_index: 2,
                value: Some(LogValue {
                    old: id,
                    new: id,
                    name: b"\xff name ".to_vec(),
                    email: b"a@b".to_vec(),
                    seconds: u64::MAX,
                    offset_minutes: i16::MIN,
                    message: b"raw\0\r\n bytes".to_vec(),
                }),
            },
            LogRecord {
                name: name(b"HEAD").into(),
                update_index: 1,
                value: None,
            },
        ],
    }
}
fn name(bytes: &[u8]) -> RefName {
    RefName::new(bytes).unwrap()
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn retains_binary_records_and_tombstones(#[case] format: ObjectFormat) {
    let table = table(format);
    let encoded = table.encode(Limits::default()).unwrap();
    assert_eq!(Table::decode(&encoded, Limits::default()).unwrap(), table);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn roundtrips_empty_table(#[case] format: ObjectFormat) {
    let mut table = table(format);
    table.references.clear();
    table.logs.clear();
    let encoded = table.encode(Limits::default()).unwrap();
    assert_eq!(Table::decode(&encoded, Limits::default()).unwrap(), table);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn roundtrips_log_only_table(#[case] format: ObjectFormat) {
    let mut table = table(format);
    table.references.clear();
    let encoded = table.encode(Limits::default()).unwrap();
    assert_eq!(Table::decode(&encoded, Limits::default()).unwrap(), table);
}

#[rstest]
#[case::header(0)]
#[case::footer(200)]
fn rejects_changed_checksum_or_magic(#[case] offset: usize) {
    let mut bytes = table(ObjectFormat::Sha1).encode(Limits::default()).unwrap();
    bytes[offset] ^= 1;
    assert!(Table::decode(&bytes, Limits::default()).is_err());
}

#[test]
fn rejects_every_truncated_prefix() {
    let bytes = table(ObjectFormat::Sha256)
        .encode(Limits::default())
        .unwrap();
    assert_all_prefixes_fail(&bytes);
}
fn assert_all_prefixes_fail(bytes: &[u8]) {
    for length in 0..bytes.len() {
        assert!(
            Table::decode(&bytes[..length], Limits::default()).is_err(),
            "prefix {length}"
        );
    }
}

#[rstest]
#[case::bytes(Limits { bytes: 0, ..Limits::default() })]
#[case::block(Limits { block_bytes: 1, ..Limits::default() })]
#[case::records(Limits { records: 1, ..Limits::default() })]
#[case::strings(Limits { string_bytes: 1, ..Limits::default() })]
#[case::decoded(Limits { decoded_bytes: 1, ..Limits::default() })]
fn refuses_resource_limits(#[case] limits: Limits) {
    let table = table(ObjectFormat::Sha1);
    let bytes = table.encode(Limits::default()).unwrap();
    assert!(Table::decode(&bytes, limits).is_err());
    assert!(table.encode(limits).is_err());
}

#[test]
fn rejects_duplicate_reference_keys() {
    let mut table = table(ObjectFormat::Sha1);
    table.references.insert(1, table.references[0].clone());
    assert!(matches!(
        table.encode(Limits::default()),
        Err(Error::Malformed(_))
    ));
}

#[test]
fn rejects_mixed_hash_formats() {
    let mut table = table(ObjectFormat::Sha1);
    table.references[2].target = Some(Target::Direct(ObjectId::Sha256([1; 32])));
    assert!(matches!(
        table.encode(Limits::default()),
        Err(Error::Malformed(_))
    ));
}

#[rstest]
#[case::prefix(28, 1)]
#[case::reserved_value(29, 36)]
#[case::update_range(34, 127)]
#[case::block_length(27, 1)]
fn rejects_corrupt_reference_framing(#[case] offset: usize, #[case] value: u8) {
    let mut bytes = table(ObjectFormat::Sha1).encode(Limits::default()).unwrap();
    bytes[offset] = value;
    assert!(Table::decode(&bytes, Limits::default()).is_err());
}

#[test]
fn rejects_corrupt_restart_pointer() {
    let mut bytes = table(ObjectFormat::Sha1).encode(Limits::default()).unwrap();
    let block_len = u32::from_be_bytes([0, bytes[25], bytes[26], bytes[27]]) as usize;
    let count = u16::from_be_bytes(bytes[block_len - 2..block_len].try_into().unwrap()) as usize;
    bytes[block_len - 2 - count * 3 + 2] += 1;
    assert!(Table::decode(&bytes, Limits::default()).is_err());
}

#[test]
fn rejects_corrupt_compressed_log() {
    let mut bytes = table(ObjectFormat::Sha1).encode(Limits::default()).unwrap();
    let footer = bytes.len() - 68;
    let log = u64::from_be_bytes(bytes[footer + 48..footer + 56].try_into().unwrap()) as usize;
    bytes[log + 4] = 0;
    assert!(Table::decode(&bytes, Limits::default()).is_err());
}
