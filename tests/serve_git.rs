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

/// The response up to the pack, which differs between servers.
fn before_pack(response: &[u8]) -> String {
    let text = String::from_utf8_lossy(response);
    match text.find("000dpackfile\n") {
        Some(end) => text[..end].to_owned(),
        None => text.into_owned(),
    }
}

/// `one`, `two`, then `three`, `four` on main and `side` on a branch, merged, then `five`, `six`.
fn merge_fixture(root: &Path, format: &str) -> std::path::PathBuf {
    let repo = root.join("repo");
    git(
        root,
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
    commit(&repo, "two");
    git(&repo, &["switch", "-q", "-c", "side"], b"");
    commit(&repo, "side");
    git(&repo, &["switch", "-q", "main"], b"");
    commit(&repo, "three");
    commit(&repo, "four");
    git(
        &repo,
        &["merge", "-q", "--no-edit", "--no-gpg-sign", "side"],
        b"",
    );
    commit(&repo, "five");
    commit(&repo, "six");
    git(
        &repo,
        &["tag", "-a", "--no-sign", "-m", "t", "tagged", "main~1"],
        b"",
    );
    repo
}

#[rstest::rstest]
#[case::depth_one(&[], &["deepen 1"])]
#[case::depth_to_root(&[], &["deepen 7"])]
#[case::deepen_past_merge(&["main~3", "side"], &["deepen 6"])]
#[case::relative(&["main~3", "side"], &["deepen 1", "deepen-relative"])]
#[case::unshallow(&["main~1"], &["deepen 2147483647"])]
#[case::since(&[], &["deepen-since 1700000000"])]
#[case::not_tag(&[], &["deepen-not tagged"])]
#[case::not_branch(&[], &["deepen-not side"])]
#[case::shallow_without_deepen(&["main~2"], &[])]
fn shallow_info_matches_git(
    #[case] client_shallow: &[&str],
    #[case] deepen: &[&str],
    #[values("sha1", "sha256")] format: &str,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = merge_fixture(root.path(), format);
    let mut lines = vec![format!("want {}", rev_parse(&repo, "main"))];
    for revision in client_shallow {
        lines.push(format!("shallow {}", rev_parse(&repo, revision)));
    }
    lines.extend(deepen.iter().map(|line| (*line).to_owned()));
    lines.push("done".to_owned());
    let mut request = pkt("command=fetch\n");
    request.extend(pkt(&format!("object-format={format}\n")));
    request.extend_from_slice(b"0001");
    for line in &lines {
        request.extend(pkt(&format!("{line}\n")));
    }
    request.extend_from_slice(b"0000");
    let by_girt = girt_respond(&repo, &request);
    let by_git = git_upload_pack(&repo, &request);
    assert_eq!(before_pack(&by_girt), before_pack(&by_git));
    assert_eq!(pack_object_count(&by_girt), pack_object_count(&by_git));
}

/// The object count in the pack header of a v2 `packfile` section.
fn pack_object_count(response: &[u8]) -> Option<u32> {
    let start = response
        .windows(13)
        .position(|window| window == b"000dpackfile\n")?
        + 13;
    let mut rest = &response[start..];
    let mut pack = Vec::new();
    while rest.len() >= 4 && pack.len() < 12 {
        let length = usize::from_str_radix(std::str::from_utf8(&rest[..4]).ok()?, 16).ok()?;
        if length == 0 {
            break;
        }
        if rest[4] == 1 {
            pack.extend_from_slice(&rest[5..length]);
        }
        rest = &rest[length..];
    }
    Some(u32::from_be_bytes(pack.get(8..12)?.try_into().ok()?))
}

/// [`merge_fixture`] plus an unrelated `other` branch of three commits.
fn orphan_fixture(root: &Path) -> std::path::PathBuf {
    let repo = merge_fixture(root, "sha1");
    git(&repo, &["switch", "-q", "--orphan", "other"], b"");
    for name in ["o1", "o2", "o3"] {
        commit(&repo, name);
    }
    git(&repo, &["switch", "-q", "main"], b"");
    repo
}

#[rstest::rstest]
#[case::unshallow_lifts_unrelated(&["main~2", "other"], &["deepen 2147483647"])]
#[case::large_depth_keeps_unrelated(&["main~2", "other"], &["deepen 1000"])]
#[case::relative_skips_unrelated(&["main~2", "other"], &["deepen 1", "deepen-relative"])]
#[case::since_lifts_root(&["main~6"], &["deepen-since 1"])]
#[case::boundary_without_deepen(&["main~1"], &[])]
fn shallow_boundaries_match_git(#[case] client_shallow: &[&str], #[case] deepen: &[&str]) {
    let root = tempfile::tempdir().unwrap();
    let repo = orphan_fixture(root.path());
    let mut lines = vec![format!("want {}", rev_parse(&repo, "main"))];
    for revision in client_shallow {
        lines.push(format!("shallow {}", rev_parse(&repo, revision)));
    }
    lines.extend(deepen.iter().map(|line| (*line).to_owned()));
    lines.push("done".to_owned());
    let request = fetch_request(&lines);
    let by_girt = girt_respond(&repo, &request);
    let by_git = git_upload_pack(&repo, &request);
    assert_eq!(before_pack(&by_girt), before_pack(&by_git));
    assert_eq!(pack_object_count(&by_girt), pack_object_count(&by_git));
}

/// A full-duplex v0 session against Git's server, which reads the request as a client would
/// send it.
fn git_upload_pack_v0(repo: &Path, request: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", repo.join("absent-config"))
        .arg("upload-pack")
        .arg(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(request)?;
            child.wait_with_output()
        })
        .unwrap();
    output.stdout
}

