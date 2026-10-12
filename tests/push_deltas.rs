//! Original Git-generated large-blob edits exercise push delta selection and receiver import.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use girt::push::{ForcePolicy, PreparedPush, PushCommand, PushLimits};
use girt::refs::RefName;
use girt::{ObjectFormat, ObjectId, PackLimits, Repository};
use rstest::rstest;

fn git(path: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("git")
        .current_dir(path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Delta Author")
        .env("GIT_AUTHOR_EMAIL", "delta@example.com")
        .env("GIT_COMMITTER_NAME", "Delta Author")
        .env("GIT_COMMITTER_EMAIL", "delta@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap().trim().to_owned()
}

fn payload() -> Vec<u8> {
    let mut state = 0x12345678u32;
    (0..512 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

struct Fixture {
    _root: tempfile::TempDir,
    source: PathBuf,
    dest: PathBuf,
    old: ObjectId,
    new: ObjectId,
    data: Vec<u8>,
    blob: String,
}

fn commit(path: &Path, data: &[u8], parent: Option<ObjectId>) -> (ObjectId, String) {
    let blob = text(git(path, &["hash-object", "-w", "--stdin"], data));
    let tree = text(git(path, &["mktree"], format!("100644 blob {blob}\tlarge.bin\n").as_bytes()));
    let mut args = vec!["commit-tree", &tree];
    let parent = parent.map(|id| id.to_string());
    if let Some(parent) = &parent {
        args.extend(["-p", parent]);
    }
    let id: ObjectId = text(git(path, &args, b"original delta fixture\n")).parse().unwrap();
    git(path, &["update-ref", "refs/heads/main", &id.to_string()], &[]);
    (id, blob)
}

fn fixture(format: ObjectFormat) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.git");
    let dest = root.path().join("dest.git");
    git(root.path(), &["init", "-q", "--bare", &format!("--object-format={format}"), source.to_str().unwrap()], &[]);
    let mut data = payload();
    let (old, _) = commit(&source, &data, None);
    git(root.path(), &["clone", "-q", "--bare", source.to_str().unwrap(), dest.to_str().unwrap()], &[]);
    data[262144..262152].copy_from_slice(b"EDITED!!");
    let (new, blob) = commit(&source, &data, Some(old));
    Fixture { _root: root, source, dest, old, new, data, blob }
}

fn prepare(f: &Fixture, roots: &[ObjectId], depth: usize) -> PreparedPush {
    let repo = Repository::open(&f.source).unwrap();
    let mut limits = PushLimits::default();
    limits.read.max_delta_depth = depth;
    PreparedPush::new_local(
        &repo.objects(PackLimits::trusted()).unwrap(),
        vec![PushCommand {
            name: RefName::new("refs/heads/main").unwrap(),
            expected: Some(f.old),
            new: f.new,
            force: ForcePolicy::FastForwardOnly,
        }],
        roots,
        limits,
        &AtomicBool::new(false),
    ).unwrap()
}

fn publish(f: &Fixture, prepared: &PreparedPush) {
    let report = girt::push::send_local(
        &f.dest,
        prepared,
        &AtomicBool::new(false),
    ).unwrap();
    assert!(report.all_succeeded());
    assert_eq!(git(&f.dest, &["cat-file", "blob", &f.blob], &[]), f.data);
    assert_eq!(text(git(&f.dest, &["rev-parse", "refs/heads/main"], &[])), f.new.to_string());
    git(&f.dest, &["fsck", "--strict"], &[]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn small_edit_uses_advertised_base_and_matches_git_budget(#[case] format: ObjectFormat) {
    let f = fixture(format);
    let oracle = git(&f.source, &["pack-objects", "--stdout", "--revs", "--thin"], format!("{}\n^{}\n", f.new, f.old).as_bytes());
    let prepared = prepare(&f, &[f.old], 1);
    assert!(prepared.pack_bytes() < f.data.len() / 20, "pack bytes: {}", prepared.pack_bytes());
    assert!(prepared.pack_bytes() <= oracle.len() * 2, "girt={} Git={}", prepared.pack_bytes(), oracle.len());
    publish(&f, &prepared);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn zero_depth_sends_whole_edit_and_remains_readable(#[case] format: ObjectFormat) {
    let f = fixture(format);
    let prepared = prepare(&f, &[f.old], 0);
    assert!(prepared.pack_bytes() >= f.data.len());
    publish(&f, &prepared);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn unadvertised_base_is_sent_in_pack(#[case] format: ObjectFormat) {
    let f = fixture(format);
    let prepared = prepare(&f, &[], 1);
    assert!(prepared.pack_bytes() >= f.data.len());
    publish(&f, &prepared);
}
