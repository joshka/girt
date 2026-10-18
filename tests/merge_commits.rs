//! Original commit graphs; Git CLI observations only, with isolated configuration.
#[path = "support/rewrite_git.rs"]
mod fixture;
#[path = "support/merge_git.rs"]
mod oracle;

use std::sync::atomic::AtomicBool;

use fixture::{Fixture, id};
use girt::ObjectId;
use girt::merge::{CommitError, CommitOptions};
use rstest::rstest;

fn file(f: &Fixture, name: &[u8], bytes: &[u8]) -> ObjectId {
    f.tree(&[("100644", f.blob(bytes), name)])
}

fn expected(f: &Fixture, ours: ObjectId, theirs: ObjectId) -> ObjectId {
    let output = oracle::merge_tree(f.root.path(), &ours.to_string(), &theirs.to_string());
    assert_eq!(output.status.code(), Some(0), "{:?}", output);
    id(output.stdout.split(|b| *b == b'\n').next().unwrap())
}

#[rstest]
fn ordinary_commit_merge_matches_git(#[values("sha1", "sha256")] format: &str) {
    let f = Fixture::new(format);
    let base = oracle::commit(f.root.path(), file(&f, b"file", b"a\nb\nc\n"), None);
    let ours = oracle::commit(f.root.path(), file(&f, b"file", b"A\nb\nc\n"), Some(base));
    let theirs = oracle::commit(f.root.path(), file(&f, b"file", b"a\nb\nC\n"), Some(base));
    let result = f.objects().merge_commits(ours, theirs).unwrap();
    assert_eq!(result.parents, [ours, theirs]);
    assert_eq!(result.bases, [base]);
    assert!(result.result.is_clean());
    assert_eq!(result.result.tree.id(), expected(&f, ours, theirs));
}

#[test]
fn criss_cross_virtual_content_base_is_available_to_rename_scoring() {
    let f = Fixture::new("sha1");
    let root_tree = file(
        &f,
        b"old",
        b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
    );
    let a_tree = file(
        &f,
        b"old",
        b"ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
    );
    let b_tree = file(
        &f,
        b"old",
        b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nTEN\n",
    );
    let a_renamed = file(
        &f,
        b"new",
        b"ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
    );
    let root = oracle::commit(f.root.path(), root_tree, None);
    let a = oracle::commit(f.root.path(), a_tree, Some(root));
    let b = oracle::commit(f.root.path(), b_tree, Some(root));
    let ours = oracle::commit_parents(f.root.path(), a_renamed, &[a, b]);
    let theirs = oracle::commit_parents(f.root.path(), b_tree, &[b, a]);
    let objects = f.objects();
    let result = objects.merge_commits(ours, theirs).unwrap();
    let virtual_blob = ObjectId::for_blob(
        girt::ObjectFormat::Sha1,
        b"ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nTEN\n",
    );
    assert_eq!(result.bases.len(), 2);
    assert!(!objects.contains(virtual_blob).unwrap());
    assert!(
        result
            .result
            .blobs
            .iter()
            .any(|(id, _)| *id == virtual_blob)
    );
    assert!(result.result.is_clean());
    assert_eq!(result.result.tree.id(), expected(&f, ours, theirs));
}

#[test]
fn unresolved_virtual_base_is_an_explicit_error() {
    let f = Fixture::new("sha1");
    let root = oracle::commit(f.root.path(), file(&f, b"file", b"base\n"), None);
    let a_tree = file(&f, b"file", b"a\n");
    let b_tree = file(&f, b"file", b"b\n");
    let a = oracle::commit(f.root.path(), a_tree, Some(root));
    let b = oracle::commit(f.root.path(), b_tree, Some(root));
    let ours = oracle::commit_parents(f.root.path(), a_tree, &[a, b]);
    let theirs = oracle::commit_parents(f.root.path(), b_tree, &[b, a]);
    assert!(
        matches!(f.objects().merge_commits(ours, theirs), Err(CommitError::ConflictedBase(result)) if !result.is_clean() && result.conflicts.len() == 1)
    );
}

#[test]
fn unrelated_history_requires_explicit_policy() {
    let f = Fixture::new("sha1");
    let a = oracle::commit(f.root.path(), file(&f, b"a", b"a\n"), None);
    let b = oracle::commit(f.root.path(), file(&f, b"b", b"b\n"), None);
    assert!(matches!(
        f.objects().merge_commits(a, b),
        Err(CommitError::Unrelated)
    ));
    let mut options = CommitOptions::default();
    options.allow_unrelated_histories = true;
    let result = f
        .objects()
        .merge_commits_with(a, b, options, &AtomicBool::new(false))
        .unwrap();
    assert!(result.result.is_clean());
    assert_eq!(result.result.tree.entries().len(), 2);
}

#[test]
fn cancellation_precedes_history_reads() {
    let f = Fixture::new("sha1");
    let missing: ObjectId = "1111111111111111111111111111111111111111".parse().unwrap();
    assert!(matches!(
        f.objects().merge_commits_with(
            missing,
            missing,
            CommitOptions::default(),
            &AtomicBool::new(true)
        ),
        Err(CommitError::Cancelled)
    ));
}

#[test]
fn ancestry_query_budget_precedes_object_access() {
    let f = Fixture::new("sha1");
    let missing: ObjectId = "1111111111111111111111111111111111111111".parse().unwrap();
    let mut options = CommitOptions::default();
    options.max_ancestry_queries = 0;
    assert!(matches!(
        f.objects()
            .merge_commits_with(missing, missing, options, &AtomicBool::new(false)),
        Err(CommitError::Limit("ancestry queries"))
    ));
}
