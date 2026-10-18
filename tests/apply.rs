//! Original patches and Git CLI fixtures; no upstream implementation or test source.
#[path = "support/apply_git.rs"]
mod support;
use girt::apply::{Error, Limits, Patch};
use rstest::rstest;
use support::{git, git_output};

#[rstest]
#[case::replace(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n", b"old\n", b"new\n")]
#[case::add(b"--- /dev/null\n+++ b/f\n@@ -0,0 +1 @@\n+new\n", b"", b"new\n")]
#[case::delete(b"--- a/f\n+++ /dev/null\n@@ -1 +0,0 @@\n-old\n", b"old\n", b"")]
#[case::no_final_lf(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n", b"old", b"new")]
#[case::bytes(
    b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-\xff\r\n+\xfe\r\n",
    b"\xff\r\n",
    b"\xfe\r\n"
)]
#[case::context(
    b"--- a/f\n+++ b/f\n@@ -2,2 +2,2 @@\n same\n-old\n+new\n",
    b"before\nsame\nold\nafter\n",
    b"before\nsame\nnew\nafter\n"
)]
fn applies_exact_bytes(#[case] patch: &[u8], #[case] before: &[u8], #[case] after: &[u8]) {
    let patch = Patch::parse(patch).unwrap();

    let result = patch.files()[0].apply(before).unwrap();

    assert_eq!(result.content, after);
    assert_eq!(result.hunks.len(), 1);
}

#[test]
fn preserves_quoted_paths_and_traditional_timestamps() {
    let patch = Patch::parse(
        b"--- \"a/a\\t\\377\"\n+++ b/name with spaces\t2026-01-01\n@@ -1 +1 @@\n-a\n+b\n",
    )
    .unwrap();

    assert_eq!(patch.files()[0].old_path(), Some(b"a/a\t\xff".as_slice()));
    assert_eq!(
        patch.files()[0].new_path(),
        Some(b"b/name with spaces".as_slice())
    );
}

#[rstest]
#[case::empty(b"")]
#[case::truncated(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n")]
#[case::overflow(b"--- a/f\n+++ b/f\n@@ -999999999999999999999999999999 +1 @@\n-a\n+b\n")]
#[case::wrong_count(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n-b\n")]
#[case::zero_start(b"--- a/f\n+++ b/f\n@@ -0 +1 @@\n-a\n+b\n")]
fn rejects_malformed_input(#[case] bytes: &[u8]) {
    assert!(matches!(Patch::parse(bytes), Err(Error::Malformed { .. })));
}

#[rstest]
#[case::combined(b"diff --cc f\n", "combined diff")]
#[case::binary(b"GIT binary patch\n", "binary patch")]
fn refuses_unsupported_formats(#[case] bytes: &[u8], #[case] expected: &str) {
    assert!(
        matches!(Patch::parse(bytes), Err(Error::Unsupported { feature, .. }) if feature == expected)
    );
}

