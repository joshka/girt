//! Original fixtures generated through Git's CLI; no upstream source or fixtures are used.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use girt::index::{Entry, Index, Limits, Mode, Stage};
use girt::tree::TreeEditor;
use girt::{
    Commit, CommitPayload, InitKind, ObjectFormat, ObjectId, Repository, Signature, tree_diff,
};
use rstest::rstest;

fn git(path: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut cmd = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    let mut child = cmd
        .current_dir(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.join("absent"))
        .env("GIT_AUTHOR_NAME", "Writer")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "Writer")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
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
fn id(bytes: &[u8]) -> ObjectId {
    std::str::from_utf8(bytes).unwrap().trim().parse().unwrap()
}
fn person() -> Signature {
    Signature {
        name: b"Writer".to_vec(),
        email: b"a@example.com".to_vec(),
        seconds: 1700000000,
        offset_minutes: 0,
    }
}

#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn nested_index_editor_commit_and_rename_match_git(#[case] format: ObjectFormat) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, dir.path(), InitKind::Worktree).unwrap();
    let loose = repo.loose_objects();
    let blob = loose.write_blob(b"contents\n").unwrap();
    let executable = loose.write_blob(b"#!/bin/sh\n").unwrap();
    let entries = vec![
        Entry::new(b"dir/file".to_vec(), Mode::Regular, blob),
        Entry::new(b"dir/sub/run".to_vec(), Mode::Executable, executable),
    ];
    let index = Index::new(format, entries, Limits::default()).unwrap();
    std::fs::write(
        repo.git_dir().join("index"),
        index.encode(Limits::default()).unwrap(),
    )
    .unwrap();
    let expected = id(&git(dir.path(), &["write-tree"], b""));
    assert_eq!(index.write_tree(&repo).unwrap(), expected);
    assert_eq!(index.tree_id(), Some(expected));
    let mut editor = TreeEditor::new(&loose);
    editor.set(b"dir/file", blob).unwrap();
    editor
        .set_entry(
            b"dir/sub/run",
            girt::TreeValue {
                mode: girt::EntryMode::Executable,
                id: executable,
            },
        )
        .unwrap();
    assert_eq!(editor.write().unwrap(), expected);
    assert!(editor.set(b"../bad", blob).is_err());
    assert_eq!(editor.write().unwrap(), expected);
    editor.remove(b"dir/sub").unwrap();
    editor.remove(b"absent").unwrap();
    editor.remove(b"dir/file").unwrap();
    editor.set(b"new/file", blob).unwrap();
    let new = editor.write().unwrap();
    let commit = Commit::builder(expected, person(), b"Subject\n\nBody\n")
        .build()
        .unwrap();
    let git_commit = id(&git(
        dir.path(),
        &["commit-tree", &expected.to_string()],
        commit.message(),
    ));
    assert_eq!(commit.id(), git_commit);
    loose.write_commit(&commit).unwrap();
    assert_eq!(commit.subject(), b"Subject");
    let mut seen = Vec::new();
    let signed = Commit::builder(expected, person(), b"Subject\n\nBody\n")
        .header(b"custom", b"one\ntwo")
        .build_signed(|payload| {
            seen = payload.to_vec();
            Ok::<_, std::io::Error>(b"signature\nsecond line".to_vec())
        })
        .unwrap();
    assert_eq!(
        signed.header(b"custom").unwrap().unfolded_value(),
        b"one\ntwo"
    );
    let payload = CommitPayload::parse(signed.as_bytes()).unwrap();
    let signature_index = payload
        .headers()
        .position(|h| {
            h.name
                == if format == ObjectFormat::Sha1 {
                    b"gpgsig".as_slice()
                } else {
                    b"gpgsig-sha256".as_slice()
                }
        })
        .unwrap();
    assert_eq!(payload.without_headers(&[signature_index]).unwrap(), seen);
    let objects = repo.objects().unwrap();
    let diff = objects
        .diff_with_renames(Some(expected), Some(new))
        .unwrap();
    assert_eq!(diff.entries.len(), 2);
    assert!(diff.entries.iter().any(|e| matches!(e, tree_diff::Entry::Rewrite { source, target, similarity: 100, .. } if source == b"dir/file" && target == b"new/file")));
    let oracle = git(
        dir.path(),
        &[
            "diff",
            "--name-status",
            "-M",
            &expected.to_string(),
            &new.to_string(),
        ],
        b"",
    );
    assert_eq!(oracle, b"D\tdir/sub/run\nR100\tdir/file\tnew/file\n");
    assert!(
        objects
            .diff_with_renames_with(
                Some(expected),
                Some(new),
                Default::default(),
                Default::default(),
                &AtomicBool::new(true)
            )
            .is_err()
    );
}

