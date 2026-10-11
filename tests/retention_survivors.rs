//! Original regression probes for the #23 base debate. Git generates object identities and
//! indexes, and verifies every valid input pack. Deliberate index mutations model corruption.
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
    RetentionPolicy {
        heads,
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..RetentionPolicy::default()
    }
}
fn limits() -> RepackLimits {
    RepackLimits {
        compression: PackCompression::Ordinary,
        ..RepackLimits::default()
    }
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
        .objects(PackLimits::default())
        .map_err(|e| e.to_string())?;
    objects
        .read(object, bound)
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
        git(
            root,
            &["verify-pack", path.with_extension("idx").to_str().unwrap()],
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
#[case::old(false)]
#[case::recent(true)]
fn retirement_applies_recent_cutoff_to_unreachable_objects(#[case] recent: bool) {
    let dir = bare();
    let root = dir.path();
    chain(root, "a", &[b"abc0", b"abc1"]);
    let omitted = id(root, b"abc1");
    let fresh = loose(root, b"fresh root");
    let mut p = policy(vec![id(root, b"abc0"), fresh]);
    if recent {
        p.recent_cutoff = UNIX_EPOCH;
    }
    assert_eq!(read(root, omitted, p.read), Ok(b"abc1".to_vec()));
    Repository::open(root).unwrap().retire_old_packs(
        &mut Exclusive, &p, limits(), &AtomicBool::new(false),
    ).unwrap();
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
        p.recent_cutoff = UNIX_EPOCH;
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
    p.recent_cutoff = UNIX_EPOCH;
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

#[test]
fn reused_entries_obey_logical_input_budget() {
    let dir = bare();
    let root = dir.path();
    chain(root, "a", &[b"abc0", b"abc1"]);
    let fresh = loose(root, b"Z");
    let p = policy(vec![id(root, b"abc0"), id(root, b"abc1"), fresh]);
    let mut bound = limits();
    bound.write.max_input_bytes = 8;
    let result =
        Repository::open(root)
            .unwrap()
            .repack_retained(&p, bound, &AtomicBool::new(false));
    eprintln!("nine logical bytes with budget eight: {result:?}");
    assert!(
        result.is_err(),
        "copied entries plus loose input exceed logical payload budget"
    );
}
