//! Regression tests for additive repack contents and stable pack bytes.
#![cfg(unix)]
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use girt::retention::{RepackLimits, RetentionPolicy};
use girt::{ObjectId, Repository};

fn git(root: &Path, args: &[&str], input: &[u8]) -> String {
    let mut child = Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn packs(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root.join("objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pack"))
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Additive repack keeps required objects even when their old pack is protected.
#[test]
fn default_policy_repack_object_count() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "--bare", "--template=", "."], b"");
    let tree = git(root, &["mktree"], b"");
    let commit = git(root, &["commit-tree", &tree], b"one\n");
    git(root, &["update-ref", "refs/heads/main", &commit], b"");
    git(root, &["repack", "-a", "-d"], b"");
    let repo = Repository::open(root).unwrap();
    let published = repo
        .repack_retained(
            &RetentionPolicy::default(),
            RepackLimits::trusted(),
            &AtomicBool::new(false),
        )
        .unwrap();
    eprintln!(
        "default policy: published objects = {}, packs now = {:?}",
        published.written.objects,
        packs(root)
    );
    assert_eq!(
        published.written.objects, 2,
        "expected commit + tree in the replacement under the default policy"
    );
}

fn loose_versions(root: &Path) -> Vec<ObjectId> {
    let mut heads = Vec::new();
    let mut text = String::new();
    for i in 0..6 {
        text.push_str(&format!("line {i} with enough text to be worth a delta\n"));
        let id: ObjectId = git(root, &["hash-object", "-w", "--stdin"], text.as_bytes())
            .parse()
            .unwrap();
        heads.push(id);
    }
    heads
}

/// Newly compressed entries use the same framing that later repacks copy.
#[test]
fn repeated_repack_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "--bare", "--template=", "."], b"");
    let heads = loose_versions(root);
    let repo = Repository::open(root).unwrap();
    let policy = RetentionPolicy {
        heads,
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..RetentionPolicy::trusted()
    };
    let first = repo
        .repack_retained(&policy, RepackLimits::trusted(), &AtomicBool::new(false))
        .unwrap();
    let second = repo
        .repack_retained(&policy, RepackLimits::trusted(), &AtomicBool::new(false))
        .unwrap();
    let third = repo
        .repack_retained(&policy, RepackLimits::trusted(), &AtomicBool::new(false))
        .unwrap();
    eprintln!(
        "checksums: {} / {} / {}; deltas {} / {} / {}; packs {:?}",
        first.written.checksum,
        second.written.checksum,
        third.written.checksum,
        first.written.deltas.entries,
        second.written.deltas.entries,
        third.written.deltas.entries,
        packs(root)
    );
    assert_eq!(
        first.written.checksum, second.written.checksum,
        "second repack must reproduce the first pack"
    );
}