fn girt_serve_v0(repo: &Path, request: &[u8]) -> Vec<u8> {
    let repository = Repository::open(repo).unwrap();
    let mut response = Vec::new();
    let _ = UploadPack::new(&repository).unwrap().serve(
        Protocol::V0,
        &mut &request[..],
        &mut response,
        &AtomicBool::new(false),
    );
    response
}

/// Packets up to the first pack data, with the agent capability and progress removed.
fn v0_transcript(response: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = response;
    while rest.len() >= 4 {
        let length = usize::from_str_radix(std::str::from_utf8(&rest[..4]).unwrap_or("x"), 16);
        let Ok(length) = length else {
            lines.push("<raw pack>".to_owned());
            break;
        };
        if length == 0 {
            lines.push("0000".to_owned());
            rest = &rest[4..];
            continue;
        }
        let payload = &rest[4..length];
        rest = &rest[length..];
        match payload.first() {
            Some(1) => {
                lines.push("<pack>".to_owned());
                break;
            }
            Some(2) => continue,
            _ => {}
        }
        let text = String::from_utf8_lossy(payload);
        let text = match text.find(" agent=") {
            Some(start) => text[..start].to_owned(),
            None => text.into_owned(),
        };
        lines.push(text);
    }
    lines
}

/// Builds a v0 request: each `None` is a flush.
fn v0_request(lines: &[Option<String>]) -> Vec<u8> {
    lines
        .iter()
        .flat_map(|line| match line {
            Some(line) => pkt(&format!("{line}\n")),
            None => b"0000".to_vec(),
        })
        .collect()
}

#[rstest::rstest]
#[case::list_only("", "")]
#[case::clone("", "multi_ack_detailed side-band-64k")]
#[case::detailed("main~3 | main~4 ! U ! main~6", "multi_ack_detailed side-band-64k")]
#[case::detailed_single("main~3", "multi_ack_detailed side-band-64k")]
#[case::continue_acks("U ! main~3 | main~4 ! U", "multi_ack side-band-64k")]
#[case::plain("U | main~3 | main~4 ! U", "side-band-64k")]
#[case::no_common("U", "multi_ack_detailed side-band-64k")]
#[case::small_sideband("main~3", "multi_ack_detailed side-band")]
#[case::no_sideband("main~3", "multi_ack_detailed")]
#[case::no_done_is_ignored("main~3", "multi_ack_detailed side-band-64k no-done")]
fn v0_negotiation_matches_git(
    #[case] haves: &str,
    #[case] capabilities: &str,
    #[values("sha1", "sha256")] format: &str,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = merge_fixture(root.path(), format);
    let unknown = "0".repeat(if format == "sha1" { 40 } else { 64 });
    let mut lines = Vec::new();
    if !capabilities.is_empty() {
        let mut capabilities = capabilities.to_owned();
        if format == "sha256" {
            capabilities.push_str(" object-format=sha256");
        }
        lines.push(Some(format!(
            "want {} {capabilities}",
            rev_parse(&repo, "main")
        )));
        lines.push(Some(format!("want {}", rev_parse(&repo, "side"))));
    }
    lines.push(None);
    if !capabilities.is_empty() {
        // `|` separates haves within a round and `!` ends a round with a flush.
        for round in haves.split('!').filter(|round| !round.trim().is_empty()) {
            for have in round.split('|').map(str::trim) {
                let id = if have == "U" {
                    unknown.clone()
                } else {
                    rev_parse(&repo, have)
                };
                lines.push(Some(format!("have {id}")));
            }
            lines.push(None);
        }
        if !capabilities.contains("no-done") {
            lines.push(Some("done".to_owned()));
        }
    }
    let request = v0_request(&lines);
    assert_eq!(
        v0_transcript(&girt_serve_v0(&repo, &request)),
        v0_transcript(&git_upload_pack_v0(&repo, &request)),
    );
}

