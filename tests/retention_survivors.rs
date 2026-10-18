//! Integrity regression tests for readable survivors and resource limits.
//! Git verifies original input packs; index mutations model damaged entries and mislabelled bases.
#![cfg(unix)]
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use girt::retention::{MaintenanceIsolation, RepackLimits, RetentionPolicy};
use girt::{ObjectId, PackCompression, PackLimits, ReadLimits, Repository};
use sha1::{Digest, Sha1};

fn git(root: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
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
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}
fn bare() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--bare", "--template=", "."], b"");
    dir
}
fn id(root: &Path, bytes: &[u8]) -> ObjectId {
    String::from_utf8(git(root, &["hash-object", "--stdin"], bytes))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
fn loose(root: &Path, bytes: &[u8]) -> ObjectId {
    String::from_utf8(git(root, &["hash-object", "-w", "--stdin"], bytes))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
fn policy(heads: Vec<ObjectId>) -> RetentionPolicy {
    RetentionPolicy::default()
        .with_heads(heads)
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60))
}
fn limits() -> RepackLimits {
    RepackLimits::default().with_compression(PackCompression::Ordinary)
}
struct Exclusive;
impl MaintenanceIsolation for Exclusive {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        Ok(())
    }
}
fn packs(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(root.join("objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pack"))
        .collect();
    paths.sort();
    paths
}
fn read(root: &Path, object: ObjectId, bound: ReadLimits) -> Result<Vec<u8>, String> {
    let repo = Repository::open(root).map_err(|e| e.to_string())?;
    let objects = repo
        .objects_with(PackLimits::default())
        .map_err(|e| e.to_string())?;
    objects
        .read_with(object, bound)
        .map_err(|e| e.to_string())?
        .map(|o| o.data().to_vec())
        .ok_or_else(|| "missing object".into())
}
fn isolate(pack: &Path) -> tempfile::TempDir {
    let dir = bare();
    fs::copy(pack, dir.path().join("objects/pack/pack-copy.pack")).unwrap();
    fs::copy(
        pack.with_extension("idx"),
        dir.path().join("objects/pack/pack-copy.idx"),
    )
    .unwrap();
    dir
}
/// Four-byte blobs allow an explicit delta: copy the first three bytes, insert the last byte.
/// This encoder is original test code; Git index-pack creates the index and verify-pack validates
/// it.
fn chain(root: &Path, name: &str, values: &[&[u8; 4]]) -> PathBuf {
    let mut pack = b"PACK\0\0\0\x02".to_vec();
    pack.extend_from_slice(&(values.len() as u32).to_be_bytes());
    let mut previous = 0;
    for (n, value) in values.iter().enumerate() {
        let offset = pack.len();
        let payload = if n == 0 {
            pack.push(0x34);
            value.to_vec()
        } else {
            assert_eq!(&value[..3], &values[n - 1][..3]);
            let distance = offset - previous;
            assert!(distance < 128);
            pack.extend_from_slice(&[0x66, distance as u8]);
            vec![4, 4, 0x90, 3, 1, value[3]]
        };
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&payload).unwrap();
        pack.extend(encoder.finish().unwrap());
        previous = offset;
    }
    pack.extend_from_slice(&Sha1::digest(&pack));
    let path = root.join(format!("objects/pack/pack-{name}.pack"));
    fs::write(&path, pack).unwrap();
    git(root, &["index-pack", path.to_str().unwrap()], b"");
    git(
        root,
        &["verify-pack", path.with_extension("idx").to_str().unwrap()],
        b"",
    );
    path
}
fn raw_id(id: ObjectId) -> Vec<u8> {
    let hex = id.to_string();
    (0..20)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}
