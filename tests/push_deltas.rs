//! Checks that pushes delta-compress against advertised bases and that Git accepts the result.

#[cfg(feature = "http")]
#[path = "support/http_git.rs"]
mod http_git;
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
#[path = "support/stand_in.rs"]
mod stand_in;

// Original Git-generated large-blob edits exercise push delta selection and receiver import.
use std::io::{Read, Write};
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
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
    let retained = text(git(
        path,
        &["hash-object", "-w", "--stdin"],
        b"retained tree entry\n",
    ));
    let mut entries = format!("100644 blob {blob}\tlarge.bin\n");
    for n in 0..16 {
        entries.push_str(&format!("100644 blob {retained}\tretained-{n:02}.bin\n"));
    }
    let tree = text(git(path, &["mktree"], entries.as_bytes()));
    let mut args = vec!["commit-tree", &tree];
    let parent = parent.map(|id| id.to_string());
    if let Some(parent) = &parent {
        args.extend(["-p", parent]);
    }
    let id: ObjectId = text(git(path, &args, b"original delta fixture\n"))
        .parse()
        .unwrap();
    git(
        path,
        &["update-ref", "refs/heads/main", &id.to_string()],
        &[],
    );
    (id, blob)
}

fn fixture(format: ObjectFormat) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.git");
    let dest = root.path().join("dest.git");
    git(
        root.path(),
        &[
            "init",
            "-q",
            "--bare",
            &format!("--object-format={format}"),
            source.to_str().unwrap(),
        ],
        &[],
    );
    let mut data = payload();
    let (old, _) = commit(&source, &data, None);
    git(
        root.path(),
        &[
            "clone",
            "-q",
            "--bare",
            "--no-hardlinks",
            source.to_str().unwrap(),
            dest.to_str().unwrap(),
        ],
        &[],
    );
    data[262144..262152].copy_from_slice(b"EDITED!!");
    let (new, blob) = commit(&source, &data, Some(old));
    Fixture {
        _root: root,
        source,
        dest,
        old,
        new,
        data,
        blob,
    }
}

fn prepare(f: &Fixture, roots: &[ObjectId], depth: usize) -> PreparedPush {
    let mut limits = PushLimits::default();
    limits.read.max_delta_depth = depth;
    prepare_with_limits(f, roots, limits)
}

fn prepare_with_limits(f: &Fixture, roots: &[ObjectId], limits: PushLimits) -> PreparedPush {
    let repo = Repository::open(&f.source).unwrap();
    PreparedPush::new_local(
        &repo.objects(PackLimits::trusted()).unwrap(),
        vec![
            PushCommand::new(RefName::new("refs/heads/main").unwrap(), Some(f.old), f.new)
                .with_force(ForcePolicy::FastForwardOnly),
        ],
        roots,
        limits,
        &AtomicBool::new(false),
    )
    .unwrap()
}

