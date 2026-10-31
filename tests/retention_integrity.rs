//! Integrity regression tests for delta reuse, source precedence, and protected packs.
//! Original fixtures use Git-generated packs and checksum-consistent mislabelled index entries.
#![cfg(unix)]
use std::fs;
use std::io::Write;
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

fn policy(heads: Vec<ObjectId>) -> RetentionPolicy {
    RetentionPolicy::trusted()
        .with_heads(heads)
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60))
}

fn hex(id: ObjectId) -> Vec<u8> {
    let text = id.to_string();
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

/// Writes the objects `ids` of `scratch` into `target/objects/pack/<name>.{pack,idx}`.
fn pack_into(scratch: &Path, ids: &[ObjectId], target: &Path, name: &str) -> PathBuf {
    let listing = ids.iter().map(|id| format!("{id}\n")).collect::<String>();
    let output = git_raw(
        scratch,
        &["pack-objects", "--delta-base-offset", "--stdout"],
        listing.as_bytes(),
    );
    assert!(output.status.success());
    let directory = target.join("objects/pack");
    fs::create_dir_all(&directory).unwrap();
    let pack = directory.join(format!("{name}.pack"));
    fs::write(&pack, &output.stdout).unwrap();
    let idx = directory.join(format!("{name}.idx"));
    git(
        target,
        &[
            "index-pack",
            "-o",
            idx.to_str().unwrap(),
            pack.to_str().unwrap(),
        ],
        b"",
    );
    pack
}

/// Rewrites the identity `from` to `to` in a v2 index and recomputes the index checksum.
///
/// `to` must share `from`'s first byte and sort position so fanout and ordering stay valid.
fn mislabel_identity(idx: &Path, from: ObjectId, to: ObjectId) {
    let mut permissions = fs::metadata(idx).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(permissions.mode() | 0o200);
    fs::set_permissions(idx, permissions).unwrap();
    let mut bytes = fs::read(idx).unwrap();
    assert_eq!(&bytes[..8], b"\xfftOc\x00\x00\x00\x02");
    let (from, to) = (hex(from), hex(to));
    let table = 8 + 256 * 4;
    let count = u32::from_be_bytes(bytes[table - 4..table].try_into().unwrap()) as usize;
    let position = (0..count)
        .find(|i| bytes[table + i * 20..table + (i + 1) * 20] == from[..])
        .expect("mislabelled identity must be indexed");
    bytes[table + position * 20..table + (position + 1) * 20].copy_from_slice(&to);
    let end = bytes.len() - 20;
    let checksum = sha1::Sha1::digest(&bytes[..end]);
    bytes[end..].copy_from_slice(&checksum);
    fs::write(idx, bytes).unwrap();
}

fn blob_lines(seed: &str, lines: usize) -> Vec<u8> {
    (0..lines)
        .map(|i| format!("{seed} line {i:04} with enough text to be worth a delta\n"))
        .collect::<String>()
        .into_bytes()
}

fn new_pack(repo: &Path, known: &[&str]) -> PathBuf {
    let mut packs: Vec<PathBuf> = fs::read_dir(repo.join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pack"))
        .filter(|path| {
            let stem = path.file_stem().unwrap().to_str().unwrap();
            !known.contains(&stem)
        })
        .collect();
    assert_eq!(packs.len(), 1, "exactly one new pack expected: {packs:?}");
    packs.pop().unwrap()
}

/// Copies `pack` and its index into a fresh bare repository and returns that repository.
fn isolated_copy(pack: &Path) -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let root = bare(dir.path());
    let directory = root.join("objects/pack");
    fs::create_dir_all(&directory).unwrap();
    fs::copy(pack, directory.join("pack-copy.pack")).unwrap();
    fs::copy(pack.with_extension("idx"), directory.join("pack-copy.idx")).unwrap();
    let repo = Repository::open(&root).unwrap();
    (dir, repo)
}

fn read_from(repo: &Repository, id: ObjectId) -> Result<Option<Vec<u8>>, String> {
    let objects = repo
        .objects_with(PackLimits::trusted())
        .map_err(|e| e.to_string())?;
    objects
        .read_with(id, ReadLimits::trusted())
        .map(|object| object.map(|object| object.data().to_vec()))
        .map_err(|e| e.to_string())
}

fn verify_pack(repo: &Path, idx: &Path) -> Result<(), String> {
    let output = git_raw(repo, &["verify-pack", idx.to_str().unwrap()], b"");
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// Integrity regression test: A mislabelled base must not change the readable content of a copied
/// delta.
#[test]
fn copied_delta_keeps_its_content_when_a_source_base_identity_is_mislabelled() {
    let format = ObjectFormat::Sha1;
    let x_content = blob_lines("genuine", 64);
    let x = ObjectId::for_blob(format, &x_content);
    let x_hex = x.to_string();
    // Search for X' with X's first byte and the same ordering against Y, same length as X.
    let (fake_content, y_content, fake, y) = (0u32..)
        .find_map(|nonce| {
            let seed = format!("fak{nonce:04}");
            let fake_content = blob_lines(&seed, 64);
            assert_eq!(fake_content.len(), x_content.len());
            let y_content = blob_lines(&seed, 60);
            let fake = ObjectId::for_blob(format, &fake_content);
            let y = ObjectId::for_blob(format, &y_content);
            let same_bucket = fake.to_string()[..2] == x_hex[..2];
            let same_order = (fake < y) == (x < y);
            (same_bucket && same_order && fake != x).then_some((fake_content, y_content, fake, y))
        })
        .unwrap();

    let scratch_dir = tempfile::tempdir().unwrap();
    let scratch = bare(scratch_dir.path());
    for content in [&x_content, &fake_content, &y_content] {
        git(&scratch, &["hash-object", "-w", "--stdin"], content);
    }
    let repo_dir = tempfile::tempdir().unwrap();
    let root = bare(repo_dir.path());
    pack_into(&scratch, &[x], &root, "pack-a");
    let pack_b = pack_into(&scratch, &[fake, y], &root, "pack-b");
    let listing = git(
        &root,
        &[
            "verify-pack",
            "-v",
            pack_b.with_extension("idx").to_str().unwrap(),
        ],
        b"",
    );
    assert!(
        listing
            .lines()
            .any(|line| line.starts_with(&y.to_string()) && line.ends_with(&fake.to_string())),
        "fixture: Y must be a delta against X' in pack b:\n{listing}"
    );
    mislabel_identity(&pack_b.with_extension("idx"), fake, x);

    let repo = Repository::open(&root).unwrap();
    let policy = policy(vec![x, y]);
    let cancel = AtomicBool::new(false);
    repo.repack_retained(&policy, RepackLimits::trusted(), &cancel)
        .unwrap();
    let pack = new_pack(&root, &["pack-a", "pack-b"]);
    let live_read = read_from(&Repository::open(&root).unwrap(), y);
    let (published_dir, published) = isolated_copy(&pack);
    let published_read = read_from(&published, y);
    assert_eq!(
        (
            published_read.as_ref().map(|r| r.as_deref()),
            live_read.as_ref().map(|r| r.as_deref())
        ),
        (Ok(Some(&y_content[..])), Ok(Some(&y_content[..]))),
        "Y must keep its content (published pack alone, live repository): {published_read:?} / {live_read:?}"
    );
    assert_eq!(read_from(&published, x), Ok(Some(x_content)));
    // Verify in the isolated copy: the source repository still holds the mislabelled pack, and Git
    // may resolve X from it and report a collision.
    let published_idx = published_dir.path().join("objects/pack/pack-copy.idx");
    verify_pack(published_dir.path(), &published_idx)
        .expect("git verify-pack on the published pack");
}

/// Integrity regression test: A primary loose copy takes precedence over a mislabelled alternate
/// entry.
#[test]
fn repack_prefers_the_primary_copy_over_a_mislabelled_alternate_entry() {
    let format = ObjectFormat::Sha1;
    let x_content = blob_lines("primary", 32);
    let x = ObjectId::for_blob(format, &x_content);
    let x_hex = x.to_string();
    let (fake_content, fake) = (0u32..)
        .find_map(|nonce| {
            let fake_content = blob_lines(&format!("alt{nonce:05}"), 32);
            let fake = ObjectId::for_blob(format, &fake_content);
            (fake.to_string()[..2] == x_hex[..2] && fake != x).then_some((fake_content, fake))
        })
        .unwrap();

    let root_dir = tempfile::tempdir().unwrap();
    let primary_dir = root_dir.path().join("primary");
    fs::create_dir_all(&primary_dir).unwrap();
    let primary = bare(&primary_dir);
    let borrowed_dir = root_dir.path().join("borrowed");
    fs::create_dir_all(&borrowed_dir).unwrap();
    let borrowed = bare(&borrowed_dir);
    git(&primary, &["hash-object", "-w", "--stdin"], &x_content);
    git(&borrowed, &["hash-object", "-w", "--stdin"], &fake_content);
    let pack = pack_into(&borrowed, &[fake], &borrowed, "pack-alt");
    mislabel_identity(&pack.with_extension("idx"), fake, x);
    fs::write(
        primary.join("objects/info/alternates"),
        format!("{}\n", borrowed.join("objects").display()),
    )
    .unwrap();

    let repo = Repository::open(&primary).unwrap();
    assert_eq!(
        read_from(&repo, x),
        Ok(Some(x_content.clone())),
        "readers see the primary copy"
    );
    let cancel = AtomicBool::new(false);
    repo.repack_retained(&policy(vec![x]), RepackLimits::trusted(), &cancel)
        .unwrap();
    let pack = new_pack(&primary, &[]);
    let live = Repository::open(&primary).unwrap();
    let live_read = read_from(&live, x);
    let (_dir, published) = isolated_copy(&pack);
    let published_read = read_from(&published, x);
    assert_eq!(
        (
            published_read.as_ref().map(|r| r.as_deref()),
            live_read.as_ref().map(|r| r.as_deref())
        ),
        (Ok(Some(&x_content[..])), Ok(Some(&x_content[..]))),
        "X must keep the primary store's content (published pack alone, live repository): {published_read:?} / {live_read:?}"
    );
    verify_pack(&primary, &pack.with_extension("idx"))
        .expect("git verify-pack on the published pack");
}

/// Integrity regression test: Retirement reclaims old unreachable objects, matching Git's garbage
/// collection policy.
#[test]
fn retirement_leaves_one_pack_when_the_old_pack_holds_an_unreachable_object() {
    let dir = tempfile::tempdir().unwrap();
    let root = bare(dir.path());
    let tree = git(&root, &["mktree"], b"");
    let commit = git(&root, &["commit-tree", &tree], b"reachable\n");
    git(&root, &["update-ref", "refs/heads/main", &commit], b"");
    let unreachable = git(
        &root,
        &["hash-object", "-w", "--stdin"],
        &blob_lines("gone", 16),
    );
    git(&root, &["update-ref", "refs/heads/tmp", &commit], b"");
    git(&root, &["tag", "keep-u", &unreachable], b"");
    git(&root, &["repack", "-a", "-d"], b"");
    git(&root, &["tag", "-d", "keep-u"], b"");
    git(&root, &["update-ref", "-d", "refs/heads/tmp"], b"");
    let loose: ObjectId = git(&root, &["hash-object", "-w", "--stdin"], b"fresh root\n")
        .parse()
        .unwrap();
    let before: u64 = pack_bytes(&root);

    let repo = Repository::open(&root).unwrap();
    let cancel = AtomicBool::new(false);
    let report = repo
        .retire_old_packs(
            &mut FixtureIsolation,
            &policy(vec![loose]),
            RepackLimits::trusted(),
            &cancel,
        )
        .unwrap();
    assert!(report.published.is_some(), "{report:?}");
    let packs = pack_files(&root);
    let after = pack_bytes(&root);
    assert_eq!(
        packs.len(),
        1,
        "expected one pack after retirement, found {packs:?}; pack bytes before {before}, after {after}"
    );
}

fn pack_files(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root.join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pack"))
        .collect()
}

fn pack_bytes(root: &Path) -> u64 {
    pack_files(root)
        .iter()
        .map(|path| fs::metadata(path).unwrap().len())
        .sum()
}

/// Integrity regression test: Overlapping source chains must remain readable under the default
/// depth limit.
#[test]
fn published_pack_is_readable_by_a_cold_reader_with_default_depth() {
    let dir = tempfile::tempdir().unwrap();
    let root = bare(dir.path());
    let mut contents: Vec<Vec<u8>> = Vec::new();
    let mut ids: Vec<ObjectId> = Vec::new();
    for i in 0..201usize {
        let text: String = (0..200)
            .map(|line| {
                if line < i {
                    format!("changed line {line:03} with enough text to be worth a delta\n")
                } else {
                    format!("chain line {line:03} with enough text to be worth a delta\n")
                }
            })
            .collect();
        contents.push(text.clone().into_bytes());
        ids.push(
            git(&root, &["hash-object", "-w", "--stdin"], text.as_bytes())
                .parse()
                .unwrap(),
        );
    }
    // Object i is 2*i bytes larger than object i-1, so Git roots each chain at the largest object.
    // Pack one (searched first) holds objects 100..=200: object 100 is its deepest leaf. Pack two
    // holds objects 0..=100: object 100 is its root. A copy that takes object 100 from pack one
    // and keeps pack two's deltas joins the two chains.
    let first: Vec<ObjectId> = ids[100..].to_vec();
    let second: Vec<ObjectId> = ids[..=100].to_vec();
    let packs = root.join("objects/pack");
    let write = |list: &[ObjectId], name: &str| {
        let listing = list.iter().map(|id| format!("{id}\n")).collect::<String>();
        let output = git_raw(
            &root,
            &[
                "pack-objects",
                "--delta-base-offset",
                "--depth=100",
                "--window=50",
                "--stdout",
            ],
            listing.as_bytes(),
        );
        assert!(output.status.success());
        let pack = packs.join(format!("{name}.pack"));
        fs::write(&pack, &output.stdout).unwrap();
        git(
            &root,
            &[
                "index-pack",
                "-o",
                pack.with_extension("idx").to_str().unwrap(),
                pack.to_str().unwrap(),
            ],
            b"",
        );
        pack
    };
    let pack_one = write(&first, "pack-one");
    let pack_two = write(&second, "pack-two");
    for id in &ids {
        let path = root
            .join("objects")
            .join(&id.to_string()[..2])
            .join(&id.to_string()[2..]);
        fs::remove_file(path).unwrap();
    }
    let depth = |idx: &Path| -> usize {
        git(&root, &["verify-pack", "-v", idx.to_str().unwrap()], b"")
            .lines()
            .filter_map(|line| {
                line.split_whitespace()
                    .nth(5)
                    .and_then(|d| d.parse::<usize>().ok())
            })
            .max()
            .unwrap_or(0)
    };
    let (d1, d2) = (
        depth(&pack_one.with_extension("idx")),
        depth(&pack_two.with_extension("idx")),
    );
    eprintln!("source chain depths: pack one {d1}, pack two {d2}");

    let repo = Repository::open(&root).unwrap();
    let cancel = AtomicBool::new(false);
    // Default read limits (max_delta_depth 64) both for the repack policy, so a pre-publication
    // depth guard can act, and for the cold reader below.
    let policy = (policy(ids.clone())).with_read(ReadLimits::default());
    repo.repack_retained(&policy, RepackLimits::trusted(), &cancel)
        .unwrap();
    let pack = new_pack(&root, &["pack-one", "pack-two"]);
    let published_depth = depth(&pack.with_extension("idx"));
    eprintln!("published pack max chain depth per git: {published_depth}");
    let (_dir, published) = isolated_copy(&pack);
    for (id, content) in ids.iter().zip(&contents) {
        // A fresh store per read, so no warmed base cache can mask the chain depth.
        let objects = published.objects_with(PackLimits::trusted()).unwrap();
        let read = objects
            .read_with(*id, ReadLimits::default())
            .map_err(|e| e.to_string());
        assert_eq!(
            read.map(|o| o.map(|o| o.data().to_vec())),
            Ok(Some(content.clone())),
            "object {id} from the published pack with default limits"
        );
    }
}

/// Integrity regression test: Changing roots must not multiply the copies held by a protected pack.
#[test]
fn protected_pack_objects_are_not_duplicated_across_maintenance_runs() {
    let dir = tempfile::tempdir().unwrap();
    let root = bare(dir.path());
    let blob = git(
        &root,
        &["hash-object", "-w", "--stdin"],
        &blob_lines("bulk", 2000),
    );
    let tree = git(
        &root,
        &["mktree"],
        format!("100644 blob {blob}\tfile\n").as_bytes(),
    );
    let commit = git(&root, &["commit-tree", &tree], b"root\n");
    git(&root, &["update-ref", "refs/heads/main", &commit], b"");
    git(&root, &["repack", "-a", "-d"], b"");
    let original = pack_bytes(&root);
    let mut policy = RetentionPolicy::trusted()
        .with_recent_cutoff(SystemTime::now() - Duration::from_secs(3600));
    let repo = Repository::open(&root).unwrap();
    let mut copies = Vec::new();
    let mut bytes = Vec::new();
    for run in 0..3 {
        let loose: ObjectId = git(
            &root,
            &["hash-object", "-w", "--stdin"],
            format!("root {run}\n").as_bytes(),
        )
        .parse()
        .unwrap();
        policy.heads.push(loose);
        let report = repo
            .retire_old_packs(
                &mut FixtureIsolation,
                &policy,
                RepackLimits::trusted(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(report.published.is_some(), "run {run}: {report:?}");
        let holding = pack_files(&root)
            .iter()
            .filter(|pack| {
                git(
                    &root,
                    &[
                        "verify-pack",
                        "-v",
                        pack.with_extension("idx").to_str().unwrap(),
                    ],
                    b"",
                )
                .lines()
                .any(|line| line.starts_with(&commit))
            })
            .count();
        copies.push(holding);
        bytes.push(pack_bytes(&root));
    }
    assert_eq!(
        copies,
        [1, 1, 1],
        "copies of the commit per run; total pack bytes per run {bytes:?} against original {original}"
    );
    assert!(
        bytes.iter().all(|&total| total <= original + 8 * 1024),
        "total pack bytes per run {bytes:?} against original {original}"
    );
}
