//! Original fixtures generated with Git, with Git as the object and pack fidelity oracle.
use std::ops::ControlFlow;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::fetch::{FetchLimits, KnownHistory, LocalFetchProgress, receive_local_with_known_and_progress};
use girt::refs::{Expected, RefName, Target};
use girt::transport::TransportControl;
use girt::{InitKind, ObjectFormat, PackLimits, Repository};
use rstest::rstest;

fn git(path: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C").arg(path).args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Transfer Fixture")
        .env("GIT_AUTHOR_EMAIL", "transfer@example.invalid")
        .env("GIT_COMMITTER_NAME", "Transfer Fixture")
        .env("GIT_COMMITTER_EMAIL", "transfer@example.invalid")
        .output().unwrap();
    assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}

fn fixture(format: ObjectFormat) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-b", "main", &format!("--object-format={format}")]);
    // Similar, independently written versions cause Git to produce real delta chains.
    let base = "original fixture line\n".repeat(2048);
    for n in 0..100 {
        std::fs::write(root.path().join("file"), format!("{base}version {n}\n")).unwrap();
        git(root.path(), &["add", "file"]);
        git(root.path(), &["commit", "-qm", &format!("version {n}")]);
    }
    git(root.path(), &["repack", "-adf", "--depth=12", "--window=100"]);
    root
}

fn tip(path: &Path) -> girt::ObjectId {
    String::from_utf8(git(path, &["rev-parse", "HEAD"])).unwrap().trim().parse().unwrap()
}

fn receive(path: &Path, known: &KnownHistory) -> (girt::fetch::ReceivedFetch, u64) {
    let mut reads = 0;
    let received = receive_local_with_known_and_progress(
        path, |_| vec![tip(path)], known, FetchLimits::trusted(),
        TransportControl::new(&AtomicBool::new(false)), |_| ControlFlow::Continue(()),
        |event| {
            if let LocalFetchProgress::Reading { objects } = event { reads = objects; }
        },
    ).unwrap();
    (received, reads)
}

fn publish(received: &girt::fetch::ReceivedFetch, dest: &Repository, id: girt::ObjectId) {
    received.install(dest, PackLimits::trusted(), &AtomicBool::new(false)).unwrap();
    dest.references().unwrap().update_without_reflog(
        &RefName::new("refs/heads/main").unwrap(), Target::Direct(id), Expected::Any,
    ).unwrap();
    git(dest.git_dir(), &["fsck", "--strict"]);
}

fn object_set(path: &Path) -> Vec<String> {
    let bytes = git(path, &["cat-file", "--batch-all-objects", "--batch-check"]);
    let mut lines: Vec<_> = String::from_utf8(bytes).unwrap().lines().map(str::to_owned).collect();
    lines.sort();
    lines
}

fn delta_count(path: &Path) -> usize {
    let pack_dir = path.join("objects/pack");
    std::fs::read_dir(pack_dir).unwrap().map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "idx"))
        .map(|idx| {
            let output = git(path, &["verify-pack", "-v", idx.to_str().unwrap()]);
            String::from_utf8(output).unwrap().lines().filter(|line| line.split_whitespace().count() == 7).count()
        }).sum()
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn local_clone_pack_reuses_source_deltas_and_matches_git(#[case] format: ObjectFormat) {
    let source = fixture(format);
    let root = tempfile::tempdir().unwrap();
    let oracle = root.path().join("git.git");
    git(root.path(), &["clone", "--bare", "--no-local", source.path().to_str().unwrap(), oracle.to_str().unwrap()]);
    let dest = Repository::init(format, root.path().join("girt.git"), InitKind::Bare).unwrap();
    let (received, _) = receive(source.path(), &KnownHistory::default());
    publish(&received, &dest, tip(source.path()));
    assert_eq!(object_set(dest.git_dir()), object_set(&oracle));
    assert!(delta_count(dest.git_dir()) > 0, "transfer must keep source deltas");
    let pack = std::fs::read_dir(dest.object_dir().join("pack")).unwrap()
        .map(|entry| entry.unwrap().path()).find(|p| p.extension().is_some_and(|e| e == "pack")).unwrap();
    git(dest.git_dir(), &["index-pack", "--strict", pack.to_str().unwrap()]);
}

#[test]
fn local_noop_fetch_reads_no_objects() {
    let source = fixture(ObjectFormat::Sha1);
    let repo = Repository::open(source.path()).unwrap();
    let known = KnownHistory::from_store(repo.objects(PackLimits::trusted()).unwrap(), &[tip(source.path())], FetchLimits::trusted(), &AtomicBool::new(false)).unwrap();
    let (received, reads) = receive(source.path(), &known);
    assert_eq!(received.pack_bytes(), 0);
    assert_eq!(reads, 0);
}

#[test]
fn local_fetch_of_one_commit_reads_only_new_closure() {
    let source = fixture(ObjectFormat::Sha1);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("dest.git");
    git(root.path(), &["clone", "--bare", source.path().to_str().unwrap(), path.to_str().unwrap()]);
    let dest = Repository::open(&path).unwrap();
    let known = KnownHistory::from_store(dest.objects(PackLimits::trusted()).unwrap(), &[tip(&path)], FetchLimits::trusted(), &AtomicBool::new(false)).unwrap();
    std::fs::write(source.path().join("file"), b"one changed file\n").unwrap();
    git(source.path(), &["add", "file"]);
    git(source.path(), &["commit", "-qm", "one new commit"]);
    let (received, reads) = receive(source.path(), &known);
    assert_eq!(received.object_count(), 3);
    assert_eq!(reads, 3);
    publish(&received, &dest, tip(source.path()));
    assert_eq!(object_set(&path), object_set(&source.path().join(".git")));
}

#[test]
fn local_fetch_stops_at_haves_but_sends_other_history() {
    let source = fixture(ObjectFormat::Sha1);
    let root = tempfile::tempdir().unwrap();
    let dest = Repository::init(ObjectFormat::Sha1, root.path().join("dest.git"), InitKind::Bare).unwrap();
    let unrelated = dest.loose_objects().write_blob(b"destination only").unwrap();
    let known = KnownHistory::new_local(&dest.objects(PackLimits::trusted()).unwrap(), &[unrelated], FetchLimits::trusted(), &AtomicBool::new(false)).unwrap();
    let (received, _) = receive(source.path(), &known);
    assert_eq!(received.object_count(), 300);
    publish(&received, &dest, tip(source.path()));
    git(dest.git_dir(), &["fsck", "--connectivity-only"]);
}
