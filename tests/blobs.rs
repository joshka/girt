//! Original fixtures generated in temporary directories; see docs/compatibility.md.
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use girt::{Error, LooseObjects, ObjectFormat, ObjectId};
use rstest::rstest;

/// Maps an identity to its loose-object path so tests can inspect or replace the stored file.
fn object_path(directory: &Path, id: ObjectId) -> std::path::PathBuf {
    let hex = id.to_string();
    directory.join(&hex[..2]).join(&hex[2..])
}

/// Wraps supplied bytes in valid zlib, allowing tests to isolate malformed object contents.
fn compressed(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// Writes fixture bytes directly, bypassing the validation performed by the public writer.
fn install(directory: &Path, id: ObjectId, bytes: &[u8]) {
    let path = object_path(directory, id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Runs Git in the disposable repository with supplied stdin and returns stdout.
/// Inherited Git overrides and user/system configuration are disabled; initialization disables
/// templates explicitly at the call site.
fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Git is required for interoperability tests");
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Checks empty, text, and binary blobs against Git's identities and readers in both directions.
/// Removing girt's file before Git writes ensures the final read exercises Git-produced storage.
#[rstest]
#[case::empty(Vec::new())]
#[case::text(b"girt original fixture\n".to_vec())]
#[case::binary((0..=255).cycle().take(16384).collect())]
fn interoperates_with_git_in_both_directions(#[case] bytes: Vec<u8>) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &["init", "--bare", "--object-format=sha1", "--template=", "."],
        b"",
    );
    let directory = root.path().join("objects");
    let objects = LooseObjects::new(&directory, ObjectFormat::Sha1).unwrap();
    let expected = git(root.path(), &["hash-object", "--stdin"], &bytes);
    let expected: ObjectId = std::str::from_utf8(&expected)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    let id = objects.write_blob(&bytes).unwrap();
    assert_eq!(id, expected);
    assert_eq!(
        git(root.path(), &["cat-file", "blob", &id.to_string()], b""),
        bytes
    );

    fs::remove_file(object_path(&directory, id)).unwrap();
    git(root.path(), &["hash-object", "-w", "--stdin"], &bytes);
    assert_eq!(objects.read_blob(id, bytes.len()).unwrap(), bytes);
}

/// Reports a missing object as a filesystem NotFound error rather than malformed content.
#[test]
fn reports_missing_object() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let error = objects
        .read_blob(ObjectId::for_blob(girt::ObjectFormat::Sha1, b"missing"), 10)
        .unwrap_err();
    let Error::Io(error) = error else {
        panic!("expected an I/O error")
    };
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

/// Enforces the caller's limit for both highly compressed input and a one-byte overrun.
#[rstest]
#[case::highly_compressed(vec![0; 20000], 10)]
#[case::one_byte_over_limit(b"abc".to_vec(), 2)]
fn rejects_oversized_objects(#[case] bytes: Vec<u8>, #[case] limit: usize) {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let id = objects.write_blob(&bytes).unwrap();
    assert!(matches!(objects.read_blob(id, limit), Err(Error::TooLarge)));
}

/// Recognizes a valid non-blob header but refuses to expose it through the blob API.
#[test]
fn rejects_unsupported_object_type() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"abc");
    install(root.path(), id, &compressed(b"tree 0\0"));
    assert!(matches!(
        objects.read_blob(id, 10),
        Err(Error::UnsupportedObjectType)
    ));
}

/// Rejects malformed object content independently of zlib decoding, which succeeds for each case.
#[rstest]
#[case::missing_terminator(b"blob 3")]
#[case::missing_length(b"blob\0abc")]
#[case::leading_zero(b"blob 03\0abc")]
#[case::positive_sign(b"blob +3\0abc")]
#[case::negative_length(b"blob -1\0abc")]
#[case::length_too_small(b"blob 2\0abc")]
#[case::length_too_large(b"blob 4\0abc")]
#[case::wrong_identity(b"blob 3\0xyz")]
#[case::unrepresentable_length(b"blob 999999999999999999999999999999\0abc")]
fn rejects_malformed_headers_lengths_and_identities(#[case] encoded: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"abc");
    install(root.path(), id, &compressed(encoded));
    assert!(matches!(
        objects.read_blob(id, 1024),
        Err(Error::Corrupt(_))
    ));
}

