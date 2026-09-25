//! Independent Git CLI comparisons and public inference contracts in both formats.
#[path = "support/rewrite_git.rs"]
mod fixture;

use std::sync::atomic::AtomicBool;

use fixture::{Fixture, git};
use girt::rewrites::{Copies, Error, Kind, Limits, Options};
use rstest::rstest;

#[rstest]
#[case::exact(b"a\nb\nc\nd\n", b"a\nb\nc\nd\n", Some(100))]
#[case::half(b"a\nb\nc\nd\n", b"a\nb\nx\ny\n", Some(50))]
#[case::below(b"a\nb\nc\nd\n", b"a\nx\ny\nz\n", None)]
#[case::reordered(b"a\nb\nc\nd\n", b"d\nc\nb\na\n", Some(100))]
#[case::binary(b"\0\na\nb\nc\n", b"\0\na\nb\nx\n", Some(75))]
#[case::binary_exact(b"\0binary", b"\0binary", Some(100))]
#[case::crlf(b"a\r\nb\r\n", b"a\nb\n", Some(66))]
#[case::fragment(b"a\nb", b"a\nc", Some(66))]
#[case::empty(b"", b"", None)]
fn git_similarity(
    #[values("sha1", "sha256")] format: &str,
    #[case] before: &[u8],
    #[case] after: &[u8],
    #[case] expected: Option<u8>,
) {
    let f = Fixture::new(format);
    let old = f.tree(&[("100644", f.blob(before), b"old")]);
    let new = f.tree(&[("100755", f.blob(after), b"new")]);
    let records = f
        .objects()
        .detect_rewrites(
            Some(old),
            Some(new),
            Options::default(),
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(records.first().map(|r| r.similarity), expected);
    let observed = git(
        f.root.path(),
        &[
            "diff-tree",
            "-r",
            "--no-commit-id",
            "--name-status",
            "-z",
            "-M50%",
            "--no-rename-empty",
            &old.to_string(),
            &new.to_string(),
        ],
        b"",
    );
    let expected_git = expected.map_or_else(
        || b"A\0new\0D\0old\0".to_vec(),
        |score| format!("R{score:03}\0old\0new\0").into_bytes(),
    );
    assert_eq!(observed, expected_git);
}

#[rstest]
#[case::copy(Copies::Modified, true)]
#[case::rename_only(Copies::Disabled, false)]
fn modified_source(
    #[values("sha1", "sha256")] format: &str,
    #[case] copies: Copies,
    #[case] detected: bool,
) {
    let f = Fixture::new(format);
    let source = f.blob(b"a\nb\nc\nd\n");
    let old = f.tree(&[("100644", source, b"source")]);
    let new = f.tree(&[
        ("100644", f.blob(b"changed\n"), b"source"),
        ("100644", f.blob(b"a\nb\nc\nx\n"), b"target"),
    ]);
    let records = f
        .objects()
        .detect_rewrites(
            Some(old),
            Some(new),
            Options {
                copies,
                ..Options::default()
            },
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(records.len(), usize::from(detected));
    assert_eq!(
        records.first().map(|r| (r.source_id, r.kind, r.similarity)),
        detected.then_some((source, Kind::Copy, 75))
    );
    assert_eq!(
        git(
            f.root.path(),
            &[
                "diff-tree",
                "-r",
                "--no-commit-id",
                "--name-status",
                "-C50%",
                &old.to_string(),
                &new.to_string()
            ],
            b""
        ),
        b"M\tsource\nC075\tsource\ttarget\n"
    );
}

#[rstest]
fn ties_filter_after_inference(#[values("sha1", "sha256")] format: &str) {
    let f = Fixture::new(format);
    let blob = f.blob(b"same\n");
    let old = f.tree(&[("100644", blob, b"a"), ("100644", blob, b"b")]);
    let new = f.tree(&[
        ("100644", blob, b"c"),
        ("100644", blob, b"d"),
        ("100644", blob, b"e"),
    ]);
    let objects = f.objects();
    let options = Options {
        copies: Copies::Modified,
        ..Options::default()
    };
    let all = objects
        .detect_rewrites(
            Some(old),
            Some(new),
            options,
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    let filtered = objects
        .detect_rewrites(
            Some(old),
            Some(new),
            options,
            Limits::default(),
            Some(&[b"e"]),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        all.iter().map(|r| (&r.source, r.kind)).collect::<Vec<_>>(),
        vec![
            (&b"a".to_vec(), Kind::Rename),
            (&b"b".to_vec(), Kind::Rename),
            (&b"a".to_vec(), Kind::Copy)
        ]
    );
    assert_eq!(filtered, all[2..]);
}

#[rstest]
#[case::symlink("120000", "120000", false)]
#[case::type_change("120000", "100644", false)]
#[case::executable("100644", "100755", true)]
fn modes(
    #[values("sha1", "sha256")] format: &str,
    #[case] old_mode: &str,
    #[case] new_mode: &str,
    #[case] detected: bool,
) {
    let f = Fixture::new(format);
    let blob = f.blob(b"same");
    let old = f.tree(&[(old_mode, blob, b"old")]);
    let new = f.tree(&[(new_mode, blob, b"new")]);
    let records = f
        .objects()
        .detect_rewrites(
            Some(old),
            Some(new),
            Options::default(),
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(records.len(), usize::from(detected));
}

#[rstest]
fn file_directory_transition(#[values("sha1", "sha256")] format: &str) {
    let f = Fixture::new(format);
    let blob = f.blob(b"same");
    let old = f.tree(&[("100644", blob, b"path")]);
    let subtree = f.tree(&[("100644", blob, b"child")]);
    let new = f.tree(&[("040000", subtree, b"path")]);
    let records = f
        .objects()
        .detect_rewrites(
            Some(old),
            Some(new),
            Options::default(),
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        (&records[0].source, &records[0].target),
        (&b"path".to_vec(), &b"path/child".to_vec())
    );
}

#[rstest]
#[case::exact(b"a\nb\nc\nd\n", true)]
#[case::edited(b"a\nb\nx\ny\n", false)]
fn exact_precedes_candidate_limit(
    #[values("sha1", "sha256")] format: &str,
    #[case] target: &[u8],
    #[case] exact: bool,
) {
    let f = Fixture::new(format);
    let old = f.tree(&[("100644", f.blob(b"a\nb\nc\nd\n"), b"old")]);
    let new = f.tree(&[("100644", f.blob(target), b"new")]);
    let result = f.objects().detect_rewrites(
        Some(old),
        Some(new),
        Options {
            candidate_limit: 0,
            ..Options::default()
        },
        Limits::default(),
        None,
        &AtomicBool::new(false),
    );
    assert_eq!(result.is_ok(), exact);
    assert_eq!(
        matches!(
            result,
            Err(Error::Candidates {
                sources: 1,
                targets: 1,
                limit: 0
            })
        ),
        !exact
    );
}

#[rstest]
fn cancelled_before_storage(#[values("sha1", "sha256")] format: &str) {
    let f = Fixture::new(format);
    let missing = f.blob(b"not a tree");
    assert!(matches!(
        f.objects().detect_rewrites(
            Some(missing),
            None,
            Options::default(),
            Limits::default(),
            None,
            &AtomicBool::new(true)
        ),
        Err(Error::Cancelled)
    ));
}

#[rstest]
#[case::pairs(Limits { max_pairs: 0, ..Limits::default() }, "pairs")]
#[case::work(Limits { max_work: 0, ..Limits::default() }, "work")]
#[case::spans(Limits { max_spans: 0, ..Limits::default() }, "spans")]
fn budgets(#[case] limits: Limits, #[case] expected: &str) {
    let f = Fixture::new("sha1");
    let old = f.tree(&[("100644", f.blob(b"a\nb\nc\n"), b"old")]);
    let new = f.tree(&[("100644", f.blob(b"a\nb\nx\n"), b"new")]);
    assert!(
        matches!(f.objects().detect_rewrites(Some(old), Some(new), Options::default(), limits, None, &AtomicBool::new(false)), Err(Error::Limit(name)) if name == expected)
    );
}

#[rstest]
#[case::exclude(false, 0)]
#[case::include(true, 1)]
fn empty_policy(#[case] track_empty: bool, #[case] count: usize) {
    let f = Fixture::new("sha1");
    let blob = f.blob(b"");
    let old = f.tree(&[("100644", blob, b"old")]);
    let new = f.tree(&[("100644", blob, b"new")]);
    let result = f
        .objects()
        .detect_rewrites(
            Some(old),
            Some(new),
            Options {
                track_empty,
                ..Options::default()
            },
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(result.len(), count);
}

#[rstest]
fn exact_only_rejects_reordered_spans(#[values("sha1", "sha256")] format: &str) {
    let f = Fixture::new(format);
    let old = f.tree(&[("100644", f.blob(b"a\nb\nc\n"), b"old")]);
    let new = f.tree(&[("100644", f.blob(b"c\nb\na\n"), b"new")]);
    assert!(
        f.objects()
            .detect_rewrites(
                Some(old),
                Some(new),
                Options {
                    similarity: 100,
                    ..Options::default()
                },
                Limits::default(),
                None,
                &AtomicBool::new(false)
            )
            .unwrap()
            .is_empty()
    );
}

#[rstest]
#[case::unchanged("100644", 0)]
#[case::mode_only("100755", 1)]
fn modified_source_includes_mode_only(#[case] mode: &str, #[case] count: usize) {
    let f = Fixture::new("sha1");
    let blob = f.blob(b"same\n");
    let old = f.tree(&[("100644", blob, b"source")]);
    let new = f.tree(&[(mode, blob, b"source"), ("100644", blob, b"target")]);
    let result = f
        .objects()
        .detect_rewrites(
            Some(old),
            Some(new),
            Options {
                copies: Copies::Modified,
                ..Options::default()
            },
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(result.len(), count);
}

#[rstest]
fn wrong_kind_is_structured(#[values("sha1", "sha256")] format: &str) {
    let f = Fixture::new(format);
    let wrong = f.tree(&[]);
    let repo = girt::Repository::open(f.root.path()).unwrap();
    let write = |name: &[u8]| {
        let tree = girt::Tree::new(
            repo.object_format(),
            vec![girt::TreeEntry {
                name: name.to_vec(),
                mode: girt::EntryMode::Blob,
                id: wrong,
            }],
        )
        .unwrap();
        repo.loose_objects().write_tree(&tree).unwrap()
    };
    let old = write(b"old");
    let new = write(b"new");
    assert!(
        matches!(f.objects().detect_rewrites(Some(old), Some(new), Options::default(), Limits::default(), None, &AtomicBool::new(false)), Err(Error::NotBlob { id, actual: girt::ObjectKind::Tree }) if id == wrong)
    );
}

#[rstest]
fn missing_exact_blob_is_not_trusted(#[values("sha1", "sha256")] format: &str) {
    let f = Fixture::new(format);
    let missing = girt::ObjectId::for_blob(f.objects().object_format(), b"not stored");
    let old = f.tree(&[("100644", missing, b"old")]);
    let new = f.tree(&[("100644", missing, b"new")]);
    assert!(
        matches!(f.objects().detect_rewrites(Some(old), Some(new), Options::default(), Limits::default(), None, &AtomicBool::new(false)), Err(Error::Missing(id)) if id == missing)
    );
}

#[rstest]
fn blob_budget_applies_before_exact_matches(#[values("sha1", "sha256")] format: &str) {
    let f = Fixture::new(format);
    let blob = f.blob(b"same\n");
    let old = f.tree(&[("100644", blob, b"old")]);
    let new = f.tree(&[("100644", blob, b"new")]);
    assert!(
        matches!(f.objects().detect_rewrites(Some(old), Some(new), Options::default(), Limits { max_blob_bytes: 0, ..Limits::default() }, None, &AtomicBool::new(false)), Err(Error::Read { id, .. }) if id == blob)
    );
}

#[rstest]
#[case::zero(0)]
#[case::too_large(101)]
fn invalid_similarity(#[case] similarity: u8) {
    let f = Fixture::new("sha1");
    assert!(
        matches!(f.objects().detect_rewrites(None, None, Options { similarity, ..Options::default() }, Limits::default(), None, &AtomicBool::new(false)), Err(Error::Similarity(value)) if value == similarity)
    );
}
