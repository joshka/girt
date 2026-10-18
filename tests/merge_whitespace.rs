//! Original byte fixtures checked through isolated Git's ort whitespace strategies.
#[path = "support/rewrite_git.rs"]
mod fixture;
#[path = "support/merge_git.rs"]
mod oracle;

use std::sync::atomic::AtomicBool;

use fixture::{Fixture, id};
use girt::content_diff::{DiffError, DiffLimits};
use girt::merge::{ContentOptions, TreeMergeOptions, Whitespace, content_with};
use rstest::rstest;

fn oracle_output(
    root: &std::path::Path,
    ours: &str,
    theirs: &str,
    flag: &str,
) -> std::process::Output {
    if flag.is_empty() {
        oracle::merge_tree(root, ours, theirs)
    } else {
        oracle::merge_tree_options(root, ours, theirs, &[flag])
    }
}

#[rstest]
#[case::insert_context(
    b"a\nb\nc\n",
    b"new\na \nb\nc\n",
    b"a\nb\nC\n",
    Whitespace::IgnoreSpaceAtEol,
    "-Xignore-space-at-eol"
)]
#[case::delete_context(
    b"a\nb\nc\nd\ne\n",
    b"a \nb\nd\ne\n",
    b"a\nb\nc\nd\nE\n",
    Whitespace::IgnoreSpaceAtEol,
    "-Xignore-space-at-eol"
)]
#[case::same_substantive(
    b"alpha beta\n",
    b"ALPHA  beta\n",
    b"ALPHA\tbeta\n",
    Whitespace::IgnoreSpaceChange,
    "-Xignore-space-change"
)]
#[case::empty_body_eof(b" ", b"\t", b"new", Whitespace::IgnoreAllSpace, "-Xignore-all-space")]
#[case::exact(b"a b\n", b"a  b\n", b"a B\n", Whitespace::Exact, "")]
#[case::our_space(
    b"alpha beta\nkeep\nend\n",
    b"alpha  beta\nkeep\nend\n",
    b"alpha BETA\nkeep\nend\n",
    Whitespace::IgnoreSpaceChange,
    "-Xignore-space-change"
)]
#[case::their_space(
    b"alpha beta\n",
    b"alpha BETA\n",
    b"alpha\tbeta\n",
    Whitespace::IgnoreSpaceChange,
    "-Xignore-space-change"
)]
#[case::context(
    b"alpha beta\nkeep\nend\n",
    b"alpha  beta\nkeep\nend\n",
    b"alpha beta\nkeep\nEND\n",
    Whitespace::IgnoreSpaceChange,
    "-Xignore-space-change"
)]
#[case::both_space(
    b"alpha beta\n",
    b"alpha  beta\n",
    b"alpha\tbeta\n",
    Whitespace::IgnoreSpaceChange,
    "-Xignore-space-change"
)]
#[case::zero_space_amount(
    b"alpha beta\n",
    b"alphabeta\n",
    b"alpha BETA\n",
    Whitespace::IgnoreSpaceChange,
    "-Xignore-space-change"
)]
#[case::zero_space_all(
    b"alpha beta\n",
    b"alphabeta\n",
    b"alpha BETA\n",
    Whitespace::IgnoreAllSpace,
    "-Xignore-all-space"
)]
#[case::trailing(
    b"a\nb\nc\n",
    b"a \t\nb\nc\n",
    b"A\nb\nc\n",
    Whitespace::IgnoreSpaceAtEol,
    "-Xignore-space-at-eol"
)]
#[case::inline_is_significant(
    b"a b\n",
    b"a  b\n",
    b"a B\n",
    Whitespace::IgnoreSpaceAtEol,
    "-Xignore-space-at-eol"
)]
#[case::crlf(
    b"a\nb\nc\n",
    b"a\r\nb\r\nc\r\n",
    b"a\nb\nC\n",
    Whitespace::IgnoreCrAtEol,
    "-Xignore-cr-at-eol"
)]
#[case::eof(
    b"a\nb\nc\n",
    b"a \nb\nc\n",
    b"a\nb\nC",
    Whitespace::IgnoreSpaceAtEol,
    "-Xignore-space-at-eol"
)]
#[case::bridge(
    b"a\nb\nc\n",
    b"A\nb \nc\n",
    b"a\nb\nC\n",
    Whitespace::IgnoreSpaceAtEol,
    "-Xignore-space-at-eol"
)]
#[case::non_utf8(
    b"a\xff b\n",
    b"a\xff  b\n",
    b"a\xff B\n",
    Whitespace::IgnoreSpaceChange,
    "-Xignore-space-change"
)]
fn matches_git(
    #[case] base: &[u8],
    #[case] ours: &[u8],
    #[case] theirs: &[u8],
    #[case] whitespace: Whitespace,
    #[case] flag: &str,
) {
    let f = Fixture::new("sha1");
    let tree = |bytes| f.tree(&[("100644", f.blob(bytes), b"file")]);
    let base_tree = tree(base);
    let ours_tree = tree(ours);
    let theirs_tree = tree(theirs);
    let base_commit = oracle::commit(f.root.path(), base_tree, None);
    let ours_commit = oracle::commit(f.root.path(), ours_tree, Some(base_commit));
    let theirs_commit = oracle::commit(f.root.path(), theirs_tree, Some(base_commit));
    let expected = oracle_output(
        f.root.path(),
        &ours_commit.to_string(),
        &theirs_commit.to_string(),
        flag,
    );
    assert!(
        matches!(expected.status.code(), Some(0 | 1)),
        "{:?}",
        expected
    );
    let mut options = TreeMergeOptions::default();
    options.trees.content = ContentOptions::default().with_whitespace(whitespace);
    let result = f
        .objects()
        .merge_trees_with(
            base_tree,
            ours_tree,
            theirs_tree,
            options,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(result.is_clean(), expected.status.success());
    let expected_id = id(expected.stdout.split(|b| *b == b'\n').next().unwrap());
    let expected_bytes = fixture::git(
        f.root.path(),
        &["show", &format!("{expected_id}:file")],
        b"",
    );
    assert_eq!(
        result.is_clean().then_some(result.tree.id()),
        expected.status.success().then_some(expected_id),
        "expected bytes {:?}; actual generated {:?}",
        String::from_utf8_lossy(&expected_bytes),
        result.blobs
    );
}

#[test]
fn normalization_work_is_bounded() {
    let options = ContentOptions::default()
        .with_whitespace(Whitespace::IgnoreAllSpace)
        .with_limits(DiffLimits::default().with_max_work(0));
    assert!(matches!(
        content_with(b"a", b" a", b"A", options, &AtomicBool::new(false)),
        Err(girt::merge::Error::Diff(DiffError::Limit(
            "whitespace work"
        )))
    ));
}

#[test]
fn cancellation_precedes_normalization() {
    let options = ContentOptions::default().with_whitespace(Whitespace::IgnoreAllSpace);
    assert!(matches!(
        content_with(b"a", b" a", b"A", options, &AtomicBool::new(true)),
        Err(girt::merge::Error::Diff(DiffError::Cancelled))
    ));
}
