//! Git clients clone and fetch from `girt upload-pack` and get what Git's own server sends.
#[allow(dead_code)]
#[path = "../../tests/support/pack_git.rs"]
mod pack_git;

use std::path::Path;

use pack_git::{git, git_output};

/// The `--upload-pack` command naming the girt binary under test.
fn upload_pack() -> String {
    format!("--upload-pack='{}' upload-pack", env!("CARGO_BIN_EXE_girt"))
}

fn commit(repo: &Path, name: &str) {
    std::fs::write(repo.join(name), format!("{name}\n")).unwrap();
    git(repo, &["add", name], b"");
    git(repo, &["commit", "-q", "--no-gpg-sign", "-m", name], b"");
}

/// A repository with two branches, a lightweight tag and nested annotated tags.
fn origin(root: &Path, format: &str) -> std::path::PathBuf {
    let origin = root.join("origin");
    git(
        root,
        &[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={format}"),
            "origin",
        ],
        b"",
    );
    commit(&origin, "one");
    git(&origin, &["tag", "light"], b"");
    commit(&origin, "two");
    git(
        &origin,
        &["tag", "-a", "--no-sign", "-m", "inner", "inner"],
        b"",
    );
    git(
        &origin,
        &["tag", "-a", "--no-sign", "-m", "outer", "outer", "inner"],
        b"",
    );
    git(&origin, &["switch", "-q", "-c", "side"], b"");
    commit(&origin, "side");
    git(&origin, &["switch", "-q", "main"], b"");
    origin
}

fn refs(repo: &Path) -> String {
    String::from_utf8(git(
        repo,
        &["for-each-ref", "--format=%(objectname) %(refname)"],
        b"",
    ))
    .unwrap()
}

/// Clones `origin` into `name`, through girt's server when `girt` is set.
fn clone(root: &Path, origin: &Path, name: &str, girt: bool) {
    clone_with(root, origin, name, girt, "protocol.version=2");
}

fn clone_with(root: &Path, origin: &Path, name: &str, girt: bool, protocol: &str) {
    let origin = origin.to_str().unwrap();
    let mut args = vec!["-c", protocol, "clone", "-q"];
    let server = upload_pack();
    if girt {
        args.push(&server);
    }
    args.extend(["--no-local", origin, name]);
    git(root, &args, b"");
    git(&root.join(name), &["fsck", "--strict"], b"");
}

fn fetch_with(repo: &Path, girt: bool, protocol: &str) {
    let server = upload_pack();
    let mut args = vec!["-c", protocol, "fetch", "-q"];
    if girt {
        args.push(&server);
    }
    args.extend(["origin", "--tags"]);
    git(repo, &args, b"");
    git(repo, &["fsck", "--strict"], b"");
}

#[test]
fn sha1_clone_and_fetch_match_git() {
    clone_and_fetch_match_git("sha1");
}

#[test]
fn sha256_clone_and_fetch_match_git() {
    clone_and_fetch_match_git("sha256");
}

#[test]
fn sha1_v0_clone_and_fetch_match_git() {
    clone_and_fetch_match_git_with("sha1", "protocol.version=0");
}

#[test]
fn sha256_v0_clone_and_fetch_match_git() {
    clone_and_fetch_match_git_with("sha256", "protocol.version=0");
}

fn clone_and_fetch_match_git(format: &str) {
    clone_and_fetch_match_git_with(format, "protocol.version=2");
}

fn clone_and_fetch_match_git_with(format: &str, protocol: &str) {
    let root = tempfile::tempdir().unwrap();
    let origin = origin(root.path(), format);
    clone_with(root.path(), &origin, "by-girt", true, protocol);
    clone_with(root.path(), &origin, "by-git", false, protocol);
    assert_eq!(
        refs(&root.path().join("by-girt")),
        refs(&root.path().join("by-git"))
    );

    commit(&origin, "three");
    git(
        &origin,
        &["tag", "-a", "--no-sign", "-m", "new", "new"],
        b"",
    );
    fetch_with(&root.path().join("by-girt"), true, protocol);
    fetch_with(&root.path().join("by-git"), false, protocol);
    assert_eq!(
        refs(&root.path().join("by-girt")),
        refs(&root.path().join("by-git"))
    );
}

