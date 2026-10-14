//! Integrity regression tests for the identity-hash boundary between content reads and walks.
//!
//! Fixtures are Git-generated repositories whose packs are then damaged in place, or given a
//! checksum-consistent mislabelled index entry. `git fsck` is the oracle that Git rejects each one.
#![cfg(unix)]
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use girt::retention::{MaintenanceIsolation, RepackLimits, RetentionPolicy};
use girt::{ObjectFormat, ObjectId, PackLimits, ReadLimits, Repository};
use sha1::Digest;

fn git_raw(root: &Path, args: &[&str], input: &[u8]) -> std::process::Output {
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
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn git(root: &Path, args: &[&str], input: &[u8]) -> String {
    let output = git_raw(root, args, input);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn bare(root: &Path) -> PathBuf {
    git(
        root,
        &[
            "init",
            "--bare",
            "--template=",
            "--initial-branch=main",
            ".",
        ],
        b"",
    );
    root.to_path_buf()
}

struct FixtureIsolation;

impl MaintenanceIsolation for FixtureIsolation {
    type Guard = ();

    fn acquire(&mut self, _: &Repository) -> std::io::Result<Self::Guard> {
        Ok(())
    }
}

/// Every loose object counts as old, so prune may delete anything the plan finds unreachable.
fn policy() -> RetentionPolicy {
    RetentionPolicy::trusted().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60))
}

fn id(text: &str) -> ObjectId {
    text.parse().unwrap()
}

fn writable(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    fs::set_permissions(path, permissions).unwrap();
}

fn only_pack(root: &Path) -> PathBuf {
    let packs: Vec<PathBuf> = fs::read_dir(root.join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pack"))
        .collect();
    assert_eq!(packs.len(), 1, "{packs:?}");
    packs[0].clone()
}

/// All files under `objects`, so a test can show that a refused operation deleted nothing.
fn object_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.join("objects")];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Flips one byte inside the zlib stream of `target`'s entry, leaving the index and trailer intact.
fn damage_entry(root: &Path, target: ObjectId) {
    let pack = only_pack(root);
    let listing = git(
        root,
        &[
            "verify-pack",
            "-v",
            pack.with_extension("idx").to_str().unwrap(),
        ],
        b"",
    );
    let line = listing
        .lines()
        .find(|line| line.starts_with(&target.to_string()))
        .expect("damaged object must be packed");
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(
        fields.len(),
        5,
        "fixture: {target} must be stored whole: {line}"
    );
    let (in_pack, offset): (usize, usize) =
        (fields[3].parse().unwrap(), fields[4].parse().unwrap());
    writable(&pack);
    let mut bytes = fs::read(&pack).unwrap();
    bytes[offset + in_pack / 2 + 1] ^= 0x55;
    fs::write(&pack, bytes).unwrap();
}

fn fsck_fails(root: &Path) {
    let output = git_raw(root, &["fsck", "--full", "--no-dangling"], b"");
    assert!(!output.status.success(), "git fsck must reject the fixture");
}

/// One commit whose tree holds a large blob and a subtree, packed whole by Git.
fn packed_history() -> (tempfile::TempDir, PathBuf, ObjectId, ObjectId, ObjectId) {
    let dir = tempfile::tempdir().unwrap();
    let root = bare(dir.path());
    let content: String = (0..400)
        .map(|i| format!("line {i:04} of a blob that is stored whole\n"))
        .collect();
    let blob = id(&git(
        &root,
        &["hash-object", "-w", "--stdin"],
        content.as_bytes(),
    ));
    let small = id(&git(&root, &["hash-object", "-w", "--stdin"], b"small\n"));
    let subtree = id(&git(
        &root,
        &["mktree"],
        format!("100644 blob {small}\tleaf\n").as_bytes(),
    ));
    let tree = id(&git(
        &root,
        &["mktree"],
        format!("100644 blob {blob}\tbig\n040000 tree {subtree}\tdir\n").as_bytes(),
    ));
    let commit = id(&git(
        &root,
        &["commit-tree", "-m", "one", &tree.to_string()],
        b"",
    ));
    git(
        &root,
        &["update-ref", "refs/heads/main", &commit.to_string()],
        b"",
    );
    git(
        &root,
        &["repack", "-a", "-d", "-q", "--depth=0", "--window=0"],
        b"",
    );
    git(&root, &["prune-packed"], b"");
    (dir, root, blob, subtree, commit)
}

