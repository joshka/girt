//! `UploadPack` answers protocol v2 `ls-refs` with the same bytes as `git upload-pack`.
#[allow(dead_code)]
#[path = "support/pack_git.rs"]
mod pack_git;

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use girt::Repository;
use girt::serve::{Protocol, UploadPack, WantPolicy};
use pack_git::git;

fn pkt(line: &str) -> Vec<u8> {
    let mut bytes = format!("{:04x}", line.len() + 4).into_bytes();
    bytes.extend_from_slice(line.as_bytes());
    bytes
}

fn ls_refs(format: &str, arguments: &[&str]) -> Vec<u8> {
    let mut request = pkt("command=ls-refs\n");
    request.extend(pkt(&format!("object-format={format}\n")));
    request.extend_from_slice(b"0001");
    for argument in arguments {
        request.extend(pkt(&format!("{argument}\n")));
    }
    request.extend_from_slice(b"0000");
    request
}

/// Runs Git's server with isolated configuration.
fn git_upload_pack(repo: &Path, request: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", repo.join("absent-config"))
        .env("GIT_PROTOCOL", "version=2")
        .args(["upload-pack", "--stateless-rpc"])
        .arg(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(request)?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success());
    output.stdout
}

fn girt_respond(repo: &Path, request: &[u8]) -> Vec<u8> {
    let repository = Repository::open(repo).unwrap();
    let upload = UploadPack::new(&repository).unwrap();
    let mut response = Vec::new();
    upload
        .respond(&mut &request[..], &mut response, &AtomicBool::new(false))
        .unwrap();
    response
}

fn commit(repo: &Path, name: &str) {
    std::fs::write(repo.join(name), name).unwrap();
    git(repo, &["add", name], b"");
    git(repo, &["commit", "-q", "--no-gpg-sign", "-m", name], b"");
}

#[rstest::rstest]
#[case::plain(&[])]
#[case::all(&["peel", "symrefs", "unborn"])]
#[case::prefixes(&["peel", "ref-prefix refs/tags/", "ref-prefix HEAD"])]
#[case::no_match(&["ref-prefix refs/nothing/"])]
fn ls_refs_matches_git(#[case] arguments: &[&str], #[values("sha1", "sha256")] format: &str) {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    git(
        root.path(),
        &[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={format}"),
            "repo",
        ],
        b"",
    );
    commit(&repo, "one");
    git(&repo, &["tag", "light"], b"");
    git(
        &repo,
        &["tag", "-a", "--no-sign", "-m", "t", "annotated"],
        b"",
    );
    git(
        &repo,
        &["tag", "-a", "--no-sign", "-m", "t", "nested", "annotated"],
        b"",
    );
    git(&repo, &["branch", "side"], b"");
    git(
        &repo,
        &["symbolic-ref", "refs/heads/alias", "refs/heads/side"],
        b"",
    );
    git(
        &repo,
        &["config", "uploadpack.hideRefs", "refs/tags/light"],
        b"",
    );
    let request = ls_refs(format, arguments);
    assert_eq!(
        String::from_utf8(girt_respond(&repo, &request)).unwrap(),
        String::from_utf8(git_upload_pack(&repo, &request)).unwrap(),
    );
}

#[test]
fn unborn_head_matches_git() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "-b", "trunk", "repo"], b"");
    let repo = root.path().join("repo");
    for arguments in [&["unborn", "symrefs"][..], &["unborn"], &["symrefs"]] {
        let request = ls_refs("sha1", arguments);
        assert_eq!(
            String::from_utf8(girt_respond(&repo, &request)).unwrap(),
            String::from_utf8(git_upload_pack(&repo, &request)).unwrap(),
            "{arguments:?}"
        );
    }
}

#[test]
fn advertisement_lists_v2_commands() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "repo"], b"");
    let repository = Repository::open(root.path().join("repo")).unwrap();
    let mut advertisement = Vec::new();
    UploadPack::new(&repository)
        .unwrap()
        .advertise(Protocol::V2, &mut advertisement, &AtomicBool::new(false))
        .unwrap();
    let text = String::from_utf8(advertisement).unwrap();
    assert!(text.starts_with("000eversion 2\n"));
    assert!(text.contains("ls-refs=unborn\n"));
    assert!(text.contains("object-format=sha1\n"));
    assert!(text.ends_with("0000"));
}

fn fetch_request(lines: &[String]) -> Vec<u8> {
    let mut request = pkt("command=fetch\n");
    request.extend_from_slice(b"0001");
    for line in lines {
        request.extend(pkt(&format!("{line}\n")));
    }
    request.extend_from_slice(b"0000");
    request
}

fn rev_parse(repo: &Path, revision: &str) -> String {
    String::from_utf8(git(repo, &["rev-parse", revision], b""))
        .unwrap()
        .trim()
        .to_owned()
}

/// A repository whose `side` branch is hidden, with an untagged commit below `main`.
fn hidden_fixture(root: &Path) -> std::path::PathBuf {
    let repo = root.join("repo");
    git(root, &["init", "-q", "-b", "main", "repo"], b"");
    commit(&repo, "one");
    commit(&repo, "two");
    git(&repo, &["branch", "side"], b"");
    commit(&repo, "three");
    git(
        &repo,
        &["config", "uploadpack.hideRefs", "refs/heads/side"],
        b"",
    );
    repo
}

#[rstest::rstest]
#[case::advertised(WantPolicy::Advertised, [false, false])]
#[case::tips(WantPolicy::Tips, [true, false])]
#[case::reachable(WantPolicy::Reachable, [true, true])]
#[case::any(WantPolicy::Any, [true, true])]
fn explicit_want_policy_applies_to_v2(#[case] policy: WantPolicy, #[case] expected: [bool; 2]) {
    let root = tempfile::tempdir().unwrap();
    let repo = hidden_fixture(root.path());
    git(&repo, &["branch", "-D", "side"], b"");
    git(&repo, &["branch", "side", "main~1"], b"");
    let hidden_tip = rev_parse(&repo, "side");
    let reachable = rev_parse(&repo, "main~2");
    let repository = Repository::open(&repo).unwrap();
    let upload = UploadPack::new(&repository)
        .unwrap()
        .with_want_policy(policy);
    for (want, allowed) in [hidden_tip, reachable].into_iter().zip(expected) {
        let request = fetch_request(&[format!("want {want}"), "done".to_owned()]);
        let mut response = Vec::new();
        let result = upload.respond(&mut &request[..], &mut response, &AtomicBool::new(false));
        assert_eq!(result.is_ok(), allowed, "{policy:?} {want}: {result:?}");
        if !allowed {
            let expected = format!("ERR upload-pack: not our ref {want}\n");
            assert!(String::from_utf8_lossy(&response).contains(&expected));
        }
    }
}

#[test]
fn wait_for_done_acknowledges_without_a_pack() {
    let root = tempfile::tempdir().unwrap();
    let repo = hidden_fixture(root.path());
    let want = rev_parse(&repo, "main");
    let have = rev_parse(&repo, "main~1");
    let request = fetch_request(&[
        format!("want {want}"),
        format!("have {have}"),
        "wait-for-done".to_owned(),
    ]);
    assert_eq!(
        String::from_utf8(girt_respond(&repo, &request)).unwrap(),
        String::from_utf8(git_upload_pack(&repo, &request)).unwrap(),
    );
}