#[test]
fn clones_empty_repository_with_unborn_branch() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "-b", "trunk", "origin"], b"");
    clone(root.path(), &root.path().join("origin"), "copy", true);
    let head = git(&root.path().join("copy"), &["symbolic-ref", "HEAD"], b"");
    assert_eq!(head, b"refs/heads/trunk\n");
}

#[test]
fn hidden_refs_are_not_listed() {
    let root = tempfile::tempdir().unwrap();
    let origin = origin(root.path(), "sha1");
    git(
        &origin,
        &["config", "uploadpack.hideRefs", "refs/heads/side"],
        b"",
    );
    let listed = git(
        root.path(),
        &[
            "-c",
            "protocol.version=2",
            "ls-remote",
            &upload_pack(),
            origin.to_str().unwrap(),
        ],
        b"",
    );
    let listed = String::from_utf8(listed).unwrap();
    assert!(listed.contains("refs/heads/main"));
    assert!(!listed.contains("refs/heads/side"));
}

/// Fetches `id` by object ID from `origin` into a new repository.
/// Through girt's server when `girt` is set, otherwise Git's.
fn fetch_id(root: &Path, origin: &Path, id: &str, name: &str, girt: bool) -> std::process::Output {
    git(root, &["init", "-q", name], b"");
    let server = upload_pack();
    let mut args = vec!["-c", "protocol.version=2", "fetch"];
    if girt {
        args.push(&server);
    }
    args.extend([origin.to_str().unwrap(), id]);
    git_output(&root.join(name), &args, b"")
}

fn rev_parse(repo: &Path, revision: &str) -> String {
    String::from_utf8(git(repo, &["rev-parse", revision], b""))
        .unwrap()
        .trim()
        .to_owned()
}

/// Protocol v2 wants may name any object, whatever `uploadpack.allow*SHA1InWant` says, as with
/// Git's server.
#[test]
fn v2_wants_match_git_under_every_policy() {
    let root = tempfile::tempdir().unwrap();
    let origin = origin(root.path(), "sha1");
    git(
        &origin,
        &["config", "uploadpack.hideRefs", "refs/heads/side"],
        b"",
    );
    commit(&origin, "untagged");
    commit(&origin, "tip");
    let hidden_tip = rev_parse(&origin, "side");
    let reachable = rev_parse(&origin, "main~1");
    let blob = String::from_utf8(git(&origin, &["hash-object", "-w", "--stdin"], b"loose\n"))
        .unwrap()
        .trim()
        .to_owned();
    let absent = "0123456789012345678901234567890123456789".to_owned();

    let settings = [
        None,
        Some("allowTipSHA1InWant"),
        Some("allowReachableSHA1InWant"),
        Some("allowAnySHA1InWant"),
    ];
    for (index, setting) in settings.into_iter().enumerate() {
        if let Some(setting) = setting {
            git(
                &origin,
                &["config", &format!("uploadpack.{setting}"), "true"],
                b"",
            );
        }
        for want in [&hidden_tip, &reachable, &blob, &absent] {
            let name = format!("copy-{index}-{want}");
            let by_git = fetch_id(root.path(), &origin, want, &format!("{name}-git"), false);
            let by_girt = fetch_id(root.path(), &origin, want, &name, true);
            assert_eq!(
                by_girt.status.success(),
                by_git.status.success(),
                "{setting:?} {want}: {}",
                String::from_utf8_lossy(&by_girt.stderr)
            );
        }
    }
}

/// What a shallow copy holds: its boundary commits and every commit it has.
fn shape(repo: &Path) -> (String, String) {
    let shallow = std::fs::read_to_string(repo.join(".git/shallow")).unwrap_or_default();
    let commits = String::from_utf8(git(repo, &["rev-list", "--all"], b"")).unwrap();
    (shallow, commits)
}