#[rstest::rstest]
#[case::depth(&["deepen 2"], "")]
#[case::relative(&["shallow main~3", "shallow side", "deepen 1"], " deepen-relative")]
#[case::since(&["deepen-since 1700000000"], "")]
#[case::not(&["deepen-not tagged"], "")]
fn v0_shallow_matches_git(#[case] extra: &[&str], #[case] capability: &str) {
    let root = tempfile::tempdir().unwrap();
    let repo = merge_fixture(root.path(), "sha1");
    let mut lines = vec![Some(format!(
        "want {} multi_ack_detailed side-band-64k shallow{capability}",
        rev_parse(&repo, "main")
    ))];
    for line in extra {
        lines.push(Some(match line.strip_prefix("shallow ") {
            Some(revision) => format!("shallow {}", rev_parse(&repo, revision)),
            None => (*line).to_owned(),
        }));
    }
    lines.extend([
        None,
        Some(format!("have {}", rev_parse(&repo, "main~3"))),
        None,
    ]);
    lines.push(Some("done".to_owned()));
    let request = v0_request(&lines);
    assert_eq!(
        v0_transcript(&girt_serve_v0(&repo, &request)),
        v0_transcript(&git_upload_pack_v0(&repo, &request)),
    );
}

#[rstest::rstest]
#[case::empty(None)]
#[case::hidden(Some(("uploadpack.hideRefs", "refs/tags")))]
#[case::tip(Some(("uploadpack.allowTipSHA1InWant", "true")))]
#[case::reachable(Some(("uploadpack.allowReachableSHA1InWant", "true")))]
#[case::any(Some(("uploadpack.allowAnySHA1InWant", "true")))]
fn v0_advertisement_matches_git(#[case] setting: Option<(&str, &str)>) {
    let root = tempfile::tempdir().unwrap();
    let repo = match setting {
        None => {
            git(root.path(), &["init", "-q", "repo"], b"");
            root.path().join("repo")
        }
        Some((key, value)) => {
            let repo = merge_fixture(root.path(), "sha1");
            git(&repo, &["config", key, value], b"");
            repo
        }
    };
    let request = b"0000";
    assert_eq!(
        v0_transcript(&girt_serve_v0(&repo, request)),
        v0_transcript(&git_upload_pack_v0(&repo, request)),
    );
}

#[rstest::rstest]
#[case::one_want_one_common(&["main"], &["main~3"])]
#[case::two_wants_far_common(&["main", "side"], &["main~3"])]
#[case::two_wants_near_common(&["main", "side"], &["main~3", "main~4"])]
#[case::unknown_only(&["main"], &["U"])]
#[case::no_haves_without_done(&["main"], &[])]
fn v2_negotiation_matches_git(#[case] wants: &[&str], #[case] haves: &[&str]) {
    let root = tempfile::tempdir().unwrap();
    let repo = merge_fixture(root.path(), "sha1");
    let mut lines: Vec<String> = wants
        .iter()
        .map(|want| format!("want {}", rev_parse(&repo, want)))
        .collect();
    for have in haves {
        let id = if *have == "U" {
            "1".repeat(40)
        } else {
            rev_parse(&repo, have)
        };
        lines.push(format!("have {id}"));
    }
    let request = fetch_request(&lines);
    assert_eq!(
        before_pack(&girt_respond(&repo, &request)),
        before_pack(&git_upload_pack(&repo, &request)),
    );
}

/// Repeated flushes after an unhelpful common commit don't repeat the readiness walk, so a small
/// session budget still covers them.
#[test]
fn v0_flushes_share_one_readiness_budget() {
    let root = tempfile::tempdir().unwrap();
    let repo = orphan_fixture(root.path());
    let mut lines = vec![
        Some(format!(
            "want {} multi_ack_detailed side-band-64k",
            rev_parse(&repo, "main")
        )),
        None,
        Some(format!("have {}", rev_parse(&repo, "other"))),
    ];
    lines.extend(std::iter::repeat_n(None, 200));
    lines.push(Some("done".to_owned()));
    let request = v0_request(&lines);
    let repository = Repository::open(&repo).unwrap();
    let limits = girt::serve::UploadLimits::default()
        .with_history(girt::HistoryLimits::default().with_max_commits(40));
    let result = UploadPack::new(&repository)
        .unwrap()
        .with_limits(limits)
        .serve(
            Protocol::V0,
            &mut &request[..],
            &mut Vec::new(),
            &AtomicBool::new(false),
        );
    assert!(result.is_ok(), "{result:?}");
}
