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
    let origin = origin.to_str().unwrap();
    let mut args = vec!["-c", "protocol.version=2", "clone", "-q"];
    let server = upload_pack();
    if girt {
        args.push(&server);
    }
    args.extend(["--no-local", origin, name]);
    git(root, &args, b"");
    git(&root.join(name), &["fsck", "--strict"], b"");
}

fn fetch(repo: &Path, girt: bool) {
    let server = upload_pack();
    let mut args = vec!["-c", "protocol.version=2", "fetch", "-q"];
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

fn clone_and_fetch_match_git(format: &str) {
    let root = tempfile::tempdir().unwrap();
    let origin = origin(root.path(), format);
    clone(root.path(), &origin, "by-girt", true);
    clone(root.path(), &origin, "by-git", false);
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
    fetch(&root.path().join("by-girt"), true);
    fetch(&root.path().join("by-git"), false);
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