/// Runs `git <args>` in `repo` through girt's server or Git's, checking connectivity after.
fn run(repo: &Path, args: &[&str], girt: bool, protocol: &str) {
    let server = upload_pack();
    let mut full = vec!["-c", protocol];
    full.push(args[0]);
    if girt {
        full.push(&server);
    }
    full.extend(&args[1..]);
    git(repo, &full, b"");
}

/// A longer history, with a merge and a tag, for shallow requests.
fn deep_origin(root: &Path) -> std::path::PathBuf {
    let origin = origin(root, "sha1");
    for name in ["three", "four"] {
        commit(&origin, name);
    }
    git(
        &origin,
        &["merge", "-q", "--no-edit", "--no-gpg-sign", "side"],
        b"",
    );
    for name in ["five", "six"] {
        commit(&origin, name);
    }
    origin
}

#[test]
fn shallow_depth_matches_git() {
    shallow_requests_match_git(
        &["--depth=2"],
        &[
            &["fetch", "-q", "--deepen=2"],
            &["fetch", "-q", "--unshallow"],
        ],
    );
}

#[test]
fn shallow_depth_through_merge_matches_git() {
    shallow_requests_match_git(&["--depth=4"], &[&["fetch", "-q", "--depth=6"]]);
}

#[test]
fn shallow_since_matches_git() {
    shallow_requests_match_git(&["--shallow-since=@1700000000"], &[]);
}

#[test]
fn shallow_exclude_matches_git() {
    shallow_requests_match_git(
        &["--shallow-exclude=light"],
        &[&["fetch", "-q", "--shallow-exclude=inner"]],
    );
}

fn shallow_requests_match_git(clone_args: &[&str], fetches: &[&[&str]]) {
    for protocol in ["protocol.version=2", "protocol.version=0"] {
        shallow_requests_match_git_with(clone_args, fetches, protocol);
    }
}

fn shallow_requests_match_git_with(clone_args: &[&str], fetches: &[&[&str]], protocol: &str) {
    let root = tempfile::tempdir().unwrap();
    let origin = deep_origin(root.path());
    let url = format!("file://{}", origin.display());
    for (name, girt) in [("by-girt", true), ("by-git", false)] {
        let mut args = vec!["clone", "-q"];
        args.extend(clone_args);
        args.extend([url.as_str(), name]);
        run(root.path(), &args, girt, protocol);
    }
    let compare = |step: &str| {
        let by_girt = root.path().join("by-girt");
        git(&by_girt, &["fsck", "--strict"], b"");
        assert_eq!(
            shape(&by_girt),
            shape(&root.path().join("by-git")),
            "{step}"
        );
    };
    compare("clone");
    for fetch in fetches {
        run(&root.path().join("by-girt"), fetch, true, protocol);
        run(&root.path().join("by-git"), fetch, false, protocol);
        compare(&fetch.join(" "));
    }
}

/// Protocol v0 follows `uploadpack.allow*SHA1InWant`, as Git's server does.
#[test]
fn v0_wants_match_git_under_every_policy() {
    let root = tempfile::tempdir().unwrap();
    let origin = origin(root.path(), "sha1");
    git(
        &origin,
        &["config", "uploadpack.hideRefs", "refs/heads/side"],
        b"",
    );
    commit(&origin, "untagged");
    commit(&origin, "tip");
    let hidden_tip = rev_parse(&origin, "side");
    let reachable = rev_parse(&origin, "main~1");
    let blob = String::from_utf8(git(&origin, &["hash-object", "-w", "--stdin"], b"loose\n"))
        .unwrap()
        .trim()
        .to_owned();
    let settings = [
        None,
        Some("allowTipSHA1InWant"),
        Some("allowReachableSHA1InWant"),
        Some("allowAnySHA1InWant"),
    ];
    for (index, setting) in settings.into_iter().enumerate() {
        if let Some(setting) = setting {
            git(
                &origin,
                &["config", &format!("uploadpack.{setting}"), "true"],
                b"",
            );
        }
        for want in [&hidden_tip, &reachable, &blob] {
            let fetch = |name: &str, girt: bool| {
                git(root.path(), &["init", "-q", name], b"");
                let server = upload_pack();
                let mut args = vec!["-c", "protocol.version=0", "fetch"];
                if girt {
                    args.push(&server);
                }
                args.extend([origin.to_str().unwrap(), want.as_str()]);
                let output = git_output(&root.path().join(name), &args, b"");
                (
                    output.status.success(),
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                )
            };
            let name = format!("v0-{index}-{want}");
            let (by_girt, stderr) = fetch(&name, true);
            let (by_git, _) = fetch(&format!("{name}-git"), false);
            assert_eq!(by_girt, by_git, "{setting:?} {want}: {stderr}");
        }
    }
}