/// Change an indexed identity while preserving CRCs, offsets, sorted order, fanout and checksums.
fn rename_identity(path: &Path, old: ObjectId, new: ObjectId) {
    let bytes = fs::read(path).unwrap();
    assert_eq!(&bytes[..8], b"\xfftOc\0\0\0\x02");
    let n = u32::from_be_bytes(bytes[1028..1032].try_into().unwrap()) as usize;
    let mut entries: Vec<_> = (0..n)
        .map(|i| {
            let mut oid = bytes[1032 + i * 20..1052 + i * 20].to_vec();
            if oid == raw_id(old) {
                oid = raw_id(new);
            }
            (
                oid,
                bytes[1032 + n * 20 + i * 4..1036 + n * 20 + i * 4].to_vec(),
                bytes[1032 + n * 24 + i * 4..1036 + n * 24 + i * 4].to_vec(),
            )
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = bytes[..8].to_vec();
    for bucket in 0..256 {
        out.extend_from_slice(
            &(entries.iter().filter(|e| e.0[0] as usize <= bucket).count() as u32).to_be_bytes(),
        );
    }
    for e in &entries {
        out.extend_from_slice(&e.0);
    }
    for e in &entries {
        out.extend_from_slice(&e.1);
    }
    for e in &entries {
        out.extend_from_slice(&e.2);
    }
    out.extend_from_slice(&bytes[bytes.len() - 40..bytes.len() - 20]);
    out.extend_from_slice(&Sha1::digest(&out));
    fs::remove_file(path).unwrap();
    fs::write(path, out).unwrap();
}
/// Either refuse before publishing an index or publish bytes readable by a cold reader.
fn assert_repack_safe(root: &Path, policy: &RetentionPolicy, target: ObjectId, expected: &[u8]) {
    let before = packs(root);
    let result =
        Repository::open(root)
            .unwrap()
            .repack_retained(policy, limits(), &AtomicBool::new(false));
    let new: Vec<_> = packs(root)
        .into_iter()
        .filter(|p| !before.contains(p))
        .filter(|p| p.with_extension("idx").exists())
        .collect();
    eprintln!("repack={result:?}, new indexed packs={new:?}");
    for path in &new {
        let isolated = isolate(path);
        assert_eq!(
            read(isolated.path(), target, policy.read),
            Ok(expected.to_vec()),
            "new pack must preserve target bytes for a cold reader"
        );
        // Source packs can deliberately contain mislabelled identities. Verify the replacement
        // independently so Git does not compare it against those damaged duplicate copies.
        git(
            isolated.path(),
            &["verify-pack", "objects/pack/pack-copy.idx"],
            b"",
        );
    }
    assert!(
        result.is_err() || !new.is_empty(),
        "fixture must create distinct output"
    );
    assert_eq!(
        read(root, target, policy.read),
        Ok(expected.to_vec()),
        "live repository must remain readable"
    );
}
#[test]
fn joined_depth_is_checked_before_publication() {
    let dir = bare();
    let root = dir.path();
    chain(root, "a", &[b"abc0", b"abc1", b"abc2"]);
    chain(root, "b", &[b"abc2", b"abc3"]);
    let ids: Vec<_> = [b"abc0", b"abc1", b"abc2", b"abc3"]
        .iter()
        .map(|b| id(root, *b))
        .collect();
    let mut p = policy(ids.clone());
    p.read.max_delta_depth = 2;
    assert_eq!(read(root, ids[3], p.read), Ok(b"abc3".to_vec()));
    assert_eq!(read(root, ids[2], p.read), Ok(b"abc2".to_vec()));
    assert_repack_safe(root, &p, ids[3], b"abc3");
}
#[test]
fn source_base_identity_does_not_change_delta_content() {
    let dir = bare();
    let root = dir.path();
    chain(root, "a", &[b"XYZ0"]);
    let second = chain(root, "b", &[b"abc0", b"abc1"]);
    let x = id(root, b"XYZ0");
    let y = id(root, b"abc1");
    rename_identity(&second.with_extension("idx"), id(root, b"abc0"), x);
    assert_eq!(read(root, x, ReadLimits::default()), Ok(b"XYZ0".to_vec()));
    assert_eq!(read(root, y, ReadLimits::default()), Ok(b"abc1".to_vec()));
    assert_repack_safe(root, &policy(vec![x, y]), y, b"abc1");
}
#[test]
fn primary_loose_precedence_survives_alternate_copy() {
    let dir = bare();
    let alt = bare();
    let x = loose(dir.path(), b"XYZ0");
    let pack = chain(alt.path(), "a", &[b"abc0"]);
    rename_identity(&pack.with_extension("idx"), id(alt.path(), b"abc0"), x);
    fs::write(
        dir.path().join("objects/info/alternates"),
        format!("{}\n", alt.path().join("objects").display()),
    )
    .unwrap();
    assert_eq!(
        read(dir.path(), x, ReadLimits::default()),
        Ok(b"XYZ0".to_vec())
    );
    assert_repack_safe(dir.path(), &policy(vec![x]), x, b"XYZ0");
}
// Git permits old unreachable objects to be reclaimed; recent packs keep their readable copy.
#[rstest::rstest]
#[case::old(false, 253_402_300_799)]
#[case::recent(true, 0)]
fn retirement_applies_recent_cutoff_to_unreachable_objects(
    #[case] recent: bool,
    #[case] cutoff: u64,
) {
    let dir = bare();
    let root = dir.path();
    chain(root, "a", &[b"abc0", b"abc1"]);
    let omitted = id(root, b"abc1");
    let fresh = loose(root, b"fresh root");
    let mut p = policy(vec![id(root, b"abc0"), fresh]);
    p.recent_cutoff = Some(UNIX_EPOCH + Duration::from_secs(cutoff));
    assert_eq!(read(root, omitted, p.read), Ok(b"abc1".to_vec()));
    Repository::open(root)
        .unwrap()
        .retire_old_packs(&mut Exclusive, &p, limits(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(has_readable_pack_copy(root, omitted, b"abc1"), recent);
}

fn protected_growth(keep: bool) {
    let dir = bare();
    let root = dir.path();
    let source = chain(root, "a", &[b"abc0"]);
    if keep {
        fs::write(source.with_extension("keep"), b"").unwrap();
    }
    let object = id(root, b"abc0");
    let mut p = policy(vec![object]);
    if !keep {
        p.recent_cutoff = Some(UNIX_EPOCH);
    }
    let mut copies = Vec::new();
    let mut sizes = Vec::new();
    for round in 0..3 {
        p.heads
            .push(loose(root, format!("new root {round}").as_bytes()));
        let result = Repository::open(root).unwrap().retire_old_packs(
            &mut Exclusive,
            &p,
            limits(),
            &AtomicBool::new(false),
        );
        result.unwrap();
        let current = packs(root);
        copies.push(
            current
                .iter()
                .filter(|path| read(isolate(path).path(), object, ReadLimits::default()).is_ok())
                .count(),
        );
        sizes.push(
            current
                .iter()
                .map(|p| fs::metadata(p).unwrap().len())
                .sum::<u64>(),
        );
    }
    eprintln!("protected keep={keep}: copies={copies:?}, bytes={sizes:?}");
    assert_eq!(
        copies,
        vec![1, 1, 1],
        "protected content must not amplify across maintenance runs"
    );
}
#[test]
fn keep_pack_content_is_not_duplicated() {
    protected_growth(true);
}
#[test]
fn recent_pack_content_is_not_duplicated() {
    protected_growth(false);
}

fn has_readable_pack_copy(root: &Path, object: ObjectId, expected: &[u8]) -> bool {
    packs(root).iter().any(|pack| {
        read(isolate(pack).path(), object, ReadLimits::default()) == Ok(expected.to_vec())
    })
}
#[test]
fn recent_extra_keeps_its_last_readable_copy() {
    let dir = bare();
    let root = dir.path();
    chain(root, "a", &[b"XYZ0"]);
    let second = chain(root, "b", &[b"abc0", b"abc1"]);
    rename_identity(
        &second.with_extension("idx"),
        id(root, b"abc0"),
        id(root, b"XYZ0"),
    );
    let y = id(root, b"abc1");
    assert!(has_readable_pack_copy(root, y, b"abc1"));
    let mut p = policy(vec![loose(root, b"fresh root")]);
    p.recent_cutoff = Some(UNIX_EPOCH);
    let result = Repository::open(root).unwrap().retire_old_packs(
        &mut Exclusive,
        &p,
        limits(),
        &AtomicBool::new(false),
    );
    eprintln!("recent extra retirement={result:?}");
    assert!(
        has_readable_pack_copy(root, y, b"abc1"),
        "recent extra lost its last readable pack copy"
    );
}
#[test]
fn damaged_first_blob_does_not_remove_the_readable_copy() {
    let dir = bare();
    let root = dir.path();
    let damaged = chain(root, "a", &[b"abc0"]);
    let x = id(root, b"XYZ0");
    chain(root, "b", &[b"XYZ0"]);
    rename_identity(&damaged.with_extension("idx"), id(root, b"abc0"), x);
    let tree: ObjectId = String::from_utf8(git(
        root,
        &["mktree", "--missing"],
        format!("100644 blob {x}\tfile\n").as_bytes(),
    ))
    .unwrap()
    .trim()
    .parse()
    .unwrap();
    assert!(has_readable_pack_copy(root, x, b"XYZ0"));
    let result = Repository::open(root).unwrap().retire_old_packs(
        &mut Exclusive,
        &policy(vec![tree]),
        limits(),
        &AtomicBool::new(false),
    );
    eprintln!("damaged blob retirement={result:?}");
    assert!(
        has_readable_pack_copy(root, x, b"XYZ0"),
        "last readable blob copy removed"
    );
}
#[test]
fn damaged_protected_entry_does_not_justify_removing_readable_copy() {
    let dir = bare();
    let root = dir.path();
    chain(root, "a", &[b"XYZ0"]);
    let damaged = chain(root, "b", &[b"abc0"]);
    let x = id(root, b"XYZ0");
    rename_identity(&damaged.with_extension("idx"), id(root, b"abc0"), x);
    fs::write(damaged.with_extension("keep"), b"").unwrap();
    let fresh = loose(root, b"fresh root");
    assert!(has_readable_pack_copy(root, x, b"XYZ0"));
    let result = Repository::open(root).unwrap().retire_old_packs(
        &mut Exclusive,
        &policy(vec![x, fresh]),
        limits(),
        &AtomicBool::new(false),
    );
    eprintln!("protected entry retirement={result:?}");
    assert!(
        has_readable_pack_copy(root, x, b"XYZ0"),
        "protected damaged entry replaced the last readable copy"
    );
}

#[rstest::rstest]
#[case::input_bytes(8, 4, "input bytes")]
#[case::object_bytes(9, 3, "object bytes")]
fn reused_entries_obey_logical_byte_budgets(
    #[case] input: u64,
    #[case] object: u64,
    #[case] reason: &str,
) {
    let dir = bare();
    let root = dir.path();
    chain(root, "a", &[b"abc0", b"abc1"]);
    let fresh = loose(root, b"Z");
    let p = policy(vec![id(root, b"abc0"), id(root, b"abc1"), fresh]);
    let mut bound = limits();
    bound.write.max_input_bytes = input;
    bound.write.max_object_bytes = object;
    let result =
        Repository::open(root)
            .unwrap()
            .repack_retained(&p, bound, &AtomicBool::new(false));
    assert!(
        matches!(result, Err(girt::retention::RepackError::Write(girt::PackWriteError::Limit(actual))) if actual == reason)
    );
    assert_eq!(packs(root).len(), 1);
}

fn git_pack_selected(root: &Path, name: &str, ids: &[ObjectId]) -> PathBuf {
    let input = ids.iter().map(|id| format!("{id}\n")).collect::<String>();
    let bytes = git(
        root,
        &["pack-objects", "--stdout", "--delta-base-offset"],
        input.as_bytes(),
    );
    let path = root.join(format!("objects/pack/pack-{name}.pack"));
    fs::write(&path, bytes).unwrap();
    git(root, &["index-pack", path.to_str().unwrap()], b"");
    git(
        root,
        &["verify-pack", path.with_extension("idx").to_str().unwrap()],
        b"",
    );
    path
}

#[test]
fn recent_unreachable_commit_keeps_older_packed_dependencies() {
    let dir = bare();
    let root = dir.path();
    let blob = loose(root, b"old content still used by recent commit");
    let tree: ObjectId = String::from_utf8(git(
        root,
        &["mktree"],
        format!("100644 blob {blob}\tfile\n").as_bytes(),
    ))
    .unwrap()
    .trim()
    .parse()
    .unwrap();
    let commit: ObjectId = String::from_utf8(git(
        root,
        &[
            "-c",
            "user.name=Probe",
            "-c",
            "user.email=probe@example.com",
            "commit-tree",
            &tree.to_string(),
        ],
        b"recent unreachable commit\n",
    ))
    .unwrap()
    .trim()
    .parse()
    .unwrap();
    let old = git_pack_selected(root, "old", &[blob, tree]);
    git_pack_selected(root, "recent", &[commit]);
    remove_loose_copies(root, &[blob, tree, commit]);
    let file = fs::File::open(&old).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH))
        .unwrap();
    let fresh = loose(root, b"independent live root");
    git(
        root,
        &["update-ref", "refs/tags/current", &fresh.to_string()],
        b"",
    );
    let control = bare();
    copy_fixture_tree(root, control.path());
    git(
        control.path(),
        &["-c", "gc.auto=0", "gc", "--prune=1.hour.ago"],
        b"",
    );
    assert!(
        read(control.path(), commit, ReadLimits::default()).is_ok(),
        "Git retains recent commit"
    );
    assert!(
        read(control.path(), tree, ReadLimits::default()).is_ok(),
        "Git retains recent commit tree"
    );
    assert!(
        read(control.path(), blob, ReadLimits::default()).is_ok(),
        "Git retains recent commit blob"
    );
    let mut p = policy(vec![fresh]);
    p.recent_cutoff = Some(SystemTime::now() - Duration::from_secs(3600));
    assert!(read(root, commit, p.read).is_ok());
    assert!(read(root, tree, p.read).is_ok());
    assert!(read(root, blob, p.read).is_ok());
    let report = Repository::open(root).unwrap().retire_old_packs(
        &mut Exclusive,
        &p,
        limits(),
        &AtomicBool::new(false),
    );
    eprintln!("recent commit dependency retirement: {report:?}");
    assert!(
        read(root, commit, p.read).is_ok(),
        "recent commit must survive"
    );
    assert!(
        read(root, tree, p.read).is_ok(),
        "recent retained commit lost its tree"
    );
    assert!(
        read(root, blob, p.read).is_ok(),
        "recent retained commit lost its blob"
    );
}

fn remove_loose_copies(root: &Path, objects: &[ObjectId]) {
    for object in objects {
        let name = object.to_string();
        fs::remove_file(root.join("objects").join(&name[..2]).join(&name[2..])).unwrap();
    }
}

fn copy_fixture_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_fixture_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            let metadata = entry.metadata().unwrap();
            fs::File::open(&target)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
                .unwrap();
        }
    }
}