#[test]
fn index_refuses_conflicts_and_omits_intent_to_add() {
    let dir = tempfile::tempdir().unwrap();
    let repo =
        Repository::init(ObjectFormat::Sha1, dir.path().join("repo"), InitKind::Bare).unwrap();
    let loose = repo.loose_objects();
    let blob = loose.write_blob(b"x").unwrap();
    let conflict = Index::new(
        ObjectFormat::Sha1,
        vec![Entry::new(b"x".to_vec(), Mode::Regular, blob).with_stage(Stage::Ours)],
        Limits::default(),
    )
    .unwrap();
    assert!(conflict.write_tree(&repo).is_err());
    let intent = Index::new(
        ObjectFormat::Sha1,
        vec![
            Entry::new(
                b"x".to_vec(),
                Mode::Regular,
                ObjectId::null(ObjectFormat::Sha1),
            )
            .with_intent_to_add(true),
        ],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        intent.write_tree(&repo).unwrap(),
        Index::empty(ObjectFormat::Sha1).write_tree(&repo).unwrap()
    );
}

#[test]
fn builder_validates_before_signing_and_preserves_signer_errors() {
    let tree = girt::Tree::new(ObjectFormat::Sha1, vec![]).unwrap().id();
    let mut called = false;
    let duplicate = Commit::builder(tree, person(), b"message")
        .header(b"gpgsig", b"existing")
        .build_signed(|_| {
            called = true;
            Ok::<_, std::io::Error>(Vec::new())
        });
    assert!(matches!(
        duplicate,
        Err(girt::commit::SignError::ExistingSignature)
    ));
    assert!(!called);
    let invalid = Commit::builder(tree, person(), b"message")
        .header(b"bad header", b"value")
        .build_signed(|_| {
            called = true;
            Ok::<_, std::io::Error>(Vec::new())
        });
    assert!(matches!(invalid, Err(girt::commit::SignError::Commit(_))));
    assert!(!called);
    let refused = Commit::builder(tree, person(), b"message")
        .build_signed(|_| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)));
    assert!(
        matches!(refused, Err(girt::commit::SignError::Signer(error)) if error.kind() == std::io::ErrorKind::PermissionDenied)
    );
    let invalid_signature = Commit::builder(tree, person(), b"message")
        .build_signed(|_| Ok::<_, std::io::Error>(b"bad\rsignature".to_vec()));
    assert!(matches!(
        invalid_signature,
        Err(girt::commit::SignError::Commit(_))
    ));
}

#[test]
fn configured_builder_resolves_distinct_author_and_committer() {
    let config = girt::Config::parse(b"[user]\nname = Writer\nemail = a@example.com\n").unwrap();
    let environment = girt::workflow::Environment::default()
        .with("GIT_AUTHOR_DATE", "@1700000000 +0130")
        .with("GIT_COMMITTER_DATE", "@1700000001 -0230")
        .with("GIT_COMMITTER_NAME", "Recorder");
    let tree = girt::Tree::new(ObjectFormat::Sha1, vec![]).unwrap().id();
    let commit =
        girt::commit::CommitBuilder::from_config(tree, b"\xffsubject\nbody", &config, &environment)
            .unwrap()
            .parent(tree)
            .parent(tree)
            .header(b"x", b"first")
            .header(b"x", b"second")
            .build()
            .unwrap();
    let fields = commit.to_fields().unwrap();
    assert_eq!(fields.author.name, b"Writer");
    assert_eq!(fields.author.offset_minutes, 90);
    assert_eq!(fields.committer.name, b"Recorder");
    assert_eq!(fields.committer.seconds, 1700000001);
    assert_eq!(fields.committer.offset_minutes, -150);
    assert_eq!(commit.parents(), &[tree, tree]);
    assert_eq!(commit.header(b"x").unwrap().value, b"first");
    assert!(commit.header(b"X").is_none());
    assert_eq!(commit.subject(), b"\xffsubject body");
}

