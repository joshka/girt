//! Original trees and histories generated through Git's public CLI under isolated config.
#[path = "support/rewrite_git.rs"]
mod fixture;
#[path = "support/merge_git.rs"]
mod oracle;

use std::sync::atomic::AtomicBool;

use fixture::{Fixture, git, id};
use girt::index::Stage;
use girt::merge::{ConflictKind, TreeError, TreeOptions};
use girt::{ObjectId, Repository};
use rstest::rstest;

fn commit(fixture: &Fixture, tree: ObjectId, parent: Option<ObjectId>) -> ObjectId {
    let tree = tree.to_string();
    let parent = parent.map(|p| p.to_string());
    let mut args = vec![
        "-c",
        "user.name=Merge Author",
        "-c",
        "user.email=merge@example.com",
        "commit-tree",
        &tree,
    ];
    if let Some(parent) = &parent {
        args.extend(["-p", parent]);
    }
    id(&git(fixture.root.path(), &args, b"fixture\n"))
}

fn oracle_clean(fixture: &Fixture, base: ObjectId, ours: ObjectId, theirs: ObjectId) -> ObjectId {
    let base = commit(fixture, base, None);
    let ours = commit(fixture, ours, Some(base));
    let theirs = commit(fixture, theirs, Some(base));
    let bytes = git(
        fixture.root.path(),
        &[
            "merge-tree",
            "--write-tree",
            &ours.to_string(),
            &theirs.to_string(),
        ],
        b"",
    );
    id(bytes.split(|b| *b == b'\n').next().unwrap())
}

#[rstest]
fn independent_content_and_executable_mode_match_git(#[values("sha1", "sha256")] format: &str) {
    let f = Fixture::new(format);
    let base_blob = f.blob(b"one\ntwo\nthree\n");
    let ours_blob = f.blob(b"ONE\ntwo\nthree\n");
    let theirs_blob = f.blob(b"one\ntwo\nTHREE\n");
    let base = f.tree(&[("100644", base_blob, b"file")]);
    let ours = f.tree(&[("100755", ours_blob, b"file")]);
    let theirs = f.tree(&[("100644", theirs_blob, b"file")]);
    let expected = oracle_clean(&f, base, ours, theirs);
    let result = f.objects().merge_tree_entries(base, ours, theirs).unwrap();
    assert!(result.is_clean());
    assert_eq!(result.tree.id(), expected);
    assert_eq!(result.blobs[0].1, b"ONE\ntwo\nTHREE\n");
}

#[test]
fn nested_additions_and_deletions_match_git() {
    let f = Fixture::new("sha1");
    let payload = f.blob(b"payload\n");
    let old_child = f.tree(&[("100644", payload, b"removed")]);
    let our_child = f.tree(&[("100644", payload, b"ours")]);
    let their_child = f.tree(&[
        ("100644", payload, b"removed"),
        ("100644", payload, b"theirs"),
    ]);
    let base = f.tree(&[("040000", old_child, b"dir")]);
    let ours = f.tree(&[("040000", our_child, b"dir")]);
    let theirs = f.tree(&[("040000", their_child, b"dir")]);
    let expected = oracle_clean(&f, base, ours, theirs);
    let result = f.objects().merge_tree_entries(base, ours, theirs).unwrap();
    assert!(result.is_clean());
    assert_eq!(result.tree.id(), expected);
    assert_eq!(result.trees.len(), 2);
}

#[rstest]
#[case::content(true, true, ConflictKind::Content, vec![Stage::Base, Stage::Ours, Stage::Theirs])]
#[case::add_add(false, true, ConflictKind::AddAdd, vec![Stage::Ours, Stage::Theirs])]
#[case::modify_delete(true, false, ConflictKind::ModifyDelete, vec![Stage::Base, Stage::Theirs])]
fn conflicts_preserve_original_stages(
    #[case] has_base: bool,
    #[case] has_ours: bool,
    #[case] expected: ConflictKind,
    #[case] stages: Vec<Stage>,
) {
    let f = Fixture::new("sha1");
    let b = f.blob(b"base\n");
    let o = f.blob(b"ours\n");
    let t = f.blob(b"theirs\n");
    let base_entries: Vec<_> = has_base
        .then_some(("100644", b, b"file".as_slice()))
        .into_iter()
        .collect();
    let our_entries: Vec<_> = has_ours
        .then_some(("100644", o, b"file".as_slice()))
        .into_iter()
        .collect();
    let base = f.tree(&base_entries);
    let ours = f.tree(&our_entries);
    let theirs = f.tree(&[("100644", t, b"file")]);
    let result = f.objects().merge_tree_entries(base, ours, theirs).unwrap();
    assert_eq!(result.conflicts.len(), 1);
    assert_eq!(result.conflicts[0].kind, expected);
    let entries = result.conflicts[0].index_entries().unwrap();
    assert_eq!(entries.iter().map(|e| e.stage).collect::<Vec<_>>(), stages);
    assert_eq!(result.conflicts[0].theirs.unwrap().id, t);
    assert!(!result.is_clean());
    let base_commit = commit(&f, base, None);
    let ours_commit = commit(&f, ours, Some(base_commit));
    let theirs_commit = commit(&f, theirs, Some(base_commit));
    let oracle = oracle::merge_tree(
        f.root.path(),
        &ours_commit.to_string(),
        &theirs_commit.to_string(),
    );
    assert_eq!(oracle.status.code(), Some(1));
    let rows: Vec<_> = oracle
        .stdout
        .split(|b| *b == b'\n')
        .skip(1)
        .take_while(|line| !line.is_empty())
        .collect();
    let expected: Vec<_> = entries
        .iter()
        .map(|e| {
            format!(
                "{} {} {}\tfile",
                match e.mode {
                    girt::index::Mode::Regular => "100644",
                    _ => panic!("unexpected mode"),
                },
                e.id,
                e.stage as u8
            )
            .into_bytes()
        })
        .collect();
    assert_eq!(rows, expected);
}

