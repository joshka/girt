//! Independent Git CLI comparisons and public inference contracts in both formats.
#[path = "support/rewrite_git.rs"]
mod fixture;

use std::sync::atomic::AtomicBool;

use fixture::{Fixture, git, git_output};
use girt::ObjectId;
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
            Options::default().with_copies(copies),
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
fn modified_postimage_reports_preimage_id(#[values("sha1", "sha256")] format: &str) {
    let f = Fixture::new(format);
    let before = f.blob(b"foo\n");
    let after = f.blob(b"bar\n");
    let old = f.tree(&[("100644", before, b"source")]);
    let new = f.tree(&[("100644", after, b"source"), ("100644", after, b"target")]);
    let records = f
        .objects()
        .detect_rewrites(
            Some(old),
            Some(new),
            Options::default().with_copies(Copies::ModifiedPostimage),
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].source_id, before);
    assert_eq!(records[0].target_id, after);
    assert_eq!(records[0].similarity, 100);
    assert_eq!(records[0].kind, Kind::Copy);
}

#[rstest]
fn skips_approximate_binary_but_keeps_exact(#[values("sha1", "sha256")] format: &str) {
    let f = Fixture::new(format);
    let source = f.blob(b"\0\na\nb\nc\n");
    let edited = f.blob(b"\0\na\nb\nx\n");
    let old = f.tree(&[("100644", source, b"old")]);
    let edited_tree = f.tree(&[("100644", edited, b"new")]);
    let exact_tree = f.tree(&[("100644", source, b"new")]);
    let options = Options::default().with_approximate_binary(false);
    let objects = f.objects();
    let edited_records = objects
        .detect_rewrites(
            Some(old),
            Some(edited_tree),
            options,
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(edited_records.is_empty());
    let exact_records = objects
        .detect_rewrites(
            Some(old),
            Some(exact_tree),
            options,
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(exact_records.len(), 1);
    assert_eq!(exact_records[0].similarity, 100);
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
    let options = Options::default().with_copies(Copies::Modified);
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
        Options::default().with_candidate_limit(0),
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

/// Git skips edited detection when remaining sources times destinations exceeds the squared
/// limit, still reporting exact renames: one source and three destinations fit a limit of 2.
#[rstest]
#[case::within_squared_limit(
    2,
    None,
    &[(&b"source"[..], &b"edited"[..], 75), (b"exact", b"moved", 100)],
    "R075\tsource\tedited\nR100\texact\tmoved\nA\tnew1\nA\tnew2\n",
    "",
)]
#[case::over_squared_limit(
    1,
    Some((1, 3)),
    &[(&b"exact"[..], &b"moved"[..], 100)],
    "A\tedited\nR100\texact\tmoved\nA\tnew1\nA\tnew2\nD\tsource\n",
    "warning: exhaustive rename detection was skipped due to too many files.\n\
     warning: you may want to set your diff.renameLimit variable to at least 3 and retry the \
     command.\n",
)]
fn candidate_limit_matches_git(
    #[values("sha1", "sha256")] format: &str,
    #[case] limit: usize,
    #[case] skipped: Option<(usize, usize)>,
    #[case] expected: &[(&[u8], &[u8], u8)],
    #[case] git_stdout: &str,
    #[case] git_stderr: &str,
) {
    let f = Fixture::new(format);
    let exact = f.blob(b"exact\nfile\n");
    let old = f.tree(&[
        ("100644", exact, b"exact"),
        ("100644", f.blob(b"a\nb\nc\nd\n"), b"source"),
    ]);
    let new = f.tree(&[
        ("100644", exact, b"moved"),
        ("100644", f.blob(b"a\nb\nc\nx\n"), b"edited"),
        ("100644", f.blob(b"one\n"), b"new1"),
        ("100644", f.blob(b"two\n"), b"new2"),
    ]);
    let objects = f.objects();
    let options = Options::default().with_candidate_limit(limit);
    let detection = objects
        .detect_rewrites_or_exact(
            Some(old),
            Some(new),
            options,
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    let records: Vec<_> = detection
        .rewrites
        .iter()
        .map(|r| (r.source.as_slice(), r.target.as_slice(), r.similarity))
        .collect();
    assert_eq!(records, expected);
    assert_eq!(
        detection
            .inexact_skipped
            .map(|s| (s.sources, s.targets, s.limit, s.suggested_limit())),
        skipped.map(|(sources, targets)| (sources, targets, limit, 3))
    );
    let strict = objects.detect_rewrites(
        Some(old),
        Some(new),
        options,
        Limits::default(),
        None,
        &AtomicBool::new(false),
    );
    assert_eq!(
        strict.err().map(|e| e.to_string()),
        skipped.map(|(sources, targets)| format!(
            "rewrite candidates ({sources} sources, {targets} targets) exceed limit {limit}"
        ))
    );
    let observed = git_output(
        f.root.path(),
        &[
            "diff-tree",
            "-r",
            "--no-commit-id",
            "--name-status",
            "-M50%",
            &format!("-l{limit}"),
            &old.to_string(),
            &new.to_string(),
        ],
        b"",
    );
    assert_eq!(String::from_utf8(observed.stdout).unwrap(), git_stdout);
    assert_eq!(String::from_utf8(observed.stderr).unwrap(), git_stderr);
}

/// Writes each `(path, content)` as a regular-file tree entry.
fn blob_entries<'a>(
    f: &Fixture,
    files: &[(&'a [u8], &[u8])],
) -> Vec<(&'static str, ObjectId, &'a [u8])> {
    files
        .iter()
        .map(|&(path, content)| ("100644", f.blob(content), path))
        .collect()
}

/// Git counts files left unpaired by exact matching toward the limit: binary files always, and
/// empty files only when empty renames are detected (`track_empty`, Git's default, versus
/// `--no-rename-empty`). One edited rename plus such files at limit 1 shows which were counted.
#[rstest]
#[case::empty_targets_counted(
    &[(&b"src"[..], &b"a\nb\nc\nd\n"[..])],
    &[(&b"dst"[..], &b"a\nb\nc\nx\n"[..]), (b"e1", b""), (b"e2", b"")],
    true,
    &["--rename-empty"],
    Some((1, 3, 3)),
    "A\tdst\nA\te1\nA\te2\nD\tsrc\n",
)]
#[case::empty_sources_counted(
    &[(&b"src"[..], &b"a\nb\nc\nd\n"[..]), (b"e1", b""), (b"e2", b"")],
    &[(&b"dst"[..], &b"a\nb\nc\nx\n"[..])],
    true,
    &["--rename-empty"],
    Some((3, 1, 3)),
    "A\tdst\nD\te1\nD\te2\nD\tsrc\n",
)]
#[case::empty_not_counted(
    &[(&b"src"[..], &b"a\nb\nc\nd\n"[..]), (b"e1", b"")],
    &[(&b"dst"[..], &b"a\nb\nc\nx\n"[..]), (b"e2", b"")],
    false,
    &["--no-rename-empty"],
    None,
    "R075\tsrc\tdst\nD\te1\nA\te2\n",
)]
#[case::binary_counted(
    &[(&b"src"[..], &b"a\nb\nc\nd\n"[..])],
    &[(&b"dst"[..], &b"a\nb\nc\nx\n"[..]), (b"b1", b"bin\0one\n")],
    false,
    &["--no-rename-empty"],
    Some((1, 2, 2)),
    "A\tb1\nA\tdst\nD\tsrc\n",
)]
#[case::binary_counted_without_scorable_source(
    &[(&b"b1"[..], &b"bin\0one\n"[..]), (b"b2", b"bin\0two\n")],
    &[(&b"dst"[..], &b"a\nb\nc\nx\n"[..])],
    false,
    &["--no-rename-empty"],
    Some((2, 1, 2)),
    "D\tb1\nD\tb2\nA\tdst\n",
)]
fn candidate_counts_match_git(
    #[values("sha1", "sha256")] format: &str,
    #[case] old: &[(&[u8], &[u8])],
    #[case] new: &[(&[u8], &[u8])],
    #[case] track_empty: bool,
    #[case] empty_flag: &[&str],
    #[case] skipped: Option<(usize, usize, usize)>,
    #[case] git_stdout: &str,
) {
    let f = Fixture::new(format);
    let old = f.tree(&blob_entries(&f, old));
    let new = f.tree(&blob_entries(&f, new));
    let detection = f
        .objects()
        .detect_rewrites_or_exact(
            Some(old),
            Some(new),
            Options::default()
                .with_candidate_limit(1)
                .with_track_empty(track_empty)
                .with_approximate_binary(false),
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        detection
            .inexact_skipped
            .map(|s| (s.sources, s.targets, s.suggested_limit())),
        skipped
    );
    let (old, new) = (old.to_string(), new.to_string());
    let mut args = vec![
        "diff-tree",
        "-r",
        "--no-commit-id",
        "--name-status",
        "-M50%",
        "-l1",
    ];
    args.extend_from_slice(empty_flag);
    args.extend([old.as_str(), new.as_str()]);
    let observed = git_output(f.root.path(), &args, b"");
    assert_eq!(String::from_utf8(observed.stdout).unwrap(), git_stdout);
    let stderr = String::from_utf8(observed.stderr).unwrap();
    assert_eq!(
        stderr
            .lines()
            .find(|line| line.starts_with("warning: you may want"))
            .map(str::to_owned),
        skipped.map(|(_, _, suggested)| format!(
            "warning: you may want to set your diff.renameLimit variable to at least {suggested} \
             and retry the command."
        ))
    );
}