/// The `--receive-pack` command naming the girt binary under test.
fn receive_pack() -> String {
    format!(
        "--receive-pack='{}' receive-pack",
        env!("CARGO_BIN_EXE_girt")
    )
}

/// What a push left behind: the client's porcelain report, its exit status and the server's refs.
#[derive(Debug, PartialEq)]
struct Pushed {
    report: String,
    success: bool,
    refs: String,
    /// The non-bare server's working-tree status, including untracked files.
    worktree: String,
}

/// Pushes from a fresh clone of a fresh server, through girt's receive-pack or Git's.
///
/// `server` configures the bare server (or a non-bare one when `bare` is false) before the push,
/// and `client` prepares the clone.
fn push_scenario(
    girt: bool,
    bare: bool,
    server: &dyn Fn(&Path),
    client: &dyn Fn(&Path),
    args: &[&str],
) -> Pushed {
    push_scenario_in("sha1", girt, bare, server, client, args)
}

fn push_scenario_in(
    format: &str,
    girt: bool,
    bare: bool,
    server: &dyn Fn(&Path),
    client: &dyn Fn(&Path),
    args: &[&str],
) -> Pushed {
    let root = tempfile::tempdir().unwrap();
    let origin = origin(root.path(), format);
    let server_path = root.path().join("server");
    let mut clone_args = vec!["clone", "-q"];
    if bare {
        clone_args.push("--bare");
    }
    clone_args.extend([origin.to_str().unwrap(), "server"]);
    git(root.path(), &clone_args, b"");
    if !bare {
        git(&server_path, &["switch", "-q", "main"], b"");
    }
    server(&server_path);
    git(
        root.path(),
        &[
            "clone",
            "-q",
            "--no-local",
            server_path.to_str().unwrap(),
            "client",
        ],
        b"",
    );
    let client_path = root.path().join("client");
    client(&client_path);
    let mut full = vec!["push", "--porcelain"];
    let command = receive_pack();
    if girt {
        full.push(&command);
    }
    full.push("origin");
    full.extend(args);
    let output = git_output(&client_path, &full, b"");
    let refs = String::from_utf8(git(
        &server_path,
        &["for-each-ref", "--format=%(objectname) %(refname)"],
        b"",
    ))
    .unwrap();
    let worktree = if bare {
        String::new()
    } else {
        String::from_utf8(git(
            &server_path,
            &["status", "--porcelain", "--untracked-files=all"],
            b"",
        ))
        .unwrap()
    };
    Pushed {
        report: String::from_utf8_lossy(&output.stdout)
            .replace(&*server_path.to_string_lossy(), "SERVER"),
        success: output.status.success(),
        refs,
        worktree,
    }
}

fn push_matches_git(bare: bool, server: &dyn Fn(&Path), client: &dyn Fn(&Path), args: &[&str]) {
    let by_girt = push_scenario(true, bare, server, client, args);
    let by_git = push_scenario(false, bare, server, client, args);
    assert_eq!(by_girt, by_git);
}

fn new_commit(repo: &Path) {
    commit(repo, "pushed");
}