fn publish(f: &Fixture, prepared: &PreparedPush) {
    let report = girt::push::send_local(&f.dest, prepared, &AtomicBool::new(false)).unwrap();
    assert!(report.all_succeeded());
    assert_eq!(git(&f.dest, &["cat-file", "blob", &f.blob], &[]), f.data);
    assert_eq!(
        text(git(&f.dest, &["rev-parse", "refs/heads/main"], &[])),
        f.new.to_string()
    );
    git(&f.dest, &["fsck", "--strict"], &[]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn small_edit_uses_advertised_base_and_matches_git_budget(#[case] format: ObjectFormat) {
    let f = fixture(format);
    let oracle = git(
        &f.source,
        &["pack-objects", "--stdout", "--revs", "--thin"],
        format!("{}\n^{}\n", f.new, f.old).as_bytes(),
    );
    let prepared = prepare(&f, &[f.old], 1);
    assert!(
        prepared.pack_bytes() < f.data.len() / 20,
        "pack bytes: {}",
        prepared.pack_bytes()
    );
    assert!(
        prepared.pack_bytes() <= oracle.len() * 2,
        "girt={} Git={}",
        prepared.pack_bytes(),
        oracle.len()
    );
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

fn packet(output: &mut Vec<u8>, payload: &[u8]) {
    output.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    output.extend_from_slice(payload);
}

/// Capture caller-owned wire output without asking girt's reader to interpret its own pack.
fn wire_pack(f: &Fixture, prepared: &PreparedPush, ofs: bool) -> Vec<u8> {
    let format = f.old.format();
    let caps = if ofs {
        "report-status ofs-delta"
    } else {
        "report-status"
    };
    let mut response = Vec::new();
    packet(
        &mut response,
        format!("{} refs/heads/main\0{caps} object-format={format}\n", f.old).as_bytes(),
    );
    response.extend_from_slice(b"0000");
    packet(&mut response, b"unpack ok\n");
    packet(&mut response, b"ok refs/heads/main\n");
    response.extend_from_slice(b"0000");
    let mut sent = Vec::new();
    let report = girt::push::send(
        &mut response.as_slice(),
        &mut sent,
        prepared,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(report.all_succeeded());
    let start = sent.windows(4).position(|bytes| bytes == b"PACK").unwrap();
    sent[start..].to_vec()
}

/// Inspect only framing using the documented pack format and an independent zlib decoder.
fn entry_codes(pack: &[u8], digest_len: usize) -> Vec<u8> {
    let count = u32::from_be_bytes(pack[8..12].try_into().unwrap());
    let mut cursor = std::io::Cursor::new(&pack[12..pack.len() - digest_len]);
    let mut codes = Vec::new();
    for _ in 0..count {
        let mut byte = [0];
        cursor.read_exact(&mut byte).unwrap();
        let code = (byte[0] >> 4) & 7;
        codes.push(code);
        while byte[0] & 128 != 0 {
            cursor.read_exact(&mut byte).unwrap();
        }
        match code {
            7 => cursor.set_position(cursor.position() + digest_len as u64),
            6 => {
                cursor.read_exact(&mut byte).unwrap();
                while byte[0] & 128 != 0 {
                    cursor.read_exact(&mut byte).unwrap();
                }
            }
            _ => {}
        }
        let mut decoder = flate2::bufread::ZlibDecoder::new(&mut cursor);
        std::io::copy(&mut decoder, &mut std::io::sink()).unwrap();
    }
    codes
}

#[rstest]
#[case::sha1_ref(ObjectFormat::Sha1, false)]
#[case::sha1_ofs_capability(ObjectFormat::Sha1, true)]
#[case::sha256_ref(ObjectFormat::Sha256, false)]
#[case::sha256_ofs_capability(ObjectFormat::Sha256, true)]
fn thin_pack_uses_ref_framing_and_git_fixes_it(#[case] format: ObjectFormat, #[case] ofs: bool) {
    let f = fixture(format);
    let prepared = prepare(&f, &[f.old], 1);
    let bytes = wire_pack(&f, &prepared, ofs);
    let codes = entry_codes(&bytes, format.digest_len());
    assert_eq!(codes.iter().filter(|&&code| code == 7).count(), 2);
    assert!(!codes.contains(&6));
    git(
        &f.dest,
        &["index-pack", "--stdin", "--fix-thin", "--strict"],
        &bytes,
    );
    git(
        &f.dest,
        &["update-ref", "refs/heads/main", &f.new.to_string()],
        &[],
    );
    assert_eq!(git(&f.dest, &["cat-file", "blob", &f.blob], &[]), f.data);
    git(&f.dest, &["fsck", "--strict"], &[]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn packed_edit_with_excluded_base_still_uses_thin_delta(#[case] format: ObjectFormat) {
    let f = fixture(format);
    git(&f.source, &["repack", "-ad"], &[]);
    let prepared = prepare(&f, &[f.old], 1);
    assert!(prepared.pack_bytes() < f.data.len() / 20);
    publish(&f, &prepared);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn packed_whole_search_fallback_preserves_compressed_entries(#[case] format: ObjectFormat) {
    let f = fixture(format);
    git(&f.source, &["repack", "-ad", "--window=0"], &[]);
    let ordinary = prepare_with_limits(
        &f,
        &[f.old],
        PushLimits::default().with_compression(girt::PackCompression::Ordinary),
    );
    let bounded = prepare_with_limits(
        &f,
        &[f.old],
        PushLimits::default().with_compression(girt::PackCompression::Delta(
            girt::DeltaOptions::default().with_max_work(1),
        )),
    );
    assert_eq!(
        wire_pack(&f, &bounded, false),
        wire_pack(&f, &ordinary, false)
    );
    publish(&f, &bounded);
}

#[rstest]
#[case::input_budget(PushLimits::default().with_pack(girt::PackWriteLimits::default().with_max_input_bytes(526000)))]
#[case::work_budget(PushLimits::default().with_compression(girt::PackCompression::Delta(girt::DeltaOptions::default().with_max_work(1))))]
#[case::ordinary(PushLimits::default().with_compression(girt::PackCompression::Ordinary))]
fn insufficient_search_budget_falls_back_without_missing_bases(#[case] limits: PushLimits) {
    let f = fixture(ObjectFormat::Sha1);
    let prepared = prepare_with_limits(&f, &[f.old], limits);
    assert!(prepared.pack_bytes() >= f.data.len());
    let bytes = wire_pack(&f, &prepared, false);
    git(
        &f.dest,
        &["index-pack", "--stdin", "--fix-thin", "--strict"],
        &bytes,
    );
    git(
        &f.dest,
        &["update-ref", "refs/heads/main", &f.new.to_string()],
        &[],
    );
    assert_eq!(git(&f.dest, &["cat-file", "blob", &f.blob], &[]), f.data);
    git(&f.dest, &["fsck", "--strict"], &[]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn corrupt_external_base_refuses_preparation_and_preserves_receiver(#[case] format: ObjectFormat) {
    let f = fixture(format);
    let old_blob = text(git(
        &f.source,
        &["rev-parse", &format!("{}:large.bin", f.old)],
        &[],
    ));
    let path = f
        .source
        .join("objects")
        .join(&old_blob[..2])
        .join(&old_blob[2..]);
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    #[cfg(windows)]
    {
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        // Windows clears a file attribute here; Unix permissions are never changed.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::write(path, bytes).unwrap();
    let repo = Repository::open(&f.source).unwrap();
    let result = PreparedPush::new_local(
        &repo.objects(PackLimits::trusted()).unwrap(),
        vec![
            PushCommand::new(RefName::new("refs/heads/main").unwrap(), Some(f.old), f.new)
                .with_force(ForcePolicy::FastForwardOnly),
        ],
        &[f.old],
        PushLimits::default(),
        &AtomicBool::new(false),
    );
    assert!(matches!(result, Err(girt::push::PushFailure::Read { .. })));
    assert_eq!(
        text(git(&f.dest, &["rev-parse", "refs/heads/main"], &[])),
        f.old.to_string()
    );
    git(&f.dest, &["fsck", "--strict"], &[]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn empty_receiver_gets_every_delta_base_in_the_same_pack(#[case] format: ObjectFormat) {
    let f = fixture(format);
    let prepared = prepare(&f, &[], 1);
    let bytes = wire_pack(&f, &prepared, false);
    assert!(bytes.len() < f.data.len() * 3 / 2);
    assert!(entry_codes(&bytes, format.digest_len()).contains(&7));
    let empty = f._root.path().join("empty.git");
    git(
        f._root.path(),
        &[
            "init",
            "-q",
            "--bare",
            &format!("--object-format={format}"),
            empty.to_str().unwrap(),
        ],
        &[],
    );
    git(&empty, &["index-pack", "--stdin", "--strict"], &bytes);
    git(
        &empty,
        &["update-ref", "refs/heads/main", &f.new.to_string()],
        &[],
    );
    assert_eq!(git(&empty, &["cat-file", "blob", &f.blob], &[]), f.data);
    git(&empty, &["fsck", "--strict"], &[]);
}

fn configured_push(f: &Fixture, url: &str, environment: &girt::transfer::Environment) {
    git(&f.source, &["remote", "add", "origin", url], &[]);
    let report = Repository::open(&f.source)
        .unwrap()
        .push(
            "origin",
            vec![
                PushCommand::new(RefName::new("refs/heads/main").unwrap(), Some(f.old), f.new)
                    .with_force(ForcePolicy::FastForwardOnly),
            ],
            &girt::transfer::PushOptions::default(),
            environment,
            &mut girt::transfer::NoCallbacks,
        )
        .unwrap();
    assert!(report.all_succeeded());
    assert_eq!(git(&f.dest, &["cat-file", "blob", &f.blob], &[]), f.data);
    git(&f.dest, &["fsck", "--strict"], &[]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn configured_native_push_imports_thin_delta(#[case] format: ObjectFormat) {
    let f = fixture(format);
    configured_push(
        &f,
        f.dest.to_str().unwrap(),
        &girt::transfer::Environment::default(),
    );
}

#[cfg(feature = "http")]
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn configured_http_push_sends_thin_delta_to_git(#[case] format: ObjectFormat) {
    let f = fixture(format);
    git(&f.dest, &["config", "http.receivepack", "true"], &[]);
    let server = http_git::Server::new(&f.dest, "", "", None);
    configured_push(&f, &server.url, &girt::transfer::Environment::default());
    assert_eq!(server.requests(), vec!["GET", "POST"]);
}

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn configured_ssh_push_sends_thin_delta_to_git_without_sshd(#[case] format: ObjectFormat) {
    let f = fixture(format);
    let client = f._root.path().join("ssh");
    stand_in::shell(&client, "for last; do :; done\nexec /bin/sh -c \"$last\"\n");
    let url = format!("ssh://localhost{}", f.dest.display());
    let environment = girt::transfer::Environment::default()
        .with("PATH", std::env::var_os("PATH").unwrap())
        .with("GIT_SSH_COMMAND", client);
    configured_push(&f, &url, &environment);
}

#[test]
fn receiver_decode_budget_refuses_before_installing_or_publishing() {
    let f = fixture(ObjectFormat::Sha1);
    let prepared = prepare_with_limits(
        &f,
        &[f.old],
        PushLimits::default()
            .with_pack(girt::PackWriteLimits::default().with_max_input_bytes(526000)),
    );
    let before = git(
        &f.dest,
        &["cat-file", "--batch-all-objects", "--batch-check"],
        &[],
    );
    let result = girt::push::send_local(&f.dest, &prepared, &AtomicBool::new(false));
    assert!(
        matches!(result, Err(girt::push::PushError::NotSent(girt::push::PushFailure::Install(ref error)))
        if matches!(error.as_ref(), girt::fetch::FetchError::Pack(girt::ObjectReadError::Limit("cumulative decode bytes"))))
    );
    assert_eq!(
        git(
            &f.dest,
            &["cat-file", "--batch-all-objects", "--batch-check"],
            &[]
        ),
        before
    );
    assert_eq!(
        text(git(&f.dest, &["rev-parse", "refs/heads/main"], &[])),
        f.old.to_string()
    );
    git(&f.dest, &["fsck", "--strict"], &[]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn unreferenced_receiver_objects_do_not_confuse_internal_and_external_bases(
    #[case] format: ObjectFormat,
) {
    let mut f = fixture(format);
    let pending = git(
        &f.source,
        &["pack-objects", "--stdout", "--revs"],
        format!("{}\n^{}\n", f.new, f.old).as_bytes(),
    );
    // The receiver has this commit physically, as after an interrupted or rejected receive,
    // but its refs still name only the advertised old history.
    git(&f.dest, &["index-pack", "--stdin", "--strict"], &pending);
    f.data[131072..131080].copy_from_slice(b"ANOTHER!");
    let (new, blob) = commit(&f.source, &f.data, Some(f.new));
    f.new = new;
    f.blob = blob;
    let prepared = prepare(&f, &[f.old], 4);
    assert_eq!(prepared.object_count(), 6);
    publish(&f, &prepared);
}

#[test]
fn configured_default_push_does_not_use_hidden_receiver_history_as_external_bases() {
    let f = fixture(ObjectFormat::Sha1);
    git(
        &f.dest,
        &["config", "receive.hideRefs", "refs/heads/main"],
        &[],
    );
    git(
        &f.source,
        &["remote", "add", "origin", f.dest.to_str().unwrap()],
        &[],
    );
    let report = Repository::open(&f.source)
        .unwrap()
        .push(
            "origin",
            vec![
                PushCommand::new(RefName::new("refs/heads/topic").unwrap(), None, f.new)
                    .with_force(ForcePolicy::FastForwardOnly),
            ],
            &girt::transfer::PushOptions::default(),
            &girt::transfer::Environment::default(),
            &mut girt::transfer::NoCallbacks,
        )
        .unwrap();
    assert!(report.all_succeeded());
    assert_eq!(
        text(git(&f.dest, &["rev-parse", "refs/heads/topic"], &[])),
        f.new.to_string()
    );
    assert_eq!(
        text(git(&f.dest, &["rev-parse", "refs/heads/main"], &[])),
        f.old.to_string()
    );
    assert_eq!(git(&f.dest, &["cat-file", "blob", &f.blob], &[]), f.data);
    git(&f.dest, &["fsck", "--strict"], &[]);
}
