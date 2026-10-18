//! Isolated Git comparisons for fsck reporting and recovery flags.
#[path = "../../tests/support/apply_git.rs"]
mod support;
use std::path::Path;
use std::process::{Command, Output};

fn girt(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_girt"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Fsck Author")
        .env("GIT_AUTHOR_EMAIL", "fsck@example.com")
        .env("GIT_COMMITTER_NAME", "Fsck Author")
        .env("GIT_COMMITTER_EMAIL", "fsck@example.com")
        .env("GIT_AUTHOR_DATE", "2001-09-17T00:00:00+0000")
        .env("GIT_COMMITTER_DATE", "2001-09-17T00:00:00+0000")
        .args(args)
        .output()
        .unwrap()
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    support::git(root.path(), &["init", "--bare", "--template=", "."], b"");
    let tree = support::git(root.path(), &["mktree"], b"");
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let first = support::git(root.path(), &["commit-tree", tree], b"first\n");
    let first = std::str::from_utf8(&first).unwrap().trim();
    support::git(
        root.path(),
        &["update-ref", "--create-reflog", "refs/heads/main", first],
        b"",
    );
    let second = support::git(root.path(), &["commit-tree", tree], b"second\n");
    support::git(
        root.path(),
        &[
            "update-ref",
            "refs/heads/main",
            std::str::from_utf8(&second).unwrap().trim(),
        ],
        b"",
    );
    support::git(
        root.path(),
        &["symbolic-ref", "HEAD", "refs/heads/main"],
        b"",
    );
    support::git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"dangling blob\n",
    );
    root
}

fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<_> = bytes
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    lines.sort();
    lines
}

#[test]
fn dangling_unreachable_and_reflog_flags_match_git() {
    let root = fixture();
    for flags in [
        vec![],
        vec!["--no-dangling"],
        vec!["--unreachable"],
        vec!["--no-reflogs"],
        vec!["--connectivity-only"],
        vec!["--lost-found"],
    ] {
        let mut args = vec!["fsck"];
        args.extend(flags);
        let expected = support::git_output(root.path(), &args, b"");
        let actual = girt(root.path(), &args);
        assert_eq!(
            actual.status.success(),
            expected.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&actual.stderr)
        );
        assert_eq!(lines(&actual.stdout), lines(&expected.stdout), "{args:?}");
        assert_eq!(actual.stderr, expected.stderr, "{args:?}");
    }
}

#[test]
fn configured_warning_and_strict_error_match_git() {
    let root = fixture();
    let blob = support::git(root.path(), &["hash-object", "-w", "--stdin"], b"payload");
    let blob = girt::ObjectId::from_hex(
        girt::ObjectFormat::Sha1,
        std::str::from_utf8(&blob).unwrap().trim(),
    )
    .unwrap();
    support::git(
        root.path(),
        &["hash-object", "-t", "tree", "--literally", "-w", "--stdin"],
        &[b"100644 .git\0".as_slice(), blob.as_bytes()].concat(),
    );
    for flags in [vec!["--no-dangling"], vec!["--no-dangling", "--strict"]] {
        let mut args = vec!["fsck"];
        args.extend(flags);
        let expected = support::git_output(root.path(), &args, b"");
        let actual = girt(root.path(), &args);
        assert_eq!(actual.status.success(), expected.status.success());
        assert_eq!(actual.stdout, expected.stdout);
        assert_eq!(actual.stderr, expected.stderr);
    }
}

#[test]
fn broken_tree_links_match_git() {
    let root = fixture();
    let missing = girt::ObjectId::for_blob(girt::ObjectFormat::Sha1, b"missing");
    let tree = support::git(
        root.path(),
        &["hash-object", "-t", "tree", "--literally", "-w", "--stdin"],
        &[b"100644 absent\0".as_slice(), missing.as_bytes()].concat(),
    );
    support::git(
        root.path(),
        &[
            "update-ref",
            "refs/tags/broken-tree",
            std::str::from_utf8(&tree).unwrap().trim(),
        ],
        b"",
    );

    let expected = support::git_output(root.path(), &["fsck", "--no-dangling"], b"");
    let actual = girt(root.path(), &["fsck", "--no-dangling"]);

    assert!(!actual.status.success());
    assert_eq!(actual.stdout, expected.stdout);
    assert_eq!(actual.stderr, expected.stderr);
}
