//! Parallel import of Git-written packs large enough to use worker threads.
//!
//! Packs and the expected indexes come from the Git executable. No upstream implementation or
//! test source is used.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use rstest::rstest;

use super::super::Imported;
use super::MIN_PARALLEL_OBJECTS;
use crate::fetch::{FetchError, FetchLimits};
use crate::{IdMap, ObjectFormat};

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
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// A Git pack of several thousand blobs, trees and commits, mostly OFS deltas.
fn history_pack(root: &Path, format: ObjectFormat) -> Vec<u8> {
    let object_format = format!("--object-format={format}");
    git(root, &["init", "-q", &object_format, "."], b"");
    // A background `gc --auto` could repack and delete objects while later commands run.
    git(root, &["config", "gc.auto", "0"], b"");
    git(root, &["config", "maintenance.auto", "false"], b"");
    for round in 0..4 {
        for file in 0..600 {
            if round > 0 && file % 4 != round {
                continue;
            }
            let directory = root.join(format!("d{}", file % 20));
            std::fs::create_dir_all(&directory).unwrap();
            let content: String = (0..30)
                .map(|line| {
                    format!(
                        "file {file} line {line} round {}\n",
                        (line == 0) as usize * round
                    )
                })
                .collect();
            std::fs::write(directory.join(format!("f{file}")), content).unwrap();
        }
        git(root, &["add", "-A"], b"");
        git(
            root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                &format!("r{round}"),
            ],
            b"",
        );
    }
    git(
        root,
        &[
            "pack-objects",
            "--stdout",
            "--revs",
            "--delta-base-offset",
            "--window=10",
        ],
        b"HEAD\n",
    )
}

/// Workers hash and parse while decoding continues; the index must equal Git's own.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn parallel_import_generates_gits_index(#[case] format: ObjectFormat) {
    let dir = tempfile::tempdir().unwrap();
    let pack = history_pack(dir.path(), format);
    let imported = Imported::read(
        format,
        &pack,
        FetchLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(imported.objects.len() > MIN_PARALLEL_OBJECTS);
    std::fs::write(dir.path().join("received.pack"), &pack).unwrap();
    git(
        dir.path(),
        &["index-pack", "-o", "expected.idx", "received.pack"],
        b"",
    );
    assert_eq!(
        imported.index,
        std::fs::read(dir.path().join("expected.idx")).unwrap()
    );
    let (objects, ..) = imported.objects.verified.totals();
    assert_eq!(objects, imported.objects.len());
}

/// A duplicate identity found by a worker is still rejected before any index exists.
#[test]
fn parallel_import_rejects_duplicate_identity() {
    let dir = tempfile::tempdir().unwrap();
    let format = ObjectFormat::Sha1;
    let pack = history_pack(dir.path(), format);
    std::fs::write(dir.path().join("received.pack"), &pack).unwrap();
    git(
        dir.path(),
        &["index-pack", "-o", "received.idx", "received.pack"],
        b"",
    );
    // Verify-pack lines: id, type, size, packed size, offset, then depth and base for deltas.
    let listing =
        String::from_utf8(git(dir.path(), &["verify-pack", "-v", "received.idx"], b"")).unwrap();
    let (offset, packed) = listing
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|fields| fields.len() == 5 && fields[1] == "blob")
        .map(|fields| {
            (
                fields[4].parse::<usize>().unwrap(),
                fields[3].parse::<usize>().unwrap(),
            )
        })
        .unwrap();
    let end = pack.len() - format.digest_len();
    let mut duplicated = pack[..end].to_vec();
    duplicated.extend_from_slice(&pack[offset..offset + packed]);
    let count = u32::from_be_bytes(duplicated[8..12].try_into().unwrap()) + 1;
    duplicated[8..12].copy_from_slice(&count.to_be_bytes());
    let trailer = format.checksum(&duplicated);
    duplicated.extend_from_slice(trailer.as_bytes());
    assert!(matches!(
        Imported::read_with_known(
            format,
            &duplicated,
            &IdMap::default(),
            FetchLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(FetchError::Pack(super::Error::Corrupt(
            "duplicate received identity"
        )))
    ));
}

fn limits(max_threads: usize) -> FetchLimits {
    FetchLimits {
        max_threads,
        ..FetchLimits::default()
    }
}

/// Inline hashing and every worker count produce the same index for the same pack.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn worker_counts_generate_identical_indexes(#[case] format: ObjectFormat) {
    let dir = tempfile::tempdir().unwrap();
    let pack = history_pack(dir.path(), format);
    let index = |threads| {
        Imported::read(format, &pack, limits(threads), &AtomicBool::new(false))
            .unwrap()
            .index
    };
    let inline = index(1);
    for threads in [0, 2, 3, 8] {
        assert_eq!(index(threads), inline, "{threads} threads");
    }
}

/// Cancellation while workers hold submitted objects returns no result, and the scope joins them.
#[rstest]
#[case::inline(1)]
#[case::workers(8)]
fn cancellation_during_parallel_import_returns_nothing(#[case] threads: usize) {
    let dir = tempfile::tempdir().unwrap();
    let pack = history_pack(dir.path(), ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let mut seen = 0;
    let mut notify = |_| {
        seen += 1;
        if seen == MIN_PARALLEL_OBJECTS / 2 {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    };
    let result = Imported::read_observed(
        ObjectFormat::Sha1,
        &pack,
        &IdMap::default(),
        limits(threads),
        &mut crate::fetch::ValidationObserver::new(&cancel, &mut notify),
    );
    assert!(
        matches!(result, Err(FetchError::Cancelled)),
        "{:?}",
        result.err()
    );
}

/// A corrupted entry in a resealed pack, so the checksum is valid, is still rejected by every
/// worker count.
#[rstest]
#[case::inline(1)]
#[case::workers(8)]
fn corrupt_entry_with_valid_checksum_is_rejected(#[case] threads: usize) {
    let dir = tempfile::tempdir().unwrap();
    let format = ObjectFormat::Sha1;
    let mut pack = history_pack(dir.path(), format);
    let end = pack.len() - format.digest_len();
    // Late in the pack, so workers already hold earlier objects when decoding fails.
    pack[end - 100] ^= 0x55;
    pack.truncate(end);
    let trailer = format.checksum(&pack);
    pack.extend_from_slice(trailer.as_bytes());
    assert!(Imported::read(format, &pack, limits(threads), &AtomicBool::new(false)).is_err());
}
