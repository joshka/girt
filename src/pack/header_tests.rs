//! Entry headers resolve each delta chain once per pack, observed on a Git-generated deep chain.
//!
//! The pack and the expected kinds and sizes come from the Git executable. No upstream
//! implementation or test source is used.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use crate::pack::counters;
use crate::{ObjectId, ObjectKind, ReadLimits, Repository};

fn git(root: &Path, args: &[&str], input: &[u8]) -> String {
    let mut child = Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .args(args)
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
    String::from_utf8(output.stdout).unwrap()
}

/// Forty versions of one blob, which Git stores as delta chains more than ten deep.
fn deep_chain(root: &Path, format: &str) -> Vec<(ObjectId, ObjectKind, usize)> {
    git(
        root,
        &[
            "init",
            "-q",
            "--bare",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
    let mut ids = String::new();
    for version in 0..40 {
        // Each version rewrites one more line, so Git chains the versions rather than basing
        // them all on one.
        let content: String = (0..60)
            .map(|line| {
                let stamp = if line <= version { version } else { 0 };
                format!("line {line} {} {stamp}\n", "x".repeat(40))
            })
            .collect();
        ids += &git(root, &["hash-object", "-w", "--stdin"], content.as_bytes());
    }
    git(
        root,
        &[
            "-c",
            "pack.threads=1",
            "pack-objects",
            "-q",
            "--window=50",
            "--depth=50",
            "objects/pack/pack",
        ],
        ids.as_bytes(),
    );
    git(root, &["prune-packed"], b"");
    git(
        root,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname) %(objecttype) %(objectsize)",
        ],
        b"",
    )
    .lines()
    .map(|line| {
        let mut fields = line.split(' ');
        let id = fields.next().unwrap().parse().unwrap();
        let kind = match fields.next().unwrap() {
            "blob" => ObjectKind::Blob,
            other => panic!("unexpected {other}"),
        };
        (id, kind, fields.next().unwrap().parse().unwrap())
    })
    .collect()
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn transfer_headers_read_each_chain_link_once(#[case] format: &str) {
    let dir = tempfile::tempdir().unwrap();
    let expected = deep_chain(dir.path(), format);
    let listing = git(dir.path(), &["count-objects", "-v"], b"");
    assert!(listing.contains("packs: 1"), "{listing}");
    let pack = std::fs::read_dir(dir.path().join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "idx"))
        .unwrap();
    let depths = git(
        dir.path(),
        &["verify-pack", "-v", pack.to_str().unwrap()],
        b"",
    );
    let deepest = depths
        .lines()
        .filter_map(|line| line.split_whitespace().nth(5)?.parse::<usize>().ok())
        .max();
    assert!(deepest >= Some(10), "fixture chains too shallow:\n{depths}");
    let objects = Repository::open(dir.path())
        .unwrap()
        .objects(Default::default())
        .unwrap();
    let cancel = AtomicBool::new(false);
    counters::reset();
    for &(id, kind, size) in &expected {
        let (actual, length, _) = objects
            .transfer_object(id, ReadLimits::default(), &cancel)
            .unwrap()
            .unwrap();
        assert_eq!((actual, length), (kind, size), "{id}");
    }
    // Each entry needs its own header and, for a delta, its result-size prefix. Resolving a
    // chain's base kind again for every member read every link above it.
    let reads = counters::OS_READS.with(std::cell::Cell::get);
    assert!(
        reads <= expected.len() * 3,
        "{reads} system reads for {} entries",
        expected.len()
    );
}

/// A remembered chain still counts its full depth against a later caller's smaller limit.
#[test]
fn remembered_chains_keep_depth_limits() {
    let dir = tempfile::tempdir().unwrap();
    let expected = deep_chain(dir.path(), "sha1");
    let pack = std::fs::read_dir(dir.path().join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "idx"))
        .unwrap();
    // Delta lines read: id, type, size, packed size, offset, depth, base.
    let (deepest, depth) = git(
        dir.path(),
        &["verify-pack", "-v", pack.to_str().unwrap()],
        b"",
    )
    .lines()
    .filter_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        (fields.len() == 7).then(|| {
            (
                fields[0].parse::<ObjectId>().unwrap(),
                fields[5].parse::<usize>().unwrap(),
            )
        })
    })
    .max_by_key(|&(_, depth)| depth)
    .unwrap();
    let objects = Repository::open(dir.path())
        .unwrap()
        .objects(Default::default())
        .unwrap();
    let cancel = AtomicBool::new(false);
    for &(id, ..) in &expected {
        objects
            .transfer_object(id, ReadLimits::default(), &cancel)
            .unwrap();
    }
    let limit = |max_delta_depth| ReadLimits {
        max_delta_depth,
        ..ReadLimits::default()
    };
    assert!(
        objects
            .transfer_object(deepest, limit(depth - 1), &cancel)
            .is_err()
    );
    assert!(
        objects
            .transfer_object(deepest, limit(depth), &cancel)
            .is_ok()
    );
}