fn rewind(repo: &Path) {
    git(repo, &["reset", "-q", "--hard", "HEAD~1"], b"");
    commit(repo, "rewritten");
}

fn hook(repo: &Path, name: &str, script: &str) {
    let path = repo.join("hooks").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn push_update_create_delete_matches_git() {
    push_matches_git(
        true,
        &|_| {},
        &new_commit,
        &["main", "HEAD:refs/heads/new", ":refs/heads/side"],
    );
}

#[test]
fn push_non_fast_forward_matches_git() {
    push_matches_git(true, &|_| {}, &rewind, &["+main"]);
    let deny = |repo: &Path| {
        git(
            repo,
            &["config", "receive.denyNonFastForwards", "true"],
            b"",
        )
        .clear()
    };
    push_matches_git(true, &deny, &rewind, &["+main"]);
}

#[test]
fn push_deletes_match_git() {
    let deny = |repo: &Path| git(repo, &["config", "receive.denyDeletes", "true"], b"").clear();
    push_matches_git(true, &deny, &|_| {}, &[":refs/heads/side"]);
}

#[test]
fn push_hidden_refs_match_git() {
    let hide = |repo: &Path| {
        git(
            repo,
            &["config", "receive.hideRefs", "refs/heads/side"],
            b"",
        )
        .clear()
    };
    push_matches_git(true, &hide, &new_commit, &["HEAD:refs/heads/side"]);
}

#[test]
fn push_to_checked_out_branch_matches_git() {
    push_matches_git(false, &|_| {}, &new_commit, &["main"]);
    let warn = |repo: &Path| {
        git(repo, &["config", "receive.denyCurrentBranch", "warn"], b"").clear();
    };
    push_matches_git(false, &warn, &new_commit, &["main"]);
}

#[cfg(unix)]
#[test]
fn push_hooks_match_git() {
    let pre_receive = |repo: &Path| hook(repo, "pre-receive", "echo refusing >&2; exit 1");
    push_matches_git(true, &pre_receive, &new_commit, &["main"]);
    let update = |repo: &Path| {
        hook(
            repo,
            "update",
            r#"test "$1" = refs/heads/new && exit 1; exit 0"#,
        );
    };
    push_matches_git(true, &update, &new_commit, &["main", "HEAD:refs/heads/new"]);
}

#[test]
fn sha256_push_matches_git() {
    let args = ["main", "HEAD:refs/heads/new", ":refs/heads/side"];
    assert_eq!(
        push_scenario_in("sha256", true, true, &|_| {}, &new_commit, &args),
        push_scenario_in("sha256", false, true, &|_| {}, &new_commit, &args),
    );
}

#[test]
fn stale_lease_matches_git() {
    let moved = |repo: &Path| git(repo, &["update-ref", "refs/heads/side", "main"], b"").clear();
    let server = |_: &Path| {};
    let by_girt = push_scenario(
        true,
        true,
        &server,
        &moved,
        &["--force-with-lease=side:main", "HEAD:refs/heads/side"],
    );
    let by_git = push_scenario(
        false,
        true,
        &server,
        &moved,
        &["--force-with-lease=side:main", "HEAD:refs/heads/side"],
    );
    assert_eq!(by_girt, by_git);
}

#[cfg(unix)]
#[test]
fn push_options_reach_hooks_as_with_git() {
    let server = |repo: &Path| {
        git(
            repo,
            &["config", "receive.advertisePushOptions", "true"],
            b"",
        )
        .clear();
        hook(
            repo,
            "pre-receive",
            r#"test "$GIT_PUSH_OPTION_COUNT" = 2 && test "$GIT_PUSH_OPTION_1" = second"#,
        );
    };
    push_matches_git(
        true,
        &server,
        &new_commit,
        &["-o", "first", "-o", "second", "main"],
    );
    // Options to a receiver that doesn't advertise them are refused by the client.
    push_matches_git(true, &|_| {}, &new_commit, &["-o", "first", "main"]);
}

/// Every object in `repo`, by type and name.
fn objects(repo: &Path) -> String {
    String::from_utf8(git(
        repo,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objecttype) %(objectname)",
        ],
        b"",
    ))
    .unwrap()
}

