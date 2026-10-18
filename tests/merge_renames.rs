//! Original rename histories; only Git's executable is an oracle.
#[path = "support/rewrite_git.rs"]
mod fixture;
#[path = "support/merge_git.rs"]
mod oracle;

use fixture::{Fixture, id};
use girt::merge::{ConflictKind, RenameError};
use girt::{ObjectId, Tree};
use rstest::rstest;

fn oracle(f: &Fixture, base: ObjectId, ours: ObjectId, theirs: ObjectId) -> std::process::Output {
    let b = oracle::commit(f.root.path(), base, None);
    let o = oracle::commit(f.root.path(), ours, Some(b));
    let t = oracle::commit(f.root.path(), theirs, Some(b));
    oracle::merge_tree(f.root.path(), &o.to_string(), &t.to_string())
}

fn oracle_tree(output: &std::process::Output) -> ObjectId {
    id(output.stdout.split(|b| *b == b'\n').next().unwrap())
}

#[rstest]
fn carries_opposite_modification_to_exact_rename(#[values("sha1", "sha256")] format: &str) {
    let f = Fixture::new(format);
    let b = f.blob(b"one\ntwo\nthree\n");
    let t = f.blob(b"one\nTWO\nthree\n");
    let base = f.tree(&[("100644", b, b"old")]);
    let ours = f.tree(&[("100644", b, b"new")]);
    let theirs = f.tree(&[("100644", t, b"old")]);
    let result = f.objects().merge_trees(base, ours, theirs).unwrap();
    let expected = oracle(&f, base, ours, theirs);
    assert_eq!(expected.status.code(), Some(0));
    assert!(result.is_clean());
    assert_eq!(result.tree.id(), oracle_tree(&expected));
}

#[test]
fn similarity_rename_keeps_both_content_edits() {
    let f = Fixture::new("sha1");
    let b = f.blob(b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n");
    let o = f.blob(b"ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n");
    let t = f.blob(b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nTEN\n");
    let base = f.tree(&[("100644", b, b"old")]);
    let ours = f.tree(&[("100644", o, b"new")]);
    let theirs = f.tree(&[("100644", t, b"old")]);
    let result = f.objects().merge_trees(base, ours, theirs).unwrap();
    let expected = oracle(&f, base, ours, theirs);
    assert_eq!(expected.status.code(), Some(0));
    assert!(result.is_clean());
    assert_eq!(result.tree.id(), oracle_tree(&expected));
}

#[test]
fn rename_rename_keeps_distinct_stage_paths() {
    let f = Fixture::new("sha1");
    let b = f.blob(b"payload\n");
    let base = f.tree(&[("100644", b, b"old")]);
    let ours = f.tree(&[("100644", b, b"left")]);
    let theirs = f.tree(&[("100644", b, b"right")]);
    let result = f.objects().merge_trees(base, ours, theirs).unwrap();
    let expected = oracle(&f, base, ours, theirs);
    assert_eq!(expected.status.code(), Some(1));
    assert_eq!(result.conflicts[0].kind, ConflictKind::RenameRename);
    let stages = result.conflicts[0].index_entries().unwrap();
    let mut rows: Vec<_> = stages
        .iter()
        .map(|e| {
            format!(
                "100644 {} {}\t{}",
                e.id,
                e.stage as u8,
                std::str::from_utf8(&e.path).unwrap()
            )
            .into_bytes()
        })
        .collect();
    rows.sort();
    let mut expected_rows: Vec<_> = expected
        .stdout
        .split(|b| *b == b'\n')
        .skip(1)
        .take_while(|line| !line.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    expected_rows.sort();
    assert_eq!(rows, expected_rows);
}

#[test]
fn rename_delete_retains_base_and_renamed_side() {
    let f = Fixture::new("sha1");
    let b = f.blob(b"payload\n");
    let base = f.tree(&[("100644", b, b"old")]);
    let ours = f.tree(&[("100644", b, b"new")]);
    let theirs = f.tree(&[]);
    let result = f.objects().merge_trees(base, ours, theirs).unwrap();
    let expected = oracle(&f, base, ours, theirs);
    assert_eq!(expected.status.code(), Some(1));
    assert_eq!(result.conflicts[0].kind, ConflictKind::RenameDelete);
    assert_eq!(result.conflicts[0].index_entries().unwrap().len(), 2);
    assert_eq!(result.tree.id(), oracle_tree(&expected));
}

#[test]
fn directory_rename_relocates_and_reports_opposite_addition() {
    let f = Fixture::new("sha1");
    let b = f.blob(b"payload\n");
    let added = f.blob(b"added\n");
    let old = f.tree(&[("100644", b, b"file")]);
    let changed = f.tree(&[("100644", b, b"file"), ("100644", added, b"add")]);
    let base = f.tree(&[("040000", old, b"old")]);
    let ours = f.tree(&[("040000", old, b"new")]);
    let theirs = f.tree(&[("040000", changed, b"old")]);
    let result = f.objects().merge_trees(base, ours, theirs).unwrap();
    let expected = oracle(&f, base, ours, theirs);
    assert_eq!(expected.status.code(), Some(1));
    assert_eq!(result.conflicts.len(), 1);
    assert_eq!(result.conflicts[0].kind, ConflictKind::DirectoryRename);
    assert_eq!(result.conflicts[0].path, b"new/add");
    assert_eq!(result.tree.id(), oracle_tree(&expected));
}

#[test]
fn destination_collision_is_an_explicit_refusal() {
    let f = Fixture::new("sha1");
    let b = f.blob(b"base\n");
    let t = f.blob(b"theirs\n");
    let base = f.tree(&[("100644", b, b"old")]);
    let ours = f.tree(&[("100644", b, b"new")]);
    let theirs = f.tree(&[("100644", t, b"old"), ("100644", t, b"new")]);
    assert!(
        matches!(f.objects().merge_trees(base, ours, theirs), Err(RenameError::DestinationCollision(path)) if path == b"new")
    );
}

#[test]
fn empty_directory_refusal_preserves_the_input_store() {
    let f = Fixture::new("sha1");
    let empty = f.tree(&[]);
    let root = f.tree(&[("040000", empty, b"empty")]);
    assert!(
        matches!(f.objects().merge_trees(root, root, root), Err(RenameError::EmptyDirectory(path)) if path == b"empty")
    );
    assert_eq!(
        f.objects().read_tree(empty).unwrap(),
        Tree::new(girt::ObjectFormat::Sha1, vec![]).unwrap()
    );
}