fn read(root: &Path, id: ObjectId) -> Result<Vec<u8>, String> {
    let repo = Repository::open(root).unwrap();
    let objects = repo
        .objects(PackLimits::trusted())
        .map_err(|e| e.to_string())?;
    objects
        .read(id, ReadLimits::trusted())
        .map_err(|e| e.to_string())
        .map(|object| object.unwrap().into_data())
}

fn verify_packs(root: &Path) -> Result<(), String> {
    let repo = Repository::open(root).unwrap();
    let objects = repo.objects(PackLimits::trusted()).unwrap();
    objects
        .verify_packs(&AtomicBool::new(false))
        .map_err(|e| e.to_string())
}

/// Runs every destructive maintenance entry point and checks that none deletes or publishes.
///
/// Steps that copy objects must refuse. When `walk_detects`, the damage is in an object the
/// reachability walk parses, so prune and gc must refuse too; otherwise they need not read it.
fn destructive_operations_refuse(root: &Path, walk_detects: bool) {
    let before = object_files(root);
    let repo = Repository::open(root).unwrap();
    let cancel = AtomicBool::new(false);
    assert!(
        repo.repack_retained(&policy(), RepackLimits::trusted(), &cancel)
            .is_err(),
        "repack must not rewrite unverified content"
    );
    assert_eq!(object_files(root), before, "repack must publish nothing");
    assert!(
        repo.retire_old_packs(
            &mut FixtureIsolation,
            &policy(),
            RepackLimits::trusted(),
            &cancel
        )
        .is_err()
    );
    let pruned = repo.prune_unreachable_loose(&mut FixtureIsolation, &policy(), &cancel);
    assert!(
        pruned.is_err() || pruned.unwrap().deleted.is_empty(),
        "prune must refuse or prove there is nothing to delete"
    );
    let collected = repo.collect_garbage(&mut FixtureIsolation, &policy(), &cancel);
    assert!(collected.is_err() || !walk_detects, "gc must refuse");
    assert!(
        repo.run_maintenance(
            &mut FixtureIsolation,
            &policy(),
            RepackLimits::trusted(),
            &cancel
        )
        .is_err()
    );
    assert_eq!(object_files(root), before, "no maintenance step may delete");
}

/// Integrity regression test: a damaged blob reached only by walks is still reported by content
/// reads and explicit verification, and no maintenance step copies or deletes past it.
#[test]
fn damaged_blob_is_reported_by_reads_verification_and_maintenance() {
    let (_dir, root, blob, _, _) = packed_history();
    damage_entry(&root, blob);
    fsck_fails(&root);
    assert!(read(&root, blob).is_err(), "content read must fail");
    assert!(
        verify_packs(&root).is_err(),
        "explicit verification must fail"
    );
    destructive_operations_refuse(&root, false);
}

/// Integrity regression test: a damaged tree entry fails every reader that parses it.
#[test]
fn damaged_tree_is_reported_by_reads_walks_verification_and_maintenance() {
    let (_dir, root, _, subtree, _) = packed_history();
    damage_entry(&root, subtree);
    fsck_fails(&root);
    assert!(read(&root, subtree).is_err(), "content read must fail");
    assert!(
        verify_packs(&root).is_err(),
        "explicit verification must fail"
    );
    let plan = Repository::open(&root)
        .unwrap()
        .plan_retention(&policy(), &AtomicBool::new(false));
    assert!(
        !plan.is_complete(),
        "the walk parses trees, so it must fail"
    );
    destructive_operations_refuse(&root, true);
}

/// Rewrites `from` to `to` in a v2 SHA-1 index and recomputes the index checksum.
///
/// `to` must share `from`'s first byte and sort position so fanout and ordering stay valid. The
/// entry's CRC still matches its bytes, so only an identity hash can detect the mislabel.
fn mislabel_identity(idx: &Path, from: ObjectId, to: ObjectId) {
    writable(idx);
    let mut bytes = fs::read(idx).unwrap();
    assert_eq!(&bytes[..8], b"\xfftOc\x00\x00\x00\x02");
    let table = 8 + 256 * 4;
    let count = u32::from_be_bytes(bytes[table - 4..table].try_into().unwrap()) as usize;
    let position = (0..count)
        .find(|i| bytes[table + i * 20..table + (i + 1) * 20] == *from.as_bytes())
        .expect("mislabelled identity must be indexed");
    bytes[table + position * 20..table + (position + 1) * 20].copy_from_slice(to.as_bytes());
    let end = bytes.len() - 20;
    let checksum = sha1::Sha1::digest(&bytes[..end]);
    bytes[end..].copy_from_slice(&checksum);
    fs::write(idx, bytes).unwrap();
}

