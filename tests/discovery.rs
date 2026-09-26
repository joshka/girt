//! Original disposable Git repositories establish observable advertisement formats.

use std::num::NonZeroU32;
use std::ops::ControlFlow;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use girt::fetch::{
    FetchError, FetchLimits, FetchOptions, KnownHistory, ProtocolVersion, RemoteHead, discover,
    discover_local, discover_session, receive, receive_with_known_depth,
};
use girt::transport::TransportControl;
use girt::{ObjectFormat, ObjectId, PackLimits, ReadLimits, Repository};
use rstest::rstest;

fn git(directory: &Path, args: &[&str], input: &[u8], version: Option<&str>) -> Vec<u8> {
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    if let Some(version) = version {
        command.env("GIT_PROTOCOL", format!("version={version}"));
    }
    let mut child = command.spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn repository(format: ObjectFormat) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let choice = format!("--object-format={format}");
    git(
        directory.path(),
        &["init", "--bare", "--initial-branch=main", &choice, "."],
        b"",
        None,
    );
    directory
}

fn commit(directory: &tempfile::TempDir) -> ObjectId {
    let tree = git(
        directory.path(),
        &["hash-object", "-t", "tree", "-w", "--stdin"],
        b"",
        None,
    );
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let output = Command::new("git")
        .current_dir(directory.path())
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=f@example.test",
            "commit-tree",
            tree,
        ])
        .env("GIT_AUTHOR_DATE", "@1000000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1000000000 +0000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let id = output.wait_with_output().unwrap();
    assert!(id.status.success());
    let id = std::str::from_utf8(&id.stdout).unwrap().trim();
    git(
        directory.path(),
        &["update-ref", "refs/heads/main", id],
        b"",
        None,
    );
    id.parse().unwrap()
}

fn child_commit(directory: &tempfile::TempDir, parent: ObjectId) -> ObjectId {
    git(
        directory.path(),
        &["config", "user.name", "Fixture"],
        b"",
        None,
    );
    git(
        directory.path(),
        &["config", "user.email", "f@example.test"],
        b"",
        None,
    );
    let tree = git(directory.path(), &["rev-parse", "main^{tree}"], b"", None);
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let next = git(
        directory.path(),
        &["commit-tree", tree, "-p", &parent.to_string()],
        b"next\n",
        None,
    );
    let id: ObjectId = std::str::from_utf8(&next).unwrap().trim().parse().unwrap();
    git(
        directory.path(),
        &["update-ref", "refs/heads/main", &id.to_string()],
        b"",
        None,
    );
    id
}