#[test]
fn editor_replaces_ancestors_descendants_and_keeps_prefix_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, dir.path().join("r"), InitKind::Bare).unwrap();
    let loose = repo.loose_objects();
    let blob = loose.write_blob(b"data").unwrap();
    let mut editor = TreeEditor::new(&loose);
    editor.set(b"a/child", blob).unwrap();
    editor.set(b"a-/sibling", blob).unwrap();
    editor.set(b"a", blob).unwrap();
    let root = loose.read_tree(editor.write().unwrap(), 4096).unwrap();
    assert_eq!(root.entries().len(), 2);
    assert!(
        root.entries()
            .iter()
            .any(|e| e.name == b"a" && e.mode == girt::EntryMode::Blob)
    );
    editor.set(b"a/deep/child", blob).unwrap();
    editor.remove(b"a").unwrap();
    let root = loose.read_tree(editor.write().unwrap(), 4096).unwrap();
    assert_eq!(root.entries().len(), 1);
    assert_eq!(root.entries()[0].name, b"a-");
    let before = editor.write().unwrap();
    for path in [
        &b""[..],
        b"/a",
        b"a/",
        b"a//b",
        b"a/./b",
        b"a/../b",
        b"a\0b",
    ] {
        assert!(editor.set(path, blob).is_err());
        assert!(editor.remove(path).is_err());
    }
    assert!(
        editor
            .set(b"foreign", ObjectId::null(ObjectFormat::Sha256))
            .is_err()
    );
    assert_eq!(editor.write().unwrap(), before);
}

#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn subjects_and_special_index_modes_match_git(#[case] format: ObjectFormat) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, dir.path(), InitKind::Worktree).unwrap();
    let loose = repo.loose_objects();
    let tree = Index::empty(format).write_tree(&repo).unwrap();
    for message in [
        &b"line one\nline two\n\nbody"[..],
        b"\n\n  leading  \nnext\t\n\nbody",
        b" \t\nfirst\n \t\nsecond",
        b"\r\nA\r\nB\r\n\r\nbody",
        b"",
        b"\xffsubject\nbody",
    ] {
        let commit = Commit::builder(tree, person(), message).build().unwrap();
        loose.write_commit(&commit).unwrap();
        let expected = git(
            dir.path(),
            &["show", "-s", "--format=%s", &commit.id().to_string()],
            b"",
        );
        assert_eq!(
            commit.subject(),
            expected.strip_suffix(b"\n").unwrap(),
            "{message:?}"
        );
    }
    let link = loose.write_blob(b"../target").unwrap();
    let absent_commit = ObjectId::for_blob(format, b"not stored");
    let subtree = girt::Tree::new(
        format,
        vec![girt::TreeEntry {
            name: b"link".to_vec(),
            mode: girt::EntryMode::Symlink,
            id: link,
        }],
    )
    .unwrap();
    loose.write_tree(&subtree).unwrap();
    let sparse = Index::new(
        format,
        vec![
            Entry::new(b"module".to_vec(), Mode::Gitlink, absent_commit),
            Entry::new(b"sparse/".to_vec(), Mode::SparseDirectory, subtree.id())
                .with_skip_worktree(true),
            Entry::new(b"symlink".to_vec(), Mode::Symlink, link),
        ],
        Limits::default(),
    )
    .unwrap();
    std::fs::write(
        repo.git_dir().join("index"),
        sparse.encode(Limits::default()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        sparse.write_tree(&repo).unwrap(),
        id(&git(dir.path(), &["write-tree"], b""))
    );
    let mut editor = TreeEditor::new(&loose);
    editor
        .set_entry(
            b"stored",
            girt::TreeValue {
                mode: girt::EntryMode::Tree,
                id: subtree.id(),
            },
        )
        .unwrap();
    let before = editor.write().unwrap();
    assert!(matches!(
        editor.set(b"stored/file", link),
        Err(girt::tree::TreeWriteError::StoredSubtree { .. })
    ));
    assert!(matches!(
        editor.remove(b"stored/link"),
        Err(girt::tree::TreeWriteError::StoredSubtree { .. })
    ));
    assert_eq!(editor.write().unwrap(), before);
    let missing = Index::new(
        format,
        vec![Entry::new(b"absent".to_vec(), Mode::Regular, absent_commit)],
        Limits::default(),
    )
    .unwrap();
    assert!(
        matches!(missing.write_tree(&repo), Err(girt::index::WriteTreeError::MissingObject { id, .. }) if id == absent_commit)
    );
    assert!(matches!(
        missing.write_tree_with(&repo, Default::default(), &AtomicBool::new(true)),
        Err(girt::index::WriteTreeError::Cancelled)
    ));
}

