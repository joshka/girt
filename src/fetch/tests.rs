use std::io::{self, Read, Write};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

use rstest::rstest;

use super::*;
use crate::{ObjectId, ObjectKind, PackObject, PackWriteLimits, write_pack};

fn pkt(data: &[u8]) -> Vec<u8> {
    let mut bytes = format!("{:04x}", data.len() + 4).into_bytes();
    bytes.extend_from_slice(data);
    bytes
}
fn advertised(id: ObjectId, caps: &str) -> Vec<u8> {
    let mut bytes = pkt(format!("{id} refs/heads/main\0{caps}\n").as_bytes());
    bytes.extend_from_slice(b"0000");
    bytes
}
fn blob_pack() -> (ObjectId, Vec<u8>) {
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"fetch fixture");
    let mut pack = Vec::new();
    let written = write_pack(
        &[PackObject {
            id,
            kind: ObjectKind::Blob,
            data: b"fetch fixture",
        }],
        &mut pack,
        &mut Vec::new(),
        PackWriteLimits::default(),
    );
    written.unwrap();
    (id, pack)
}
fn response(id: ObjectId, pack: &[u8]) -> Vec<u8> {
    let mut bytes = advertised(id, "side-band-64k ofs-delta");
    bytes.extend(pkt(b"NAK\n"));
    bytes.extend(pkt(b"\x02counting\r"));
    let mut band = vec![1];
    band.extend(pack);
    bytes.extend(pkt(&band));
    bytes.extend(b"0000");
    bytes
}
fn select(advertisement: &Advertisement) -> Vec<ObjectId> {
    advertisement
        .refs
        .iter()
        .filter(|r| !r.peeled)
        .map(|r| r.id)
        .collect()
}
fn run(bytes: &[u8], limits: FetchLimits) -> Result<ReceivedFetch, FetchError> {
    receive(
        &mut &bytes[..],
        &mut Vec::new(),
        select,
        limits,
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    )
}

#[test]
fn negotiates_only_supported_capabilities_and_deduplicates_wants() {
    let (id, pack) = blob_pack();
    let bytes = response(id, &pack);
    let mut sent = Vec::new();
    let mut progress = Vec::new();
    let result = receive(
        &mut bytes.as_slice(),
        &mut sent,
        |_| vec![id, id],
        FetchLimits::default(),
        &AtomicBool::new(false),
        |bytes| {
            progress.extend_from_slice(bytes);
            ControlFlow::Continue(())
        },
    );
    let result = result.unwrap();
    let mut expected = pkt(format!("want {id} side-band-64k ofs-delta\n").as_bytes());
    expected.extend(b"0000");
    expected.extend(pkt(b"done\n"));
    assert_eq!(sent, expected);
    assert_eq!(progress, b"counting\r");
    assert_eq!(result.wants(), &[id]);
    assert_eq!(result.object_count(), 1);
}

#[rstest]
#[case::short_header(b"00")]
#[case::bad_hex(b"zzzz")]
#[case::v2_delimiter(b"0001")]
#[case::response_end(b"0002")]
#[case::too_large(b"ffff")]
#[case::empty_data(b"0004")]
#[case::short_payload(b"0008xx")]
fn rejects_bad_framing(#[case] bytes: &[u8]) {
    assert!(matches!(
        run(bytes, FetchLimits::default()),
        Err(FetchError::Protocol(_))
    ));
}

#[rstest]
#[case::version(b"version 2\n", true)]
#[case::shallow(b"shallow abc\n", true)]
#[case::missing_caps(b"1111111111111111111111111111111111111111 refs/heads/main", false)]
#[case::sha256(
    b"1111111111111111111111111111111111111111 refs/heads/main\0object-format=sha256",
    true
)]
#[case::zero(
    b"0000000000000000000000000000000000000000 refs/heads/main\0side-band-64k",
    false
)]
#[case::bad_name(
    b"1111111111111111111111111111111111111111 refs/../main\0side-band-64k",
    false
)]
#[case::unmatched_peeled(
    b"1111111111111111111111111111111111111111 refs/tags/x^{}\0side-band-64k",
    false
)]
fn rejects_advertisements(#[case] data: &[u8], #[case] unsupported: bool) {
    let error = run(&pkt(data), FetchLimits::default()).unwrap_err();
    assert_eq!(matches!(error, FetchError::Unsupported(_)), unsupported);
}

