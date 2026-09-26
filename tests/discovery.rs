//! Original disposable Git repositories establish observable advertisement formats.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use girt::fetch::{
    FetchLimits, ProtocolVersion, RemoteHead, discover, discover_local, discover_session,
};
use girt::transport::TransportControl;
use girt::{ObjectFormat, ObjectId};
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
    assert_eq!(a.supports_current_fetch(), format == ObjectFormat::Sha1);
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