#[test]
fn later_protected_copy_is_not_duplicated_in_replacement() {
    let dir = bare();
    let root = dir.path();
    git_pack_selected(root, "a", &[loose(root, b"shared object")]);
    let object = id(root, b"shared object");
    let protected = git_pack_selected(root, "z", &[object]);
    fs::write(protected.with_extension("keep"), b"").unwrap();
    let name = object.to_string();
    fs::remove_file(root.join("objects").join(&name[..2]).join(&name[2..])).unwrap();
    let fresh = loose(root, b"new root makes distinct output");
    let p = policy(vec![object, fresh]);
    let report = Repository::open(root)
        .unwrap()
        .retire_old_packs(&mut Exclusive, &p, limits(), &AtomicBool::new(false))
        .unwrap();
    let copies = packs(root)
        .iter()
        .filter(|pack| read(isolate(pack).path(), object, p.read).is_ok())
        .count();
    assert_eq!(
        copies, 1,
        "later protected copy should suffice; retirement={report:?}"
    );
}
/// A reachable blob located in one pack must not hide a graph-bearing copy of the same identity
/// in another pack: that copy's edges would otherwise go unprotected.
#[test]
fn reachable_blob_with_a_graph_copy_in_another_pack_refuses_the_plan() {
    let dir = bare();
    let root = dir.path();
    // Packs are searched by index name, so the walk locates the blob in `pack-a`.
    chain(root, "a", &[b"abc0"]);
    let x = id(root, b"abc0");
    let mktree = |listing: String| -> ObjectId {
        String::from_utf8(git(root, &["mktree", "--missing"], listing.as_bytes()))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    };
    // The reachable tree is written before the relabelling below, which Git would refuse.
    let tree = mktree(format!("100644 blob {x}\tfile\n"));
    // `pack-b` labels a tree's bytes with the blob's identity.
    let other = mktree(format!("100644 blob {x}\tother\n"));
    let checksum = String::from_utf8(git(
        root,
        &["pack-objects", "-q", "objects/pack/tmp"],
        format!("{other}\n").as_bytes(),
    ))
    .unwrap();
    for extension in ["pack", "idx"] {
        fs::rename(
            root.join(format!("objects/pack/tmp-{}.{extension}", checksum.trim())),
            root.join(format!("objects/pack/pack-b.{extension}")),
        )
        .unwrap();
    }
    let hex = other.to_string();
    fs::remove_file(root.join(format!("objects/{}/{}", &hex[..2], &hex[2..]))).unwrap();
    rename_identity(&root.join("objects/pack/pack-b.idx"), other, x);
    let plan = Repository::open(root)
        .unwrap()
        .plan_retention(&policy(vec![tree]), &AtomicBool::new(false));
    assert!(
        format!("{:?}", plan.outcome).contains("graph-bearing copy"),
        "{:?}",
        plan.outcome
    );
}
/// A delta base the cache already holds still counts toward the read's depth, so a read fits its
/// limits or not whatever earlier reads left in the cache.
#[test]
fn cached_base_still_counts_toward_delta_depth() {
    let dir = bare();
    let root = dir.path();
    chain(root, "a", &[b"abc0", b"abc1", b"abc2"]);
    let tip = id(root, b"abc2");
    let objects = Repository::open(root)
        .unwrap()
        .objects_with(PackLimits::default())
        .unwrap();
    let shallow = ReadLimits::default().with_max_delta_depth(1);
    assert!(objects.read_with(tip, shallow).is_err(), "cold read");
    // A deeper read decodes and caches the tip's bases.
    let tip_data = objects
        .read_with(tip, ReadLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(tip_data.data(), b"abc2");
    assert!(
        objects.read_with(tip, shallow).is_err(),
        "read after caching bases"
    );
}