#[cfg(unix)]
#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn signing_armor_framing_and_header_match_git(#[case] format: ObjectFormat) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, dir.path(), InitKind::Worktree).unwrap();
    let key = dir.path().join("signing-key");
    let keygen = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(
        keygen.status.success(),
        "{}",
        String::from_utf8_lossy(&keygen.stderr)
    );
    let tree = Index::empty(format).write_tree(&repo).unwrap();
    let message = b"Signed snapshot\n";
    let git_id = id(&git(
        dir.path(),
        &[
            "-c",
            "gpg.format=ssh",
            "-c",
            &format!("user.signingkey={}", key.display()),
            "commit-tree",
            "-S",
            &tree.to_string(),
        ],
        message,
    ));
    let bytes = git(
        dir.path(),
        &["cat-file", "commit", &git_id.to_string()],
        b"",
    );
    let expected = Commit::parse(format, &bytes).unwrap();
    let header = if format == ObjectFormat::Sha1 {
        b"gpgsig".as_slice()
    } else {
        b"gpgsig-sha256".as_slice()
    };
    let signature = expected.header(header).unwrap().unfolded_value();
    assert!(signature.ends_with(b"-----END SSH SIGNATURE-----"));
    let unsigned = Commit::builder(tree, person(), message).build().unwrap();
    let signed = Commit::builder(tree, person(), message)
        .build_signed(|payload| {
            assert_eq!(payload, unsigned.as_bytes());
            Ok::<_, std::io::Error>([signature.as_slice(), b"\n"].concat())
        })
        .unwrap();
    assert_eq!(signed.as_bytes(), bytes);
    assert_eq!(signed.id(), git_id);
    for name in [b"gpgsig".as_slice(), b"gpgsig-sha256"] {
        let result = Commit::builder(tree, person(), message)
            .header(name, b"existing")
            .build_signed(|_| -> Result<Vec<u8>, std::io::Error> { panic!("must not sign twice") });
        assert!(matches!(
            result,
            Err(girt::commit::SignError::ExistingSignature)
        ));
    }
}