/// [`origin`] with blobs at several levels and sizes, serving filters.
fn filter_origin(root: &Path, format: &str) -> std::path::PathBuf {
    let origin = origin(root, format);
    std::fs::create_dir_all(origin.join("a/b/c")).unwrap();
    for (path, content) in [
        ("small", "x"),
        ("five", "12345"),
        ("a/top", "top\n"),
        ("a/b/mid", "mid\n"),
        ("a/b/c/deep", "deep\n"),
    ] {
        std::fs::write(origin.join(path), content).unwrap();
    }
    git(&origin, &["add", "."], b"");
    git(
        &origin,
        &["commit", "-q", "--no-gpg-sign", "-m", "tree"],
        b"",
    );
    git(&origin, &["config", "uploadpack.allowFilter", "true"], b"");
    origin
}

/// `git clone --filter` through girt's server gets the objects Git's server sends.
#[test]
fn filtered_clones_match_git() {
    for format in ["sha1", "sha256"] {
        let root = tempfile::tempdir().unwrap();
        let origin = filter_origin(root.path(), format);
        for protocol in ["protocol.version=2", "protocol.version=0"] {
            for filter in [
                "blob:none",
                "blob:limit=5",
                "tree:0",
                "tree:1",
                "tree:2",
                "object:type=commit",
                "combine:blob:limit=5+tree:3",
            ] {
                let by_girt = format!("{format}-{protocol}-{filter}-girt");
                let by_git = format!("{format}-{protocol}-{filter}-git");
                let filter_arg = format!("--filter={filter}");
                let server = upload_pack();
                for (name, girt) in [(&by_girt, true), (&by_git, false)] {
                    let mut args =
                        vec!["-c", protocol, "clone", "-q", "--no-checkout", &filter_arg];
                    if girt {
                        args.push(&server);
                    }
                    args.extend(["--no-local", origin.to_str().unwrap(), name]);
                    git(root.path(), &args, b"");
                    git(&root.path().join(name), &["fsck", "--strict"], b"");
                }
                assert_eq!(
                    objects(&root.path().join(&by_girt)),
                    objects(&root.path().join(&by_git)),
                    "{format} {protocol} {filter}"
                );
                assert_eq!(
                    refs(&root.path().join(&by_girt)),
                    refs(&root.path().join(&by_git))
                );
            }
        }
    }
}

/// A partial clone whose remote runs girt's server fetches a missing blob from it when the blob
/// is read, as Git sends `filter blob:none` with `want <blob>`, and then checks out.
#[test]
fn lazy_fetch_from_girt() {
    let root = tempfile::tempdir().unwrap();
    let origin = filter_origin(root.path(), "sha1");
    let server = upload_pack();
    git(
        root.path(),
        &[
            "-c",
            "protocol.version=2",
            "clone",
            "-q",
            "--no-checkout",
            "--filter=blob:none",
            &server,
            "--no-local",
            origin.to_str().unwrap(),
            "copy",
        ],
        b"",
    );
    let copy = root.path().join("copy");
    let blob = rev_parse(&origin, "main:a/b/c/deep");
    let listing = git(
        &copy,
        &["rev-list", "--objects", "--missing=print", "main"],
        b"",
    );
    assert!(
        String::from_utf8(listing)
            .unwrap()
            .contains(&format!("?{blob}"))
    );
    // Clone doesn't record `--upload-pack`; the lazy fetch reads it from the remote's settings.
    let command = format!("'{}' upload-pack", env!("CARGO_BIN_EXE_girt"));
    git(
        &copy,
        &["config", "remote.origin.uploadpack", &command],
        b"",
    );
    git(&copy, &["config", "protocol.version", "2"], b"");
    assert_eq!(git(&copy, &["cat-file", "-p", &blob], b""), b"deep\n");
    git(&copy, &["checkout", "-q", "main"], b"");
    assert_eq!(std::fs::read(copy.join("five")).unwrap(), b"12345");
    git(&copy, &["fsck", "--strict"], b"");
}

