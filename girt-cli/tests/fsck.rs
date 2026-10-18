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

fn assert_metadata_payload_matches_git(root: &Path, kind: &str, payload: &[u8]) {
    support::git(
        root,
        &["hash-object", "-t", kind, "--literally", "-w", "--stdin"],
        payload,
    );
    for args in [
        vec!["fsck", "--no-dangling"],
        vec!["fsck", "--no-dangling", "--strict"],
    ] {
        let expected = support::git_output(root, &args, b"");
        let actual = girt(root, &args);
        assert_eq!(actual.status.code(), expected.status.code(), "{payload:?}");
        assert_eq!(actual.stdout, expected.stdout, "{payload:?}");
        assert_eq!(actual.stderr, expected.stderr, "{payload:?}");
    }
}

fn metadata_fixture() -> (tempfile::TempDir, String) {
    let root = fixture();
    let tree = support::git(root.path(), &["rev-parse", "refs/heads/main^{tree}"], b"");
    let tree = std::str::from_utf8(&tree).unwrap().trim().to_owned();
    (root, tree)
}

#[test]
fn physical_commit_header_diagnostics_match_git_exactly() {
    for case in [
        "missing-author",
        "missing-committer",
        "duplicate-author",
        "unterminated",
        "nul-header",
        "nul-body",
    ] {
        let (root, tree) = metadata_fixture();
        let t = format!("tree {tree}\n");
        let a = "author A <a@example.invalid> 1000000000 +0000\n";
        let c = "committer C <c@example.invalid> 1000000000 +0000\n";
        let payload = match case {
            "missing-author" => format!("{t}{c}\nx\n"),
            "missing-committer" => format!("{t}{a}\nx\n"),
            "duplicate-author" => format!("{t}{a}{a}{c}\nx\n"),
            "unterminated" => format!("{t}{a}{}", c.trim_end()),
            "nul-header" => format!("{t}{}{c}\nx\n", a.replace("A", "A\0")),
            "nul-body" => format!("{t}{a}{c}\nx\0y\n"),
            _ => unreachable!(),
        };
        assert_metadata_payload_matches_git(root.path(), "commit", payload.as_bytes());
    }
}

#[test]
fn identity_diagnostics_match_git_exactly() {
    for person in [
        "A<a> 1000000000 +0000",
        "<a> 1000000000 +0000",
        "A no-email 1000000000 +0000",
        "A <a 1000000000 +0000",
        "A a> 1000000000 +0000",
        "A <a>1000000000 +0000",
        "A <a> -1 +0000",
        "A <a> 0123 +0000",
        "A <a> 18446744073709551616 +0000",
        "A <a> 1000000000 +00",
        "A <a> 1000000000 +9999",
        " <> 1000000000 +0000",
    ] {
        for role in ["author", "committer"] {
            let (root, tree) = metadata_fixture();
            let ordinary = "Ordinary <ordinary@example.invalid> 1000000000 +0000";
            let (author, committer) = if role == "author" {
                (person, ordinary)
            } else {
                (ordinary, person)
            };
            let payload =
                format!("tree {tree}\nauthor {author}\ncommitter {committer}\n\nmessage\n");
            assert_metadata_payload_matches_git(root.path(), "commit", payload.as_bytes());
        }
    }
}

#[test]
fn annotated_tag_diagnostics_match_git_exactly() {
    for case in [
        "bad-name",
        "bad-name-and-tagger",
        "missing-tagger",
        "misplaced-tagger",
        "bad-tagger",
        "nul-header",
        "unterminated",
        "nul-body",
        "non-utf8-name",
    ] {
        let (root, tree) = metadata_fixture();
        let prefix = format!("object {tree}\ntype tree\n");
        let person = "tagger A <a@example.invalid> 1000000000 +0000\n";
        let payload = match case {
            "bad-name-and-tagger" => {
                format!("{prefix}tag a..b\ntagger A noemail\n\nx\n").into_bytes()
            }
            "bad-name" => format!("{prefix}tag a..b\n{person}\nx\n").into_bytes(),
            "missing-tagger" => format!("{prefix}tag v1\n\nx\n").into_bytes(),
            "misplaced-tagger" => format!("{prefix}tag v1\nextra x\n{person}\nx\n").into_bytes(),
            "bad-tagger" => format!("{prefix}tag v1\ntagger A noemail\n\nx\n").into_bytes(),
            "nul-header" => {
                format!("{prefix}tag v1\n{}\nx\n", person.replace("A", "A\0")).into_bytes()
            }
            "unterminated" => format!("{prefix}tag v1\n{}", person.trim_end()).into_bytes(),
            "nul-body" => format!("{prefix}tag v1\n{person}\nx\0y\n").into_bytes(),
            "non-utf8-name" => [
                prefix.as_bytes(),
                b"tag bad \xff\n",
                person.as_bytes(),
                b"\nx\n",
            ]
            .concat(),
            _ => unreachable!(),
        };
        assert_metadata_payload_matches_git(root.path(), "tag", &payload);
    }
}