#[rstest]
#[case::sha1_v0(ObjectFormat::Sha1, "0")]
#[case::sha256_v0(ObjectFormat::Sha256, "0")]
#[case::sha1_v1(ObjectFormat::Sha1, "1")]
#[case::sha256_v1(ObjectFormat::Sha256, "1")]
fn git_v0_v1_wire_fetch_installs_objects(#[case] format: ObjectFormat, #[case] version: &str) {
    let source = repository(format);
    let tip = commit(&source);
    let mut child = Command::new("git")
        .current_dir(source.path())
        .args(["upload-pack", "."])
        .env("GIT_PROTOCOL", format!("version={version}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = child.stdout.take().unwrap();
    let mut writer = child.stdin.take().unwrap();
    let fetched = receive(
        &mut reader,
        &mut writer,
        |_| vec![tip],
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    drop(writer);
    assert!(child.wait_with_output().unwrap().status.success());

    let destination = repository(format);
    let repo = Repository::open(destination.path()).unwrap();
    fetched
        .install(&repo, PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        objects
            .read(tip, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        tip
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn git_v0_depth_reports_boundary_without_installing(#[case] format: ObjectFormat) {
    let source = repository(format);
    let parent = commit(&source);
    let tip = child_commit(&source, parent);
    let mut child = Command::new("git")
        .current_dir(source.path())
        .args(["upload-pack", "."])
        .env("GIT_PROTOCOL", "version=0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = child.stdout.take().unwrap();
    let mut writer = child.stdin.take().unwrap();
    let fetched = receive_with_known_depth(
        &mut reader,
        &mut writer,
        |_| vec![tip],
        &KnownHistory::default(),
        FetchOptions {
            depth: NonZeroU32::new(1),
            limits: FetchLimits::default(),
        },
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    drop(writer);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fetched = fetched.unwrap();
    assert_eq!(fetched.shallow_roots(), &[tip]);
    let destination = repository(format);
    let repo = Repository::open(destination.path()).unwrap();
    assert!(matches!(
        fetched.install(&repo, PackLimits::default(), &AtomicBool::new(false)),
        Err(FetchError::Unsupported(
            "shallow installation requires boundary publication"
        ))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn git_v0_deepens_verified_shallow_history(#[case] format: ObjectFormat) {
    let source = repository(format);
    let first = commit(&source);
    let previous = child_commit(&source, first);
    let destination = tempfile::tempdir().unwrap();
    let path = destination.path().join("shallow.git");
    git(
        source.path(),
        &[
            "clone",
            "--bare",
            "--depth=1",
            "--no-local",
            source.path().to_str().unwrap(),
            path.to_str().unwrap(),
        ],
        b"",
        None,
    );
    let repo = Repository::open(&path).unwrap();
    assert!(repo.shallow_roots().contains(previous));
    let store = repo.objects(PackLimits::default()).unwrap();
    let cancel = AtomicBool::new(false);
    let known = KnownHistory::new(&store, &[previous], FetchLimits::default(), &cancel).unwrap();

    let tip = child_commit(&source, previous);
    let mut ordinary = Command::new("git")
        .current_dir(source.path())
        .args(["upload-pack", "."])
        .env("GIT_PROTOCOL", "version=0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ordinary_reader = ordinary.stdout.take().unwrap();
    let mut ordinary_writer = ordinary.stdin.take().unwrap();
    let ordinary_fetch = receive_with_known_depth(
        &mut ordinary_reader,
        &mut ordinary_writer,
        |_| vec![tip],
        &known,
        FetchOptions {
            depth: None,
            limits: FetchLimits::default(),
        },
        &cancel,
        |_| ControlFlow::Continue(()),
    );
    drop(ordinary_writer);
    let ordinary_output = ordinary.wait_with_output().unwrap();
    assert!(
        ordinary_output.status.success(),
        "{}",
        String::from_utf8_lossy(&ordinary_output.stderr)
    );
    assert_eq!(ordinary_fetch.unwrap().shallow_roots(), &[previous]);

    let mut child = Command::new("git")
        .current_dir(source.path())
        .args(["upload-pack", "."])
        .env("GIT_PROTOCOL", "version=0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = child.stdout.take().unwrap();
    let mut writer = child.stdin.take().unwrap();
    let fetched = receive_with_known_depth(
        &mut reader,
        &mut writer,
        |_| vec![tip],
        &known,
        FetchOptions {
            depth: NonZeroU32::new(3),
            limits: FetchLimits::default(),
        },
        &cancel,
        |_| ControlFlow::Continue(()),
    );
    drop(writer);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fetched = fetched.unwrap();
    assert_eq!(fetched.shallow_roots(), &[first]);
    assert!(fetched.object_count() > 0);
}

fn packet(output: &mut Vec<u8>, line: &[u8]) {
    output.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
    output.extend_from_slice(line);
}

fn ls_refs_request(format: ObjectFormat) -> Vec<u8> {
    let mut request = Vec::new();
    packet(&mut request, b"command=ls-refs\n");
    if format == ObjectFormat::Sha256 {
        packet(&mut request, b"object-format=sha256\n");
    }
    request.extend_from_slice(b"0001");
    packet(&mut request, b"peel\n");
    packet(&mut request, b"symrefs\n");
    packet(&mut request, b"unborn\n");
    request.extend_from_slice(b"0000");
    request
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn local_unborn_head_has_explicit_target(#[case] format: ObjectFormat) {
    let repo = repository(format);
    let cancel = AtomicBool::new(false);
    let result = discover_local(
        repo.path(),
        FetchLimits::default(),
        TransportControl::new(&cancel),
    )
    .unwrap();
    assert_eq!(result.object_format, format);
    assert_eq!(result.version, ProtocolVersion::Native);
    assert!(result.supports_current_fetch());
    assert!(
        matches!(result.head, RemoteHead::Unborn { branch } if branch.as_bytes() == b"refs/heads/main")
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn git_v0_and_v1_advertisements_resolve_default_head(#[case] format: ObjectFormat) {
    let repo = repository(format);
    let id = commit(&repo);
    let cancel = AtomicBool::new(false);
    let v0 = git(
        repo.path(),
        &["upload-pack", "--stateless-rpc", "--advertise-refs", "."],
        b"",
        Some("0"),
    );
    let v1 = git(
        repo.path(),
        &["upload-pack", "--stateless-rpc", "--advertise-refs", "."],
        b"",
        Some("1"),
    );
    let a = discover(&mut v0.as_slice(), FetchLimits::default(), &cancel).unwrap();
    let b = discover(&mut v1.as_slice(), FetchLimits::default(), &cancel).unwrap();
    assert_eq!(a.object_format, format);
    assert_eq!(b.object_format, format);
    assert_eq!(a.version, ProtocolVersion::V0);
    assert_eq!(b.version, ProtocolVersion::V1);
    assert!(a.supports_current_fetch());
    assert!(matches!(a.head, RemoteHead::Symbolic { id: actual, .. } if actual == id));
    assert!(matches!(b.head, RemoteHead::Symbolic { id: actual, .. } if actual == id));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn git_v2_unborn_ls_refs_matches_request(#[case] format: ObjectFormat) {
    let repo = repository(format);
    let caps = git(
        repo.path(),
        &["upload-pack", "--stateless-rpc", "--advertise-refs", "."],
        b"",
        Some("2"),
    );
    let expected = ls_refs_request(format);
    let refs = git(
        repo.path(),
        &["upload-pack", "--stateless-rpc", "."],
        &expected,
        Some("2"),
    );
    let mut input = caps;
    input.extend_from_slice(&refs);
    let cancel = AtomicBool::new(false);
    let mut actual = Vec::new();
    let discovered = discover_session(
        &mut input.as_slice(),
        &mut actual,
        FetchLimits::default(),
        &cancel,
    )
    .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(discovered.object_format, format);
    assert_eq!(discovered.version, ProtocolVersion::V2);
    assert!(!discovered.supports_current_fetch());
    assert!(
        matches!(discovered.head, RemoteHead::Unborn { branch } if branch.as_bytes() == b"refs/heads/main")
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn git_v2_populated_ls_refs_resolves_head(#[case] format: ObjectFormat) {
    let repo = repository(format);
    let id = commit(&repo);
    let caps = git(
        repo.path(),
        &["upload-pack", "--stateless-rpc", "--advertise-refs", "."],
        b"",
        Some("2"),
    );
    let expected = ls_refs_request(format);
    let refs = git(
        repo.path(),
        &["upload-pack", "--stateless-rpc", "."],
        &expected,
        Some("2"),
    );
    let mut input = caps;
    input.extend_from_slice(&refs);
    let cancel = AtomicBool::new(false);
    let mut actual = Vec::new();
    let discovered = discover_session(
        &mut input.as_slice(),
        &mut actual,
        FetchLimits::default(),
        &cancel,
    )
    .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(discovered.object_format, format);
    assert!(matches!(discovered.head, RemoteHead::Symbolic { id: actual, .. } if actual == id));
}
