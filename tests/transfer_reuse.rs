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