// Original fixture generated with Python zlib.compress(b"blob 3\0abc"). Fixed bytes make every
// truncation case independent of compressor output changes; the complete fixture is tested below.
const ABC_LOOSE: &[u8] = &[
    0x78, 0x9c, 0x4b, 0xca, 0xc9, 0x4f, 0x52, 0x30, 0x66, 0x48, 0x4c, 0x4a, 0x06, 0x00, 0x11, 0xd9,
    0x03, 0x19,
];

/// Confirms the fixed fixture used by corruption tests is readable before it is damaged.
#[test]
fn reads_complete_zlib_fixture() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"abc");
    install(root.path(), id, ABC_LOOSE);
    assert_eq!(objects.read_blob(id, 3).unwrap(), b"abc");
}

/// Rejects every incomplete prefix, including complete payloads missing part of the checksum.
#[rstest]
#[case::empty(0)]
#[case::zlib_header_byte_1(1)]
#[case::zlib_header_only(2)]
#[case::deflate_prefix_1(3)]
#[case::deflate_prefix_2(4)]
#[case::deflate_prefix_3(5)]
#[case::deflate_prefix_4(6)]
#[case::deflate_prefix_5(7)]
#[case::deflate_prefix_6(8)]
#[case::deflate_prefix_7(9)]
#[case::deflate_prefix_8(10)]
#[case::deflate_prefix_9(11)]
#[case::deflate_prefix_10(12)]
#[case::deflate_prefix_11(13)]
#[case::missing_checksum(14)]
#[case::checksum_byte_1(15)]
#[case::checksum_byte_2(16)]
#[case::checksum_byte_3(17)]
fn rejects_truncated_zlib_data(#[case] length: usize) {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"abc");
    install(root.path(), id, &ABC_LOOSE[..length]);
    assert!(matches!(objects.read_blob(id, 100), Err(Error::Corrupt(_))));
}

/// Detects checksum damage even though the header and blob payload remain intact.
#[test]
fn rejects_invalid_zlib_checksum() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"abc");
    let mut encoded = ABC_LOOSE.to_vec();
    *encoded.last_mut().unwrap() ^= 1;
    install(root.path(), id, &encoded);
    assert!(matches!(objects.read_blob(id, 100), Err(Error::Corrupt(_))));
}

/// Requires the file to end after one zlib stream, rejecting junk and concatenated streams.
#[rstest]
#[case::trailing_byte(b"\0")]
#[case::second_stream(ABC_LOOSE)]
fn rejects_trailing_zlib_data(#[case] suffix: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"abc");
    let encoded = [ABC_LOOSE, suffix].concat();
    install(root.path(), id, &encoded);
    assert!(matches!(objects.read_blob(id, 100), Err(Error::Corrupt(_))));
}

/// Reports invalid compression framing as corruption before interpreting object contents.
#[test]
fn rejects_non_zlib_data() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"abc");
    install(root.path(), id, b"not zlib");
    assert!(matches!(objects.read_blob(id, 100), Err(Error::Corrupt(_))));
}

/// Concurrent identical writes must converge on one readable object without temporary-file leaks.
#[test]
fn concurrent_writes_publish_one_object() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let bytes = b"concurrent original fixture";
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, bytes);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let objects = &objects;
            scope.spawn(move || assert_eq!(objects.write_blob(bytes).unwrap(), id));
        }
    });
    let path = object_path(root.path(), id);
    assert_eq!(objects.read_blob(id, bytes.len()).unwrap(), bytes);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}

/// A duplicate write must preserve the existing compressed bytes and leave no temporary file.
#[test]
fn duplicate_write_preserves_existing_object() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let id = objects.write_blob(b"duplicate").unwrap();
    let path = object_path(root.path(), id);
    let original = fs::read(&path).unwrap();
    assert_eq!(objects.write_blob(b"duplicate").unwrap(), id);
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}

/// A failed duplicate write must preserve corrupt existing bytes rather than silently repair them.
#[test]
fn write_preserves_corrupt_existing_object() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"duplicate");
    let path = object_path(root.path(), id);
    install(root.path(), id, b"corrupt existing object");
    assert!(matches!(
        objects.write_blob(b"duplicate"),
        Err(Error::Corrupt(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), b"corrupt existing object");
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}

/// Blocks publication with a directory at the destination object path.
/// The write must fail, preserve that directory, and remove its temporary file.
#[test]
fn failed_publication_leaves_no_temporary_file() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
    let id = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"blocked");
    let path = object_path(root.path(), id);
    fs::create_dir_all(&path).unwrap();
    assert!(objects.write_blob(b"blocked").is_err());
    assert!(path.is_dir());
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}