#[test]
fn rename_limit_keeps_exact_matches_and_reports_skipped_inference() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, dir.path().join("r"), InitKind::Bare).unwrap();
    let loose = repo.loose_objects();
    let mut editor = TreeEditor::new(&loose);
    let exact = loose.write_blob(b"exact content\n").unwrap();
    editor.set(b"old/exact", exact).unwrap();
    editor
        .set(
            b"old/edited",
            loose.write_blob(b"some old content\n").unwrap(),
        )
        .unwrap();
    let old = editor.write().unwrap();
    editor.remove(b"old").unwrap();
    editor.set(b"new/exact", exact).unwrap();
    editor
        .set(
            b"new/different",
            loose.write_blob(b"some different content\n").unwrap(),
        )
        .unwrap();
    let new = editor.write().unwrap();
    let objects = repo.objects().unwrap();
    let report = objects
        .diff_with_renames_with(
            Some(old),
            Some(new),
            girt::rewrites::Options::default().with_candidate_limit(0),
            Default::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.inexact_skipped.is_some());
    assert_eq!(report.entries.len(), 3);
    assert!(matches!(
        &report.entries[1],
        tree_diff::Entry::Rewrite {
            similarity: 100,
            ..
        }
    ));
    let paths: Vec<_> = report
        .entries
        .iter()
        .map(tree_diff::Entry::sort_path)
        .collect();
    assert_eq!(
        paths,
        [b"new/different".as_slice(), b"new/exact", b"old/edited"]
    );
    let oracle = git(
        repo.git_dir(),
        &[
            "-c",
            "diff.renameLimit=1",
            "diff",
            "--name-status",
            "-M",
            &old.to_string(),
            &new.to_string(),
        ],
        b"",
    );
    assert_eq!(
        oracle,
        b"A\tnew/different\nR100\told/exact\tnew/exact\nD\told/edited\n"
    );
    let result = objects.diff_with_renames_with(
        Some(old),
        Some(new),
        Default::default(),
        girt::rewrites::Limits::default().with_max_pairs(0),
        &AtomicBool::new(false),
    );
    assert!(result.is_err());
}

#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn write_tree_rechecks_shared_objects_between_calls_and_preflights_before_publication(
    #[case] format: ObjectFormat,
) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, dir.path(), InitKind::Worktree).unwrap();
    let loose = repo.loose_objects();
    let blob = loose.write_blob(b"shared content\n").unwrap();
    let entries = vec![
        Entry::new(b"a/one".to_vec(), Mode::Regular, blob),
        Entry::new(b"b/two".to_vec(), Mode::Regular, blob),
    ];
    let index = Index::new(format, entries.clone(), Limits::default()).unwrap();
    let root = index.write_tree(&repo).unwrap();
    assert_eq!(Some(root), index.tree_id());
    let hex = blob.to_string();
    std::fs::remove_file(repo.object_dir().join(&hex[..2]).join(&hex[2..])).unwrap();
    assert!(
        matches!(index.write_tree(&repo), Err(girt::index::WriteTreeError::MissingObject { id, .. }) if id == blob)
    );
    // A gitlink with the same ID must not exempt a later regular file from presence checks.
    let exempt_first = Index::new(
        format,
        vec![
            Entry::new(b"a-module".to_vec(), Mode::Gitlink, blob),
            Entry::new(b"file".to_vec(), Mode::Regular, blob),
        ],
        Limits::default(),
    )
    .unwrap();
    assert!(
        matches!(exempt_first.write_tree(&repo), Err(girt::index::WriteTreeError::MissingObject { id, .. }) if id == blob)
    );
    let fresh = Repository::init(format, dir.path().join("fresh"), InitKind::Bare).unwrap();
    fresh
        .loose_objects()
        .write_blob(b"shared content\n")
        .unwrap();
    let mut incomplete = entries;
    incomplete.push(Entry::new(
        b"z/missing".to_vec(),
        Mode::Regular,
        ObjectId::for_blob(format, b"absent"),
    ));
    let incomplete = Index::new(format, incomplete, Limits::default()).unwrap();
    let subtree = girt::Tree::new(
        format,
        vec![girt::TreeEntry {
            name: b"one".to_vec(),
            mode: girt::EntryMode::Blob,
            id: blob,
        }],
    )
    .unwrap()
    .id();
    assert!(
        matches!(incomplete.write_tree(&fresh), Err(girt::index::WriteTreeError::MissingObject { id, .. }) if id == ObjectId::for_blob(format, b"absent"))
    );
    assert!(!fresh.objects().unwrap().contains(subtree).unwrap());
    assert!(
        !fresh
            .objects()
            .unwrap()
            .contains(incomplete.tree_id().unwrap())
            .unwrap()
    );
}
