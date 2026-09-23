//! Original fixtures generated in temporary directories; see docs/compatibility.md.
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use girt::{Error, LooseObjects, ObjectId};

fn object_path(directory: &Path, id: ObjectId) -> std::path::PathBuf {
    let hex = id.to_string();
    directory.join(&hex[..2]).join(&hex[2..])
}

fn compressed(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn install(directory: &Path, id: ObjectId, bytes: &[u8]) {
    let path = object_path(directory, id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

// Remove inherited Git overrides and disable user/system config and templates. Commands operate
// only in the test's disposable directory, with no changes to global configuration.
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

#[test]
fn interoperates_with_git_in_both_directions() {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &["init", "--bare", "--object-format=sha1", "--template=", "."],
        b"",
    );
    let directory = root.path().join("objects");
    let objects = LooseObjects::new(&directory, "sha1").unwrap();
    let binary: Vec<u8> = (0..=255).cycle().take(16384).collect();
    for bytes in [b"".as_slice(), b"girt original fixture\n", &binary] {
        let expected = git(root.path(), &["hash-object", "--stdin"], bytes);
        let expected: ObjectId = std::str::from_utf8(&expected)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let id = objects.write_blob(bytes).unwrap();
        assert_eq!(id, expected);
        assert_eq!(
            git(root.path(), &["cat-file", "blob", &id.to_string()], b""),
            bytes
        );
        fs::remove_file(object_path(&directory, id)).unwrap();
        git(root.path(), &["hash-object", "-w", "--stdin"], bytes);
        assert_eq!(objects.read_blob(id, bytes.len()).unwrap(), bytes);
    }
}

#[test]
fn rejects_missing_unsupported_and_oversized_objects() {
    let root = tempfile::tempdir().unwrap();
    assert!(matches!(
        LooseObjects::new(root.path(), "sha256"),
        Err(Error::UnsupportedFormat(_))
    ));
    let objects = LooseObjects::new(root.path(), "sha1").unwrap();
    let id = ObjectId::for_blob(b"missing");
    assert!(matches!(objects.read_blob(id, 10), Err(Error::Io(error))
        if error.kind() == std::io::ErrorKind::NotFound));
    let id = objects.write_blob(&vec![0; 20000]).unwrap();
    assert!(matches!(objects.read_blob(id, 10), Err(Error::TooLarge)));
    let id = objects.write_blob(b"abc").unwrap();
    assert!(matches!(objects.read_blob(id, 2), Err(Error::TooLarge)));
    install(root.path(), id, &compressed(b"tree 0\0"));
    assert!(matches!(
        objects.read_blob(id, 10),
        Err(Error::UnsupportedObjectType)
    ));
}

#[test]
fn rejects_malformed_headers_lengths_and_identities() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), "sha1").unwrap();
    let id = ObjectId::for_blob(b"abc");
    for encoded in [
        b"blob 3".as_slice(),
        b"blob\0abc",
        b"blob 03\0abc",
        b"blob +3\0abc",
        b"blob -1\0abc",
        b"blob 2\0abc",
        b"blob 4\0abc",
        b"blob 3\0xyz",
        b"blob 999999999999999999999999999999\0abc",
    ] {
        install(root.path(), id, &compressed(encoded));
        assert!(
            matches!(objects.read_blob(id, 1024), Err(Error::Corrupt(_))),
            "{encoded:?}"
        );
    }
}

#[test]
fn rejects_truncated_corrupt_and_trailing_zlib_data() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), "sha1").unwrap();
    let id = ObjectId::for_blob(b"abc");
    let encoded = compressed(&girt::encode_blob(b"abc"));
    for length in 0..encoded.len() {
        install(root.path(), id, &encoded[..length]);
        assert!(matches!(objects.read_blob(id, 100), Err(Error::Corrupt(_))));
    }
    let mut bad_checksum = encoded.clone();
    *bad_checksum.last_mut().unwrap() ^= 1;
    let mut trailing = encoded.clone();
    trailing.push(0);
    let mut concatenated = encoded.clone();
    concatenated.extend_from_slice(&encoded);
    for invalid in [bad_checksum, trailing, concatenated, b"not zlib".to_vec()] {
        install(root.path(), id, &invalid);
        assert!(matches!(objects.read_blob(id, 100), Err(Error::Corrupt(_))));
    }
}

#[test]
fn duplicate_and_concurrent_writes_preserve_existing_objects() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), "sha1").unwrap();
    let bytes = b"concurrent original fixture";
    let id = ObjectId::for_blob(bytes);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let objects = &objects;
            scope.spawn(move || assert_eq!(objects.write_blob(bytes).unwrap(), id));
        }
    });
    let path = object_path(root.path(), id);
    let original = fs::read(&path).unwrap();
    objects.write_blob(bytes).unwrap();
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(objects.read_blob(id, bytes.len()).unwrap(), bytes);
    fs::write(&path, b"corrupt existing object").unwrap();
    assert!(matches!(objects.write_blob(bytes), Err(Error::Corrupt(_))));
    assert_eq!(fs::read(&path).unwrap(), b"corrupt existing object");
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn failed_publication_leaves_no_temporary_file() {
    let root = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(root.path(), "sha1").unwrap();
    let id = ObjectId::for_blob(b"blocked");
    let path = object_path(root.path(), id);
    fs::create_dir_all(&path).unwrap();
    assert!(objects.write_blob(b"blocked").is_err());
    assert!(path.is_dir());
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}