#[rstest]
#[case::symlinks("120000", "120000", ConflictKind::Symlink)]
#[case::type_change("120000", "100644", ConflictKind::Type)]
fn nonregular_conflicts_remain_unresolved(
    #[case] our_mode: &str,
    #[case] their_mode: &str,
    #[case] expected: ConflictKind,
) {
    let f = Fixture::new("sha1");
    let b = f.blob(b"base\n");
    let o = f.blob(b"ours\n");
    let t = f.blob(b"theirs\n");
    let base = f.tree(&[("100644", b, b"file")]);
    let ours = f.tree(&[(our_mode, o, b"file")]);
    let theirs = f.tree(&[(their_mode, t, b"file")]);
    let result = f.objects().merge_tree_entries(base, ours, theirs).unwrap();
    assert_eq!(result.conflicts[0].kind, expected);
    assert_eq!(result.tree.entries()[0].id, o);
}

#[test]
fn directory_conflict_refuses_leaf_index_stages() {
    let f = Fixture::new("sha1");
    let b = f.blob(b"base\n");
    let t = f.blob(b"theirs\n");
    let child = f.tree(&[("100644", b, b"child")]);
    let base = f.tree(&[("100644", b, b"file")]);
    let ours = f.tree(&[("040000", child, b"file")]);
    let theirs = f.tree(&[("100644", t, b"file")]);
    let result = f.objects().merge_tree_entries(base, ours, theirs).unwrap();
    assert_eq!(result.conflicts[0].kind, ConflictKind::Type);
    assert!(matches!(
        result.conflicts[0].index_entries(),
        Err(TreeError::DirectoryStages)
    ));
}

#[test]
fn generated_objects_are_not_written_until_caller_installs_them() {
    let f = Fixture::new("sha1");
    let b = f.blob(b"a\nb\nc\n");
    let o = f.blob(b"A\nb\nc\n");
    let t = f.blob(b"a\nb\nC\n");
    let base = f.tree(&[("100644", b, b"file")]);
    let ours = f.tree(&[("100644", o, b"file")]);
    let theirs = f.tree(&[("100644", t, b"file")]);
    let objects = f.objects();
    let result = objects.merge_tree_entries(base, ours, theirs).unwrap();
    assert!(!objects.contains(result.tree.id()).unwrap());
    assert!(!objects.contains(result.blobs[0].0).unwrap());
    let loose = Repository::open(f.root.path()).unwrap().loose_objects();
    assert_eq!(
        loose.write_blob(&result.blobs[0].1).unwrap(),
        result.blobs[0].0
    );
    assert_eq!(loose.write_tree(&result.tree).unwrap(), result.tree.id());
    assert_eq!(
        f.objects().read_tree(result.tree.id()).unwrap(),
        result.tree
    );
}

#[test]
fn cancellation_precedes_input_reads() {
    let f = Fixture::new("sha1");
    let empty = f.tree(&[]);
    assert!(matches!(
        f.objects().merge_tree_entries_with(
            empty,
            empty,
            empty,
            TreeOptions::default(),
            &AtomicBool::new(true)
        ),
        Err(TreeError::Cancelled)
    ));
}

#[test]
fn traversal_entry_limit_is_enforced() {
    let f = Fixture::new("sha1");
    let b = f.blob(b"a\n");
    let tree = f.tree(&[("100644", b, b"file")]);
    let mut options = TreeOptions::default();
    options.max_entries = 0;
    assert!(matches!(
        f.objects()
            .merge_tree_entries_with(tree, tree, tree, options, &AtomicBool::new(false)),
        Err(TreeError::Limit("entries"))
    ));
}

#[test]
fn gitlink_conflict_does_not_read_submodule_commits() {
    let f = Fixture::new("sha1");
    let a: ObjectId = "1111111111111111111111111111111111111111".parse().unwrap();
    let b: ObjectId = "2222222222222222222222222222222222222222".parse().unwrap();
    let c: ObjectId = "3333333333333333333333333333333333333333".parse().unwrap();
    let tree = |value| {
        id(&git(
            f.root.path(),
            &["mktree", "--missing"],
            format!("160000 commit {value}\tsub\n").as_bytes(),
        ))
    };
    let result = f
        .objects()
        .merge_tree_entries(tree(a), tree(b), tree(c))
        .unwrap();
    assert_eq!(result.conflicts[0].kind, ConflictKind::Gitlink);
    assert_eq!(result.conflicts[0].index_entries().unwrap()[2].id, c);
}