/// Without `uploadpack.allowFilter`, girt doesn't advertise filtering, so Git's client warns and
/// clones everything, as it does from Git's server.
#[test]
fn filter_not_allowed_clones_everything() {
    let root = tempfile::tempdir().unwrap();
    let origin = filter_origin(root.path(), "sha1");
    git(&origin, &["config", "uploadpack.allowFilter", "false"], b"");
    for protocol in ["protocol.version=2", "protocol.version=0"] {
        let mut stderrs = Vec::new();
        for girt in [true, false] {
            let name = format!("{protocol}-{girt}");
            let server = upload_pack();
            let mut args = vec!["-c", protocol, "clone", "-q", "--filter=blob:none"];
            if girt {
                args.push(&server);
            }
            args.extend(["--no-local", origin.to_str().unwrap(), &name]);
            let output = git_output(root.path(), &args, b"");
            assert!(output.status.success());
            stderrs.push(String::from_utf8(output.stderr).unwrap());
            git(&root.path().join(&name), &["fsck", "--strict"], b"");
        }
        assert_eq!(stderrs[0], stderrs[1]);
        assert!(
            stderrs[0].contains("filtering not recognized by server"),
            "{}",
            stderrs[0]
        );
        assert_eq!(
            objects(&root.path().join(format!("{protocol}-true"))),
            objects(&origin),
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn update_instead_matches_git() {
    let instead = |repo: &Path, extra: &dyn Fn(&Path)| {
        git(
            repo,
            &["config", "receive.denyCurrentBranch", "updateInstead"],
            b"",
        )
        .clear();
        extra(repo);
    };
    type Setup<'a> = &'a dyn Fn(&Path);
    let cases: [(&str, Setup<'_>); 5] = [
        ("clean", &|_| {}),
        ("unstaged", &|repo| {
            std::fs::write(repo.join("one"), "dirty\n").unwrap()
        }),
        ("staged", &|repo| {
            std::fs::write(repo.join("staged"), "new\n").unwrap();
            git(repo, &["add", "staged"], b"").clear();
        }),
        ("untracked in the way", &|repo| {
            std::fs::write(repo.join("pushed"), "mine\n").unwrap()
        }),
        ("unrelated untracked", &|repo| {
            std::fs::write(repo.join("other"), "mine\n").unwrap()
        }),
    ];
    for (name, extra) in cases {
        let server = |repo: &Path| instead(repo, extra);
        let by_girt = push_scenario(true, false, &server, &new_commit, &["main"]);
        let by_git = push_scenario(false, false, &server, &new_commit, &["main"]);
        assert_eq!(by_girt, by_git, "{name}");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn update_instead_refused_by_other_policy_leaves_worktree_as_git_does() {
    let server = |repo: &Path| {
        git(
            repo,
            &["config", "receive.denyCurrentBranch", "updateInstead"],
            b"",
        )
        .clear();
        git(
            repo,
            &["config", "receive.denyNonFastForwards", "true"],
            b"",
        )
        .clear();
    };
    push_matches_git(false, &server, &rewind, &["+main"]);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn update_instead_with_line_ending_conversion_matches_git() {
    let server = |repo: &Path| {
        git(
            repo,
            &["config", "receive.denyCurrentBranch", "updateInstead"],
            b"",
        )
        .clear();
        git(repo, &["config", "core.autocrlf", "true"], b"").clear();
        // Rewrite the working files the way autocrlf checks them out, keeping the tree clean.
        for name in ["one", "two", "side"] {
            let path = repo.join(name);
            if path.exists() {
                std::fs::remove_file(&path).unwrap();
            }
        }
        git(repo, &["checkout", "--", "."], b"").clear();
    };
    push_matches_git(false, &server, &new_commit, &["main"]);
}