/// Builds a repository whose packed tree entry is mislabelled; returns the commit, tree and blob.
///
/// The commit's tree `T` names loose blobs `kept` and `only`. The pack stores the bytes of a tree
/// `T'` naming only `kept`, indexed as `T`. Trusting the label would make `only` unreachable, and
/// prune would delete its only copy.
fn mislabelled_tree() -> (tempfile::TempDir, PathBuf, ObjectId, ObjectId, ObjectId) {
    let format = ObjectFormat::Sha1;
    let dir = tempfile::tempdir().unwrap();
    let root = bare(dir.path());
    let kept = id(&git(&root, &["hash-object", "-w", "--stdin"], b"kept\n"));
    let only = id(&git(
        &root,
        &["hash-object", "-w", "--stdin"],
        b"only via T\n",
    ));
    let tree = id(&git(
        &root,
        &["mktree"],
        format!("100644 blob {kept}\tkept\n100644 blob {only}\tonly\n").as_bytes(),
    ));
    let commit = id(&git(
        &root,
        &["commit-tree", "-m", "one", &tree.to_string()],
        b"",
    ));
    // T' must share T's first byte and sort on the same side of the commit as T.
    let fake = (0u32..)
        .find_map(|nonce| {
            let listing = format!("100644 blob {kept}\tkept{nonce}\n");
            let fake = id(&git(&root, &["mktree"], listing.as_bytes()));
            (fake.as_bytes()[0] == tree.as_bytes()[0] && (fake < commit) == (tree < commit))
                .then_some(fake)
        })
        .unwrap();
    let listing = format!("{commit}\n{fake}\n");
    let output = git_raw(&root, &["pack-objects", "--stdout"], listing.as_bytes());
    assert!(output.status.success());
    let pack = root.join("objects/pack/pack-mislabel.pack");
    fs::write(&pack, &output.stdout).unwrap();
    git(&root, &["index-pack", pack.to_str().unwrap()], b"");
    mislabel_identity(&pack.with_extension("idx"), fake, tree);
    for packed in [commit, tree, fake] {
        let hex = packed.to_string();
        let _ = fs::remove_file(root.join("objects").join(&hex[..2]).join(&hex[2..]));
    }
    git(
        &root,
        &["update-ref", "refs/heads/main", &commit.to_string()],
        b"",
    );
    assert_eq!(format, Repository::open(&root).unwrap().object_format());
    fsck_fails(&root);
    (dir, root, commit, tree, only)
}

/// Integrity regression test: a packed tree whose index entry names different, CRC-valid tree
/// bytes must never let gc treat that tree's real children as unreachable.
#[test]
fn mislabelled_tree_never_authorizes_deletion_or_rewrite() {
    let (_dir, root, _, tree, only) = mislabelled_tree();
    assert!(
        read(&root, tree).is_err(),
        "content read must check identity"
    );
    destructive_operations_refuse(&root, true);
    assert_eq!(read(&root, only).unwrap(), b"only via T\n");
}

/// Integrity regression test: the native local fetch server walks the mislabelled tree without
/// hashing it, but the destination names each received object by hashing it. `T` then arrives as
/// `T'`, and the connectivity check refuses the fetch because `T` is missing.
#[test]
fn native_fetch_of_mislabelled_tree_fails_at_import() {
    let (_dir, root, commit, tree, _) = mislabelled_tree();
    let result = girt::fetch::receive_local(
        &root,
        |_| vec![commit],
        girt::fetch::FetchLimits::default(),
        &AtomicBool::new(false),
        |_| std::ops::ControlFlow::Continue(()),
    );
    let error = result.expect_err("import must reject the mislabelled tree");
    assert!(error.to_string().contains(&tree.to_string()), "{error}");
}