#[test]
fn empty_server_and_selection_complete_without_pack() {
    let mut bytes =
        pkt(b"0000000000000000000000000000000000000000 capabilities^{}\0side-band-64k\n");
    bytes.extend(b"0000");
    let result = run(&bytes, FetchLimits::default()).unwrap();
    assert_eq!(result.object_count(), 0);
    assert_eq!(result.pack_bytes(), 0);
}

#[test]
fn accepts_empty_flush_advertisement() {
    assert_eq!(
        run(b"0000", FetchLimits::default()).unwrap().object_count(),
        0
    );
}

#[test]
fn ignores_unrequested_caps_and_accepts_missing_lf() {
    let (id, pack) = blob_pack();
    let mut bytes = advertised(
        id,
        "side-band-64k thin-pack filter shallow multi_ack agent=test",
    );
    bytes.extend(pkt(b"NAK"));
    bytes.extend(pkt(&[&[1][..], &pack].concat()));
    bytes.extend(b"0000");
    let mut sent = Vec::new();
    let result = receive(
        &mut bytes.as_slice(),
        &mut sent,
        select,
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    result.unwrap();
    assert_eq!(
        sent,
        [
            pkt(format!("want {id} side-band-64k\n").as_bytes()),
            b"0000".to_vec(),
            pkt(b"done\n")
        ]
        .concat()
    );
}

#[rstest]
#[case::ack(b"ACK 1111111111111111111111111111111111111111\n")]
#[case::flush(b"")]
fn rejects_unexpected_negotiation(#[case] reply: &[u8]) {
    let mut bytes = advertised(
        ObjectId::for_blob(crate::ObjectFormat::Sha1, b"x"),
        "side-band-64k",
    );
    bytes.extend(pkt(reply));
    assert!(matches!(
        run(&bytes, FetchLimits::default()),
        Err(FetchError::Protocol(_))
    ));
}

#[rstest]
#[case::early(b"ERR denied\n", false)]
#[case::fatal_band(b"\x03failed\n", true)]
fn preserves_remote_errors(#[case] error: &[u8], #[case] after_nak: bool) {
    let bytes = remote_error(error, after_nak);
    assert!(matches!(
        run(&bytes, FetchLimits::default()),
        Err(FetchError::Remote(_))
    ));
}
fn remote_error(error: &[u8], after_nak: bool) -> Vec<u8> {
    let mut bytes = advertised(
        ObjectId::for_blob(crate::ObjectFormat::Sha1, b"x"),
        "side-band-64k",
    );
    if after_nak {
        bytes.extend(pkt(b"NAK"));
    }
    bytes.extend(pkt(error));
    bytes
}

#[rstest]
#[case::missing_flush(4, b"")]
#[case::truncated_pack(5, b"")]
#[case::trailing(0, b"x")]
fn requires_complete_response(#[case] remove: usize, #[case] append: &[u8]) {
    let (id, pack) = blob_pack();
    let mut bytes = response(id, &pack);
    bytes.truncate(bytes.len() - remove);
    bytes.extend(append);
    assert!(matches!(
        run(&bytes, FetchLimits::default()),
        Err(FetchError::Protocol(_))
    ));
}

#[test]
fn rejects_invalid_sideband() {
    let bytes = remote_error(b"\x04unexpected", true);
    assert!(matches!(
        run(&bytes, FetchLimits::default()),
        Err(FetchError::Protocol(_))
    ));
}

#[test]
fn refuses_unadvertised_want_before_writing() {
    let bytes = advertised(
        ObjectId::for_blob(crate::ObjectFormat::Sha1, b"x"),
        "side-band-64k",
    );
    let mut sent = Vec::new();
    let result = receive(
        &mut bytes.as_slice(),
        &mut sent,
        |_| vec![ObjectId::for_blob(crate::ObjectFormat::Sha1, b"other")],
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(result, Err(FetchError::Unadvertised(_))));
    assert!(sent.is_empty());
}

#[rstest]
#[case::wire(FetchLimits { max_wire_bytes: 3, ..FetchLimits::default() })]
#[case::advertisement(FetchLimits { max_advertisement_bytes: 4, ..FetchLimits::default() })]
#[case::refs(FetchLimits { max_refs: 0, ..FetchLimits::default() })]
#[case::wants(FetchLimits { max_wants: 0, ..FetchLimits::default() })]
#[case::pack(FetchLimits { max_pack_bytes: 1, ..FetchLimits::default() })]
#[case::objects(FetchLimits { max_objects: 0, ..FetchLimits::default() })]
#[case::payload(FetchLimits { max_object_bytes: 1, ..FetchLimits::default() })]
#[case::decode(FetchLimits { max_decode_bytes: 1, ..FetchLimits::default() })]
#[case::work(FetchLimits { max_resolution_steps: 0, ..FetchLimits::default() })]
fn resource_bounds(#[case] limits: FetchLimits) {
    let (id, pack) = blob_pack();
    assert!(matches!(
        run(&response(id, &pack), limits),
        Err(FetchError::Limit(_) | FetchError::Pack(crate::ObjectReadError::Limit(_)))
    ));
}

#[test]
fn exact_byte_and_count_bounds_succeed() {
    let (id, pack) = blob_pack();
    let bytes = response(id, &pack);
    let limits = FetchLimits {
        max_wire_bytes: bytes.len(),
        max_advertisement_bytes: advertised(id, "side-band-64k ofs-delta").len(),
        max_pack_bytes: pack.len(),
        max_objects: 1,
        max_object_bytes: 13,
        max_decode_bytes: 13,
        max_resolution_steps: 1,
        max_wants: 1,
        max_refs: 1,
        ..FetchLimits::default()
    };
    assert_eq!(run(&bytes, limits).unwrap().object_count(), 1);
}

#[test]
fn progress_can_cancel() {
    let (id, pack) = blob_pack();
    let bytes = response(id, &pack);
    let result = receive(
        &mut bytes.as_slice(),
        &mut Vec::new(),
        select,
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Break(()),
    );
    assert!(matches!(result, Err(FetchError::Cancelled)));
}

#[test]
fn flag_cancels_before_io() {
    let result = receive(
        &mut io::empty(),
        &mut Vec::new(),
        select,
        FetchLimits::default(),
        &AtomicBool::new(true),
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(result, Err(FetchError::Cancelled)));
}

#[test]
fn flag_set_during_progress_cancels() {
    let cancel = AtomicBool::new(false);
    let (id, pack) = blob_pack();
    let bytes = response(id, &pack);
    let result = receive(
        &mut bytes.as_slice(),
        &mut Vec::new(),
        select,
        FetchLimits::default(),
        &cancel,
        |_| {
            cancel.store(true, Ordering::Relaxed);
            ControlFlow::Continue(())
        },
    );
    assert!(matches!(result, Err(FetchError::Cancelled)));
}

struct Interrupted;
impl Read for Interrupted {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::ErrorKind::Interrupted.into())
    }
}
impl Write for Interrupted {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::Interrupted.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn interrupted_read_is_not_retried() {
    let result = receive(
        &mut Interrupted,
        &mut Vec::new(),
        select,
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(result, Err(FetchError::Io(e)) if e.kind() == io::ErrorKind::Interrupted));
}
#[test]
fn interrupted_write_is_not_retried() {
    let bytes = advertised(
        ObjectId::for_blob(crate::ObjectFormat::Sha1, b"x"),
        "side-band-64k",
    );
    let result = receive(
        &mut bytes.as_slice(),
        &mut Interrupted,
        select,
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(result, Err(FetchError::Io(e)) if e.kind() == io::ErrorKind::Interrupted));
}

#[test]
fn rejects_corrupt_pack_checksum() {
    let (id, mut pack) = blob_pack();
    pack[12] ^= 1;
    assert!(matches!(
        run(&response(id, &pack), FetchLimits::default()),
        Err(FetchError::Pack(_))
    ));
}
#[test]
fn rejects_missing_wanted_identity() {
    let (_, pack) = blob_pack();
    let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"not sent");
    assert!(
        matches!(run(&response(id, &pack), FetchLimits::default()), Err(FetchError::Missing(actual)) if actual == id)
    );
}

#[test]
fn refuses_sha256_advertisement_without_request_bytes() {
    let bytes = advertised(ObjectId::Sha256([1; 32]), "side-band-64k");
    let mut sent = vec![];
    let result = receive(
        &mut &bytes[..],
        &mut sent,
        select,
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    assert!(result.is_err());
    assert!(sent.is_empty());
}

#[test]
fn refuses_sha256_selection_without_request_bytes() {
    let bytes = advertised(ObjectId::Sha1([1; 20]), "side-band-64k");
    let mut sent = vec![];
    let result = receive(
        &mut &bytes[..],
        &mut sent,
        |_| vec![ObjectId::Sha256([1; 32])],
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(result, Err(FetchError::ObjectFormat(_))));
    assert!(sent.is_empty());
}