#[test]
fn late_failure_returns_no_partial_result() {
    let patch =
        Patch::parse(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n@@ -3 +3 @@\n-x\n+y\n").unwrap();
    let before = b"a\nmiddle\nc\n";

    assert_eq!(
        patch.files()[0].apply(before).unwrap_err(),
        Error::Mismatch { line: 6 }
    );
    assert_eq!(before, b"a\nmiddle\nc\n");
}

#[test]
fn enforces_parse_and_output_bounds() {
    let mut limits = Limits::default();
    limits.max_input_bytes = 0;
    assert_eq!(
        Patch::parse_with(b"x", limits).unwrap_err(),
        Error::Limit("input bytes")
    );

    let patch = Patch::parse(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+long\n").unwrap();
    limits = Limits::default();
    limits.max_output_bytes = 2;
    assert_eq!(
        patch.files()[0].apply_with(b"a\n", limits).unwrap_err(),
        Error::Limit("output bytes")
    );
}

#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn applies_git_generated_patch_to_identical_tree(#[case] format: &str) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--template=",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
    std::fs::write(root.path().join("f"), b"one\ntwo\nthree\n").unwrap();
    git(root.path(), &["add", "f"], b"");
    std::fs::write(root.path().join("f"), b"one\nchanged\nthree\nlast").unwrap();
    let bytes = git(
        root.path(),
        &["diff", "--no-ext-diff", "--no-textconv", "--binary"],
        b"",
    );
    let patch = Patch::parse(&bytes).unwrap();

    let applied = patch.files()[0].apply(b"one\ntwo\nthree\n").unwrap();
    git(root.path(), &["checkout", "--", "f"], b"");
    git(root.path(), &["apply", "--cached", "--check"], &bytes);
    git(root.path(), &["apply", "--cached"], &bytes);
    let expected = git(root.path(), &["write-tree"], b"");
    std::fs::write(root.path().join("f"), applied.content).unwrap();
    git(root.path(), &["add", "f"], b"");

    assert_eq!(git(root.path(), &["write-tree"], b""), expected);
}

#[rstest]
#[case::file_limit(0, 10, "files")]
#[case::hunk_limit(10, 0, "hunks")]
fn enforces_structural_bounds(
    #[case] files: usize,
    #[case] hunks: usize,
    #[case] expected: &'static str,
) {
    let mut limits = Limits::default();
    limits.max_files = files;
    limits.max_hunks = hunks;

    assert_eq!(
        Patch::parse_with(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n", limits).unwrap_err(),
        Error::Limit(expected)
    );
}

#[rstest]
#[case::incomplete_git(b"diff --git a/f b/f\n")]
#[case::orphan_index(b"index abc..def 100644\n")]
#[case::bad_id(b"diff --git a/f b/f\nindex zz..def 100644\n")]
#[case::after_eof(b"--- a/f\n+++ b/f\n@@ -1 +1,2 @@\n-a\n+b\n\\ No newline at end of file\n+c\n")]
#[case::incomplete_rename(b"diff --git a/f b/g\nrename from f\n")]
fn rejects_invalid_framing(#[case] bytes: &[u8]) {
    assert!(matches!(Patch::parse(bytes), Err(Error::Malformed { .. })));
}

#[test]
fn round_trips_girt_rendered_patch_against_git() {
    use girt::Repository;
    use girt::tree::TreeEditor;
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--bare", "--template=", "."], b"");
    let repository = Repository::open(root.path()).unwrap();
    let loose = repository.loose_objects();
    let mut editor = TreeEditor::new(&loose);
    editor
        .set(b"f", loose.write_blob(b"before\n").unwrap())
        .unwrap();
    let before = editor.write().unwrap();
    editor
        .set(b"f", loose.write_blob(b"after\n").unwrap())
        .unwrap();
    let after = editor.write().unwrap();
    let objects = repository.objects().unwrap();
    let diff = objects.diff_trees(Some(before), Some(after)).unwrap();
    let mut bytes = Vec::new();
    objects.write_patch(&mut bytes, &diff.entries).unwrap();

    let parsed = Patch::parse(&bytes).unwrap();
    assert_eq!(
        parsed.files()[0].apply(b"before\n").unwrap().content,
        b"after\n"
    );
    git(root.path(), &["read-tree", &before.to_string()], b"");
    git(root.path(), &["apply", "--cached"], &bytes);

    assert_eq!(
        git(root.path(), &["write-tree"], b""),
        format!("{after}\n").as_bytes()
    );
}

#[test]
fn limits_content_line_allocations() {
    let mut limits = Limits::default();
    limits.max_lines = 1;

    assert_eq!(
        Patch::parse_with(b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n", limits).unwrap_err(),
        Error::Limit("content lines")
    );
}

#[rstest]
#[case::remaining(
    b"a\nc\n",
    b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n\\ No newline at end of file\n"
)]
#[case::later_hunk(
    b"a\nc\n",
    b"--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n\\ No newline at end of file\n@@ -2 +2 @@\n-c\n+d\n"
)]
fn missing_final_newline_cannot_join_trailing_content(#[case] before: &[u8], #[case] bytes: &[u8]) {
    let patch = Patch::parse(bytes).unwrap();

    assert!(matches!(
        patch.files()[0].apply(before),
        Err(Error::Mismatch { .. })
    ));
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--template=", "."], b"");
    std::fs::write(root.path().join("f"), before).unwrap();
    assert!(
        !git_output(root.path(), &["apply", "--check"], bytes)
            .status
            .success()
    );
}
