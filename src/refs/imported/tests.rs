use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use rstest::rstest;

use super::*;
use crate::{ObjectFormat, ObjectId};

fn bytes(format: ObjectFormat, tail: &[u8]) -> Vec<u8> {
    [
        format!("{} {} ", ObjectId::null(format), id(format)).as_bytes(),
        tail,
    ]
    .concat()
}
fn id(format: ObjectFormat) -> ObjectId {
    ObjectId::from_hex(format, &"12".repeat(format.digest_len())).unwrap()
}
fn read(format: ObjectFormat, data: &[u8], limits: ReflogLimits) -> ImportedReflog {
    ImportedReflog::read(format, data, limits, &AtomicBool::new(false))
}

#[rstest]
#[case::cr(b"A <a@b> 1 +0000\ta\rb\n", b"a\rb", 1)]
#[case::nul(b"A <a@b> 1 +0000\ta\0b\n", b"a\0b", 1)]
#[case::suffix(b"A <a@b> 1 +0000suffix\tfixture\n", b"suffix\tfixture", 1)]
#[case::unsigned(b"A <a@b> 18446744073709551615 +0000\tfixture\n", b"fixture", u64::MAX as i128)]
#[case::negative(b"A <a@b> -1 +0000\tfixture\n", b"fixture", -1)]
fn retains_exact_imports(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] tail: &[u8],
    #[case] message: &[u8],
    #[case] seconds: i128,
) {
    let data = bytes(format, tail);
    let log = read(format, &data, ReflogLimits::default());
    assert!(log.is_complete());
    assert_eq!(
        log.records(),
        &[ImportedRecord::File {
            format,
            bytes: data
        }]
    );
    let fields = log.records()[0].fields().unwrap();
    assert_eq!(fields.seconds, seconds);
    assert_eq!(fields.message, message);
    assert_eq!(log.recoverable_roots().collect::<Vec<_>>(), [id(format)]);
}

#[rstest]
#[case::short(b"A <a@b> 1 +01\tx\n")]
#[case::overflow(b"A <a@b> 9999999999999999999999999999999999999999 +0000\tx\n")]
#[case::unterminated(b"A <a@b> 1 +0000\tx")]
#[case::identity(b"broken\n")]
fn corrupt_records_keep_roots(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] tail: &[u8],
) {
    let data = bytes(format, tail);
    let log = read(format, &data, ReflogLimits::default());
    assert!(!log.is_complete());
    assert!(matches!(log.end(), ReflogReadEnd::Eof));
    assert_eq!(log.recoverable_roots().collect::<Vec<_>>(), [id(format)]);
    assert_eq!(
        log.records(),
        &[ImportedRecord::File {
            format,
            bytes: data
        }]
    );
}

#[rstest]
#[case::input(ReflogLimits { bytes: 90, ..ReflogLimits::default() })]
#[case::retained(ReflogLimits { retained_bytes: 90, ..ReflogLimits::default() })]
#[case::record(ReflogLimits { record_bytes: 90, ..ReflogLimits::default() })]
fn limits_retain_a_prefix_and_its_roots(#[case] limits: ReflogLimits) {
    let data = bytes(ObjectFormat::Sha1, b"A <a@b> 1 +0000\tmessage\n");
    let log = read(ObjectFormat::Sha1, &data, limits);
    assert!(!log.is_complete());
    assert!(matches!(log.end(), ReflogReadEnd::Limit(_)));
    assert_eq!(
        log.records(),
        &[ImportedRecord::File {
            format: ObjectFormat::Sha1,
            bytes: data[..90].to_vec()
        }]
    );
    assert_eq!(
        log.recoverable_roots().collect::<Vec<_>>(),
        [id(ObjectFormat::Sha1)]
    );
}

#[test]
fn recovers_second_id_when_first_is_corrupt_and_continues() {
    let mut first = bytes(ObjectFormat::Sha1, b"bad\n");
    first[0] = b'z';
    let second = bytes(ObjectFormat::Sha1, b"A <a@b> 1 +0000\tm\n");
    let log = read(
        ObjectFormat::Sha1,
        &[first, second].concat(),
        ReflogLimits::default(),
    );
    assert!(!log.is_complete());
    assert_eq!(log.records().len(), 2);
    assert_eq!(
        log.recoverable_roots().collect::<Vec<_>>(),
        [id(ObjectFormat::Sha1); 2]
    );
}

#[test]
fn record_count_and_exact_byte_boundary_are_explicit() {
    let data = bytes(ObjectFormat::Sha1, b"A <a@b> 1 +0000\tm\n");
    let log = read(
        ObjectFormat::Sha1,
        &data.repeat(2),
        ReflogLimits {
            records: 1,
            ..ReflogLimits::default()
        },
    );
    assert_eq!(log.records().len(), 1);
    assert!(matches!(log.end(), ReflogReadEnd::Limit("records")));
    let exact = read(
        ObjectFormat::Sha1,
        &data,
        ReflogLimits {
            bytes: data.len(),
            ..ReflogLimits::default()
        },
    );
    assert!(!exact.is_complete());
    assert_eq!(exact.records().len(), 1);
}

struct FaultReader {
    prefix: io::Cursor<Vec<u8>>,
}
impl Read for FaultReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.prefix.read(output)?;
        if count == 0 {
            return Err(io::Error::other("original injected read fault"));
        }
        Ok(count)
    }
}
#[test]
fn io_failure_preserves_complete_and_partial_records() {
    let complete = bytes(ObjectFormat::Sha1, b"A <a@b> 1 +0000\tm\n");
    let partial = bytes(ObjectFormat::Sha1, b"A <a@b>");
    let data = [complete, partial.clone()].concat();
    let reader = FaultReader {
        prefix: io::Cursor::new(data),
    };
    let log = ImportedReflog::read(
        ObjectFormat::Sha1,
        reader,
        ReflogLimits::default(),
        &AtomicBool::new(false),
    );
    assert!(matches!(log.end(), ReflogReadEnd::Io(_)));
    assert!(!log.is_complete());
    assert_eq!(
        log.records()[1],
        ImportedRecord::File {
            format: ObjectFormat::Sha1,
            bytes: partial
        }
    );
    assert_eq!(log.recoverable_roots().count(), 2);
}

struct CancellingReader<'a> {
    cancel: &'a AtomicBool,
    reads: usize,
}
impl Read for CancellingReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.reads += 1;
        if self.reads == 2 {
            self.cancel.store(true, Ordering::Relaxed);
        }
        output.fill(b'x');
        Ok(output.len())
    }
}
#[test]
fn cancellation_during_large_record_keeps_bounded_prefix() {
    let cancel = AtomicBool::new(false);
    let reader = CancellingReader {
        cancel: &cancel,
        reads: 0,
    };
    let log = ImportedReflog::read(ObjectFormat::Sha1, reader, ReflogLimits::default(), &cancel);
    assert!(matches!(log.end(), ReflogReadEnd::Cancelled));
    assert_eq!(
        log.records(),
        &[ImportedRecord::File {
            format: ObjectFormat::Sha1,
            bytes: vec![b'x'; 8192]
        }]
    );
}

#[test]
fn empty_and_zero_budget_scans_are_distinct() {
    assert!(read(ObjectFormat::Sha1, b"", ReflogLimits::default()).is_complete());
    let log = read(
        ObjectFormat::Sha1,
        b"",
        ReflogLimits {
            bytes: 0,
            ..ReflogLimits::default()
        },
    );
    assert!(matches!(log.end(), ReflogReadEnd::Limit(_)));
    assert!(!log.is_complete());
}