/// Over the limit, an exact match stays a rename and the report names the remaining counts.
#[test]
fn skipped_report_keeps_exact_rename() {
    let f = Fixture::new("sha1");
    let exact = f.blob(b"same\n");
    let old = f.tree(&[
        ("100644", exact, b"old"),
        ("100644", f.blob(b"a\nb\n"), b"gone"),
    ]);
    let new = f.tree(&[
        ("100644", exact, b"new"),
        ("100644", f.blob(b"a\nc\n"), b"added"),
    ]);
    let detection = f
        .objects()
        .detect_rewrites_or_exact(
            Some(old),
            Some(new),
            Options::default().with_candidate_limit(0),
            Limits::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(detection.rewrites.len(), 1);
    assert_eq!(detection.rewrites[0].kind, Kind::Rename);
    assert_eq!(
        detection
            .inexact_skipped
            .map(|s| (s.sources, s.targets, s.limit)),
        Some((1, 1, 0))
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
#[case::pairs(Limits::default().with_max_pairs(0), "pairs")]
#[case::work(Limits::default().with_max_work(0), "work")]
#[case::spans(Limits::default().with_max_spans(0), "spans")]
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
            Options::default().with_track_empty(track_empty),
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
                Options::default().with_similarity(100),
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
            Options::default().with_copies(Copies::Modified),
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
        matches!(f.objects().detect_rewrites(Some(old), Some(new), Options::default(), Limits::default().with_max_blob_bytes(0), None, &AtomicBool::new(false)), Err(Error::Read { id, .. }) if id == blob)
    );
}

#[rstest]
#[case::zero(0)]
#[case::too_large(101)]
fn invalid_similarity(#[case] similarity: u8) {
    let f = Fixture::new("sha1");
    assert!(
        matches!(f.objects().detect_rewrites(None, None, Options::default().with_similarity(similarity), Limits::default(), None, &AtomicBool::new(false)), Err(Error::Similarity(value)) if value == similarity)
    );
}
