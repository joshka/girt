//! `working-tree-encoding` compared with `git add` and `git checkout-index`.
#[path = "support/pack_git.rs"]
#[allow(dead_code)]
mod pack_git;

use std::path::Path;

use girt::filter::{Commands, Error, Staged};
use girt::workflow::Environment;
use girt::{ObjectId, Repository};
use pack_git::{git, git_output};

const ENCODINGS: &[&str] = &[
    "UTF-16",
    "UTF-16LE",
    "UTF-16BE",
    "UTF-16LE-BOM",
    "UTF-32",
    "UTF-32LE",
    "UTF-32BE",
];

fn repository(root: &Path) {
    git(root, &["init", "-q", "."], b"");
    git(root, &["config", "core.attributesFile", "absent"], b"");
    let mut attributes = String::new();
    for (index, encoding) in ENCODINGS.iter().enumerate() {
        attributes.push_str(&format!("e{index} working-tree-encoding={encoding}\n"));
        attributes.push_str(&format!(
            "c{index} working-tree-encoding={encoding} text eol=crlf\n"
        ));
    }
    attributes.push_str("latin working-tree-encoding=latin1\n");
    std::fs::write(root.join(".gitattributes"), attributes).unwrap();
}

fn filters(root: &Path) -> girt::filter::Pipeline {
    let environment = Environment::default().with("GIT_ATTR_NOSYSTEM", "1");
    Repository::open(root)
        .unwrap()
        .filters(&environment, Commands::Refuse)
        .unwrap()
}

/// Blobs written with `git checkout-index` are what Git writes; storing them back with `git add`
/// is what Git stores. girt must agree with both.
#[test]
fn round_trip_matches_git() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root);
    let blobs: &[&[u8]] = &["hé\n😀\n".as_bytes(), b"", b"plain\n"];
    for blob in blobs {
        let id = git(
            root,
            &["hash-object", "-w", "--no-filters", "--stdin"],
            blob,
        );
        let id = String::from_utf8(id).unwrap().trim().to_owned();
        let mut names = Vec::new();
        for index in 0..ENCODINGS.len() {
            for prefix in ["e", "c"] {
                let name = format!("{prefix}{index}");
                let info = format!("100644,{id},{name}");
                git(root, &["update-index", "--add", "--cacheinfo", &info], b"");
                names.push(name);
            }
        }
        git(root, &["checkout-index", "-a", "-f"], b"");
        let id: ObjectId = id.parse().unwrap();
        let mut filters = filters(root);
        for name in &names {
            let written = std::fs::read(root.join(name)).unwrap();
            let ours = filters.to_worktree(name.as_bytes(), blob, id).unwrap();
            assert_eq!(*ours.bytes, *written, "{name} writing {blob:?}");
            assert!(!ours.unencoded);

            git(root, &["add", name], b"");
            let stored = git(root, &["cat-file", "blob", &format!(":{name}")], b"");
            let ours = filters
                .to_repository(name.as_bytes(), &written, Staged::Absent)
                .unwrap();
            assert_eq!(*ours.bytes, *stored, "{name} storing {blob:?}");
        }
    }
}

#[test]
fn invalid_files_are_refused_as_git_refuses_them() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root);
    let mut filters = filters(root);
    // e0 is UTF-16 (BOM required), e1 is UTF-16LE (BOM prohibited).
    for (name, bytes) in [
        ("e0", &b"h\x00"[..]),
        ("e1", b"\xff\xfeh\x00"),
        ("e1", b"h\x00\x00"),
        ("e1", b"\x00\xd8a\x00"),
    ] {
        std::fs::write(root.join(name), bytes).unwrap();
        let output = git_output(root, &["add", name], b"");
        assert_eq!(output.status.code(), Some(128), "git {name} {bytes:?}");
        let error = filters
            .to_repository(name.as_bytes(), bytes, Staged::Absent)
            .unwrap_err();
        assert!(matches!(error, Error::Encoding { .. }), "{error:?}");
    }
}

#[test]
fn invalid_blobs_are_written_unencoded_as_git_writes_them() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root);
    let blob = b"h\xff\n";
    let id = git(
        root,
        &["hash-object", "-w", "--no-filters", "--stdin"],
        blob,
    );
    let id = String::from_utf8(id).unwrap().trim().to_owned();
    git(
        root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{id},e1"),
        ],
        b"",
    );
    git(root, &["checkout-index", "-f", "e1"], b"");
    let written = std::fs::read(root.join("e1")).unwrap();
    let ours = filters(root)
        .to_worktree(b"e1", blob, id.parse().unwrap())
        .unwrap();
    assert_eq!(*ours.bytes, *written);
    assert!(ours.unencoded);
}

#[test]
fn other_encodings_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root);
    let error = filters(root)
        .to_repository(b"latin", b"x", Staged::Absent)
        .unwrap_err();
    assert!(matches!(error, Error::Unsupported { .. }), "{error:?}");
}

#[test]
fn bom_variant_accepts_either_mark_as_git_does() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root);
    let mut filters = filters(root);
    // e3 is UTF-16LE-BOM.
    for bytes in [
        &b"\xfe\xff\x00h\x00\n"[..],
        b"\xff\xfeh\x00\n\x00",
        b"\x00h\x00\n",
    ] {
        std::fs::write(root.join("e3"), bytes).unwrap();
        git(root, &["add", "e3"], b"");
        let stored = git(root, &["cat-file", "blob", ":e3"], b"");
        let ours = filters.to_repository(b"e3", bytes, Staged::Absent).unwrap();
        assert_eq!(*ours.bytes, *stored, "{bytes:?}");
    }
}
