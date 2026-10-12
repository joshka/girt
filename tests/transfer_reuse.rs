//! Original fixtures generated with Git, with Git as the object and pack fidelity oracle.
use std::ops::ControlFlow;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::fetch::{
    FetchLimits, KnownHistory, LocalFetchProgress, receive_local_with_known_and_progress,
};
use girt::refs::{Expected, RefName, Target};
use girt::transport::TransportControl;
use girt::{InitKind, ObjectFormat, PackLimits, Repository};
use rstest::rstest;

fn make_fixture_file_writable(path: &Path) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(windows)]
    {
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "Windows clears its read-only attribute; Unix sets owner-write above"
        )]
        permissions.set_readonly(false);
    }
    std::fs::set_permissions(path, permissions).unwrap();
}

fn git(path: &Path, args: &[&str]) -> Vec<u8> {
    let path = path.to_string_lossy();
    let path: &str = &path;
    #[cfg(windows)]
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    #[cfg(windows)]
    let args: Vec<&str> = args
        .iter()
        .map(|arg| arg.strip_prefix(r"\\?\").unwrap_or(arg))
        .collect();
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .arg("-C")
        .arg(path)
        .args(args.iter())
        .env("GIT_CONFIG_GLOBAL", Path::new(path).join("absent-config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Transfer Fixture")
        .env("GIT_AUTHOR_EMAIL", "transfer@example.invalid")
        .env("GIT_COMMITTER_NAME", "Transfer Fixture")
        .env("GIT_COMMITTER_EMAIL", "transfer@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn fixture(format: ObjectFormat) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &["init", "-b", "main", &format!("--object-format={format}")],
    );
    // Similar, independently written versions cause Git to produce real delta chains.
    let base = "original fixture line\n".repeat(2048);
    for n in 0..100 {
        std::fs::write(root.path().join("file"), format!("{base}version {n}\n")).unwrap();
        git(root.path(), &["add", "file"]);
        git(root.path(), &["commit", "-qm", &format!("version {n}")]);
    }
    git(
        root.path(),
        &["repack", "-adf", "--depth=12", "--window=100"],
    );
    root
}

fn tip(path: &Path) -> girt::ObjectId {
    String::from_utf8(git(path, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn receive(path: &Path, known: &KnownHistory) -> (girt::fetch::ReceivedFetch, u64) {
    let mut reads = 0;
    let received = receive_local_with_known_and_progress(
        path,
        |_| vec![tip(path)],
        known,
        FetchLimits::trusted(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |event| {
            if let LocalFetchProgress::Reading { objects } = event {
                reads = objects;
            }
        },
    )
    .unwrap();
    (received, reads)
}

fn publish(received: &girt::fetch::ReceivedFetch, dest: &Repository, id: girt::ObjectId) {
    received
        .install(dest, PackLimits::trusted(), &AtomicBool::new(false))
        .unwrap();
    dest.references()
        .unwrap()
        .update_without_reflog(
            &RefName::new("refs/heads/main").unwrap(),
            Target::Direct(id),
            Expected::Any,
        )
        .unwrap();
    git(dest.git_dir(), &["fsck", "--strict"]);
}

fn object_set(path: &Path) -> Vec<String> {
    let bytes = git(path, &["cat-file", "--batch-all-objects", "--batch-check"]);
    let mut lines: Vec<_> = String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

fn delta_count(path: &Path) -> usize {
    let pack_dir = path.join("objects/pack");
    std::fs::read_dir(pack_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "idx"))
        .map(|idx| {
            let output = git(path, &["verify-pack", "-v", idx.to_str().unwrap()]);
            String::from_utf8(output)
                .unwrap()
                .lines()
                .filter(|line| line.split_whitespace().count() == 7)
                .count()
        })
        .sum()
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn local_clone_pack_reuses_source_deltas_and_matches_git(#[case] format: ObjectFormat) {
    let source = fixture(format);
    let root = tempfile::tempdir().unwrap();
    let oracle = root.path().join("git.git");
    git(
        root.path(),
        &[
            "clone",
            "--bare",
            "--no-local",
            source.path().to_str().unwrap(),
            oracle.to_str().unwrap(),
        ],
    );
    let dest = Repository::init(format, root.path().join("girt.git"), InitKind::Bare).unwrap();
    let (received, _) = receive(source.path(), &KnownHistory::default());
    publish(&received, &dest, tip(source.path()));
    assert_eq!(object_set(dest.git_dir()), object_set(&oracle));
    assert!(
        delta_count(dest.git_dir()) > 0,
        "transfer must keep source deltas"
    );
    let pack = std::fs::read_dir(dest.object_dir().join("pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "pack"))
        .unwrap();
    let verification_index = root.path().join("verified.idx");
    git(
        dest.git_dir(),
        &[
            "index-pack",
            "--strict",
            "-o",
            verification_index.to_str().unwrap(),
            pack.to_str().unwrap(),
        ],
    );
}

#[test]
fn local_noop_fetch_reads_no_objects() {
    let source = fixture(ObjectFormat::Sha1);
    let repo = Repository::open(source.path()).unwrap();
    let known = KnownHistory::from_store(
        repo.objects(PackLimits::trusted()).unwrap(),
        &[tip(source.path())],
        FetchLimits::trusted(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let (received, reads) = receive(source.path(), &known);
    assert_eq!(received.pack_bytes(), 0);
    assert_eq!(reads, 0);
}

#[test]
fn local_fetch_of_one_commit_reads_only_new_closure() {
    let source = fixture(ObjectFormat::Sha1);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("dest.git");
    git(
        root.path(),
        &[
            "clone",
            "--bare",
            source.path().to_str().unwrap(),
            path.to_str().unwrap(),
        ],
    );
    let dest = Repository::open(&path).unwrap();
    let known = KnownHistory::from_store(
        dest.objects(PackLimits::trusted()).unwrap(),
        &[tip(&path)],
        FetchLimits::trusted(),
        &AtomicBool::new(false),
    )
    .unwrap();
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
    let dest = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("dest.git"),
        InitKind::Bare,
    )
    .unwrap();
    let unrelated = dest
        .loose_objects()
        .write_blob(b"destination only")
        .unwrap();
    let known = KnownHistory::new_local(
        &dest.objects(PackLimits::trusted()).unwrap(),
        &[unrelated],
        FetchLimits::trusted(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let (received, _) = receive(source.path(), &known);
    assert_eq!(received.object_count(), 300);
    publish(&received, &dest, tip(source.path()));
    git(dest.git_dir(), &["fsck", "--connectivity-only"]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn push_reuses_deltas_and_reconstructs_a_delta_without_its_base(#[case] format: ObjectFormat) {
    use girt::push::{ForcePolicy, PreparedPush, PushCommand, PushLimits, send_local};
    let source = fixture(format);
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::open(source.path()).unwrap();
    let objects = repo.objects(PackLimits::trusted()).unwrap();
    let dest = Repository::init(format, root.path().join("full.git"), InitKind::Bare).unwrap();
    let full = PreparedPush::new_local(
        &objects,
        vec![PushCommand {
            name: RefName::new("refs/heads/main").unwrap(),
            expected: None,
            new: tip(source.path()),
            force: ForcePolicy::FastForwardOnly,
        }],
        &[],
        PushLimits::trusted(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(
        send_local(dest.git_dir(), &full, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    assert!(delta_count(dest.git_dir()) > 0);
    assert_eq!(
        object_set(dest.git_dir()),
        object_set(&source.path().join(".git"))
    );
    git(dest.git_dir(), &["fsck", "--strict"]);

    let id: girt::ObjectId = String::from_utf8(git(source.path(), &["rev-parse", "HEAD~50:file"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let subset = Repository::init(format, root.path().join("subset.git"), InitKind::Bare).unwrap();
    let one = PreparedPush::new_local(
        &objects,
        vec![PushCommand {
            name: RefName::new("refs/tags/blob").unwrap(),
            expected: None,
            new: id,
            force: ForcePolicy::FastForwardOnly,
        }],
        &[],
        PushLimits::trusted(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(one.object_count(), 1);
    assert!(
        send_local(subset.git_dir(), &one, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(delta_count(subset.git_dir()), 0);
    assert_eq!(
        git(subset.git_dir(), &["cat-file", "blob", &id.to_string()]),
        git(source.path(), &["cat-file", "blob", &id.to_string()])
    );
    git(subset.git_dir(), &["fsck", "--strict"]);
}

#[test]
fn local_transfer_reencodes_chains_for_the_receiver_depth_limit() {
    let source = fixture(ObjectFormat::Sha1);
    let root = tempfile::tempdir().unwrap();
    let dest = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("dest.git"),
        InitKind::Bare,
    )
    .unwrap();
    let received = receive_local_with_known_and_progress(
        source.path(),
        |_| vec![tip(source.path())],
        &KnownHistory::default(),
        FetchLimits {
            max_delta_depth: 0,
            ..FetchLimits::trusted()
        },
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |_| {},
    )
    .unwrap();
    publish(&received, &dest, tip(source.path()));
    assert_eq!(delta_count(dest.git_dir()), 0);
    assert_eq!(
        object_set(dest.git_dir()),
        object_set(&source.path().join(".git"))
    );
}

#[test]
fn source_index_identity_is_not_trusted_for_a_reused_whole_object() {
    use sha1::Digest as _;
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--bare"]);
    let id = String::from_utf8(git(root.path(), &["hash-object", "-w", "--stdin"])).unwrap();
    // Git hashes empty stdin; advertise the blob in a non-branch namespace.
    git(root.path(), &["update-ref", "refs/for/blob", id.trim()]);
    git(root.path(), &["repack", "-ad"]);
    let idx = std::fs::read_dir(root.path().join("objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "idx"))
        .unwrap();
    let false_id = girt::ObjectId::for_blob(ObjectFormat::Sha1, b"false identity");
    let mut index = std::fs::read(&idx).unwrap();
    // A one-object v2 index with a different sorted identity and matching fanout/checksum.
    index[1032..1052].copy_from_slice(false_id.as_bytes());
    for n in 0..256 {
        let count = u32::from(n >= usize::from(false_id.as_bytes()[0]));
        index[8 + n * 4..12 + n * 4].copy_from_slice(&count.to_be_bytes());
    }
    let end = index.len() - 20;
    let hash = sha1::Sha1::digest(&index[..end]);
    index[end..].copy_from_slice(&hash);
    // Git marks indexes read-only; this disposable fixture deliberately corrupts one.
    make_fixture_file_writable(&idx);
    std::fs::write(idx, index).unwrap();
    std::fs::write(root.path().join("refs/for/blob"), format!("{false_id}\n")).unwrap();
    let result = receive_local_with_known_and_progress(
        root.path(),
        |_| vec![false_id],
        &KnownHistory::default(),
        FetchLimits::trusted(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |_| {},
    );
    assert!(
        matches!(result, Err(girt::fetch::FetchError::Missing(missing)) if missing == false_id)
    );
}

#[test]
fn legacy_index_transfer_keeps_git_fidelity_without_crc_reuse() {
    let source = fixture(ObjectFormat::Sha1);
    let pack = std::fs::read_dir(source.path().join(".git/objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "pack"))
        .unwrap();
    let old_index = pack.with_extension("idx");
    let replacement = pack.with_extension("legacy.idx");
    git(
        source.path(),
        &[
            "index-pack",
            "--index-version=1",
            "-o",
            replacement.to_str().unwrap(),
            pack.to_str().unwrap(),
        ],
    );
    make_fixture_file_writable(&old_index);
    std::fs::remove_file(&old_index).unwrap();
    std::fs::rename(replacement, old_index).unwrap();
    let root = tempfile::tempdir().unwrap();
    let dest = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("dest.git"),
        InitKind::Bare,
    )
    .unwrap();
    let (received, _) = receive(source.path(), &KnownHistory::default());
    publish(&received, &dest, tip(source.path()));
    assert_eq!(
        object_set(dest.git_dir()),
        object_set(&source.path().join(".git"))
    );
    assert_eq!(delta_count(dest.git_dir()), 0);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1, false, false)]
#[case::sha256(ObjectFormat::Sha256, false, false)]
#[case::retained_sha1(ObjectFormat::Sha1, true, false)]
#[case::retained_sha256(ObjectFormat::Sha256, true, false)]
#[case::configured_sha1(ObjectFormat::Sha1, false, true)]
#[case::configured_sha256(ObjectFormat::Sha256, false, true)]
fn unreferenced_commit_does_not_prove_complete_history(
    #[case] format: ObjectFormat,
    #[case] retained: bool,
    #[case] configured: bool,
) {
    let source = fixture(format);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("dest.git");
    git(
        root.path(),
        &[
            "clone",
            "--bare",
            source.path().to_str().unwrap(),
            path.to_str().unwrap(),
        ],
    );
    let old = tip(&path);
    std::fs::write(source.path().join("file"), b"interrupted fetch content\n").unwrap();
    git(source.path(), &["add", "file"]);
    git(source.path(), &["commit", "-qm", "interrupted fetch"]);
    let new = tip(source.path());
    let commit = git(source.path(), &["cat-file", "commit", &new.to_string()]);
    let commit_path = root.path().join("commit");
    std::fs::write(&commit_path, commit).unwrap();
    // An interrupted unpack can leave a commit without its tree and blob, outside every ref.
    let stored = git(
        &path,
        &[
            "hash-object",
            "-w",
            "-t",
            "commit",
            "--literally",
            commit_path.to_str().unwrap(),
        ],
    );
    assert_eq!(String::from_utf8(stored).unwrap().trim(), new.to_string());
    assert_eq!(tip(&path), old);
    let dest = Repository::open(&path).unwrap();
    if configured {
        let options = girt::transfer::FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]);
        dest.fetch(
            "origin",
            &options,
            &girt::transfer::Environment::from_process(),
            &mut girt::transfer::NoCallbacks,
        )
        .unwrap();
        git(&path, &["fsck", "--strict"]);
    } else {
        let known = KnownHistory::from_store(
            dest.objects(PackLimits::trusted()).unwrap(),
            &[old],
            FetchLimits::trusted(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let known = if retained {
            known.only_when_all_wants_known()
        } else {
            known
        };
        let (received, _) = receive(source.path(), &known);
        assert_eq!(
            received.object_count(),
            if retained {
                object_set(&source.path().join(".git")).len()
            } else {
                2
            }
        );
        publish(&received, &dest, new);
    }
    assert_eq!(object_set(&path), object_set(&source.path().join(".git")));
}

fn pack_path(source: &Path) -> std::path::PathBuf {
    std::fs::read_dir(source.join(".git/objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "pack")
        })
        .unwrap()
}

fn pack_rows(source: &Path, pack: &Path) -> Vec<Vec<String>> {
    let index = pack.with_extension("idx");
    String::from_utf8(git(source, &["verify-pack", "-v", index.to_str().unwrap()]))
        .unwrap()
        .lines()
        .map(|line| line.split_whitespace().map(str::to_owned).collect())
        .collect()
}

fn mislabel_index_entry(index: &Path, old: girt::ObjectId, replacement: girt::ObjectId) {
    use sha1::Digest as _;
    let mut bytes = std::fs::read(index).unwrap();
    let width = old.format().digest_len();
    let count = u32::from_be_bytes(bytes[1028..1032].try_into().unwrap()) as usize;
    let crc_start = 1032 + count * width;
    let offset_start = crc_start + count * 4;
    let mut rows: Vec<_> = (0..count)
        .map(|n| {
            let mut id = bytes[1032 + n * width..1032 + (n + 1) * width].to_vec();
            if id == old.as_bytes() {
                id = replacement.as_bytes().to_vec();
            }
            (
                id,
                bytes[crc_start + n * 4..crc_start + (n + 1) * 4].to_vec(),
                bytes[offset_start + n * 4..offset_start + (n + 1) * 4].to_vec(),
            )
        })
        .collect();
    rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut fanout = [0u32; 256];
    for (id, _, _) in &rows {
        fanout[usize::from(id[0])] += 1;
    }
    let mut sum = 0;
    for (n, count) in fanout.into_iter().enumerate() {
        sum += count;
        bytes[8 + n * 4..12 + n * 4].copy_from_slice(&sum.to_be_bytes());
    }
    for (n, (id, crc, offset)) in rows.iter().enumerate() {
        bytes[1032 + n * width..1032 + (n + 1) * width].copy_from_slice(id);
        bytes[crc_start + n * 4..crc_start + (n + 1) * 4].copy_from_slice(crc);
        bytes[offset_start + n * 4..offset_start + (n + 1) * 4].copy_from_slice(offset);
    }
    let end = bytes.len() - width;
    let digest = match old.format() {
        ObjectFormat::Sha1 => sha1::Sha1::digest(&bytes[..end]).to_vec(),
        ObjectFormat::Sha256 => sha2::Sha256::digest(&bytes[..end]).to_vec(),
    };
    bytes[end..].copy_from_slice(&digest);
    make_fixture_file_writable(index);
    std::fs::write(index, bytes).unwrap();
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn mislabelled_delta_base_cannot_publish_even_when_destination_has_that_id(
    #[case] format: ObjectFormat,
) {
    let source = fixture(format);
    let pack = pack_path(source.path());
    let rows = pack_rows(source.path(), &pack);
    let delta = rows
        .iter()
        .find(|row| row.len() == 7 && row[1] == "blob" && row[5] == "1")
        .unwrap();
    let target: girt::ObjectId = delta[0].parse().unwrap();
    let base: girt::ObjectId = delta[6].parse().unwrap();
    let mut unrelated = git(source.path(), &["cat-file", "blob", &base.to_string()]);
    unrelated[0] ^= 0xff;
    let root = tempfile::tempdir().unwrap();
    let dest = Repository::init(format, root.path().join("dest.git"), InitKind::Bare).unwrap();
    let false_base = dest.loose_objects().write_blob(&unrelated).unwrap();
    let before = object_set(dest.git_dir());
    git(
        source.path(),
        &["update-ref", "refs/for/delta", &target.to_string()],
    );
    mislabel_index_entry(&pack.with_extension("idx"), base, false_base);
    std::fs::create_dir_all(source.path().join(".git/refs/for")).unwrap();
    std::fs::write(
        source.path().join(".git/refs/for/base"),
        format!("{false_base}\n"),
    )
    .unwrap();
    let known = KnownHistory::from_store(
        dest.objects(PackLimits::trusted()).unwrap(),
        &[],
        FetchLimits::trusted(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let mut packed = 0;
    let result = receive_local_with_known_and_progress(
        source.path(),
        |_| vec![false_base, target],
        &known,
        FetchLimits {
            max_haves: 0,
            ..FetchLimits::trusted()
        },
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |event| {
            if let LocalFetchProgress::Packing { objects: (done, _) } = event {
                packed = done;
            }
        },
    );
    assert_eq!(
        packed, 2,
        "both entries must reach the receiver's independent validation"
    );
    assert!(matches!(result, Err(girt::fetch::FetchError::Missing(missing)) if missing == target));
    assert_eq!(object_set(dest.git_dir()), before);
    assert!(dest.references().unwrap().list().unwrap().is_empty());
    git(dest.git_dir(), &["fsck", "--strict"]);
}

#[rstest]
#[case::fetch_sha1(ObjectFormat::Sha1, false)]
#[case::fetch_sha256(ObjectFormat::Sha256, false)]
#[case::push_sha1(ObjectFormat::Sha1, true)]
#[case::push_sha256(ObjectFormat::Sha256, true)]
fn damaged_source_entry_cannot_change_destination(
    #[case] format: ObjectFormat,
    #[case] push: bool,
) {
    use std::io::{Seek as _, Write as _};
    let source = fixture(format);
    let pack = pack_path(source.path());
    let rows = pack_rows(source.path(), &pack);
    let ordinary = rows
        .iter()
        .find(|row| row.len() == 5 && row[1] == "blob")
        .unwrap();
    let id: girt::ObjectId = ordinary[0].parse().unwrap();
    git(
        source.path(),
        &["update-ref", "refs/for/blob", &id.to_string()],
    );
    let root = tempfile::tempdir().unwrap();
    let dest = Repository::init(format, root.path().join("dest.git"), InitKind::Bare).unwrap();
    let sentinel = dest
        .loose_objects()
        .write_blob(b"keep destination unchanged")
        .unwrap();
    let reference = RefName::new("refs/for/sentinel").unwrap();
    dest.references()
        .unwrap()
        .update_without_reflog(&reference, Target::Direct(sentinel), Expected::Any)
        .unwrap();
    let before = object_set(dest.git_dir());
    git(
        source.path(),
        &[
            "config",
            "remote.origin.url",
            dest.git_dir().to_str().unwrap(),
        ],
    );
    let offset = ordinary[4].parse::<u64>().unwrap() + ordinary[3].parse::<u64>().unwrap() - 1;
    let bytes = std::fs::read(&pack).unwrap();
    make_fixture_file_writable(&pack);
    let mut file = std::fs::OpenOptions::new().write(true).open(&pack).unwrap();
    file.seek(std::io::SeekFrom::Start(offset)).unwrap();
    file.write_all(&[bytes[offset as usize] ^ 128]).unwrap();
    drop(file);
    if push {
        let repo = Repository::open(source.path()).unwrap();
        let command = girt::push::PushCommand {
            name: RefName::new("refs/for/blob").unwrap(),
            expected: None,
            new: id,
            force: girt::push::ForcePolicy::Allow,
        };
        let error = repo
            .push(
                "origin",
                vec![command],
                &girt::transfer::PushOptions::default(),
                &girt::transfer::Environment::from_process(),
                &mut girt::transfer::NoCallbacks,
            )
            .unwrap_err();
        let girt::transfer::TransferError::Push(error) = error else {
            panic!("{error:?}")
        };
        assert!(
            matches!(*error, girt::push::PushError::NotSent(girt::push::PushFailure::Read {
            id: damaged, source: girt::ObjectReadError::Corrupt("entry CRC")
        }) if damaged == id)
        );
    } else {
        let result = receive_local_with_known_and_progress(
            source.path(),
            |_| vec![id],
            &KnownHistory::default(),
            FetchLimits::trusted(),
            TransportControl::new(&AtomicBool::new(false)),
            |_| ControlFlow::Continue(()),
            |_| {},
        );
        assert!(matches!(
            result,
            Err(girt::fetch::FetchError::LocalRead {
                source: girt::ObjectReadError::Corrupt("entry CRC"),
                ..
            })
        ));
    }
    assert_eq!(object_set(dest.git_dir()), before);
    assert_eq!(dest.references().unwrap().list().unwrap().len(), 1);
    assert_eq!(
        dest.references().unwrap().read(&reference).unwrap(),
        Some(Target::Direct(sentinel))
    );
    git(dest.git_dir(), &["fsck", "--strict"]);
}
