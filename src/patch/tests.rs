use std::sync::atomic::AtomicBool;

use rstest::rstest;

use super::*;
use crate::content_diff::diff;
use crate::tree::TreeEditor;
use crate::{InitKind, ObjectFormat, Repository};

/// Writes one tree per side holding `file` with each content, returning the change entries.
fn entries(repo: &Repository, old: &[u8], new: &[u8]) -> Vec<Entry> {
    let loose = repo.loose_objects();
    let mut editor = TreeEditor::new(&loose);
    editor.set(b"file", loose.write_blob(old).unwrap()).unwrap();
    let old = editor.write().unwrap();
    editor.set(b"file", loose.write_blob(new).unwrap()).unwrap();
    let new = editor.write().unwrap();
    let objects = repo.objects().unwrap();
    objects.diff_trees(Some(old), Some(new)).unwrap().entries
}

fn repository() -> (tempfile::TempDir, Repository) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("r"),
        InitKind::Bare,
    )
    .unwrap();
    (directory, repo)
}

fn hunks(old: &[u8], new: &[u8], context: usize) -> String {
    let ContentDiff::Text(text) = diff(
        old,
        new,
        BinaryMode::Text,
        DiffLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap() else {
        panic!("expected text");
    };
    let mut out = Vec::new();
    write_hunks(&mut out, &text, context).unwrap();
    String::from_utf8(out).unwrap()
}

/// Hunk ranges omit a count of one and start an empty side at the preceding line.
#[rstest]
#[case::insert_at_start(b"b\n", b"a\nb\n", "@@ -0,0 +1 @@\n+a\n")]
#[case::delete_middle(b"a\nb\nc\n", b"a\nc\n", "@@ -2 +1,0 @@ a\n-b\n")]
#[case::replace_two(b"a\nb\nc\n", b"x\ny\nc\n", "@@ -1,2 +1,2 @@\n-a\n-b\n+x\n+y\n")]
#[case::unterminated(
    b"a",
    b"b",
    "@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n\\ No newline at end of file\n"
)]
fn zero_context_ranges(#[case] old: &[u8], #[case] new: &[u8], #[case] expected: &str) {
    assert_eq!(hunks(old, new, 0), expected);
}

/// A gap of exactly twice the context joins two changes; one more line splits them.
#[rstest]
#[case::joined(
    b"a\nb\nc\nd\n",
    b"A\nb\nc\nD\n",
    "@@ -1,4 +1,4 @@\n-a\n+A\n b\n c\n-d\n+D\n"
)]
#[case::split(
    b"a\nb\nc\nd\ne\n",
    b"A\nb\nc\nd\nE\n",
    "@@ -1,2 +1,2 @@\n-a\n+A\n b\n@@ -4,2 +4,2 @@ c\n d\n-e\n+E\n"
)]
fn adjacent_changes_share_hunks_within_twice_the_context(
    #[case] old: &[u8],
    #[case] new: &[u8],
    #[case] expected: &str,
) {
    assert_eq!(hunks(old, new, 1), expected);
}

/// Function context is cut to 80 bytes before trailing whitespace is removed.
#[test]
fn function_context_is_truncated_then_trimmed() {
    let name = format!("{}    tail", "f".repeat(78));
    let old = format!("{name}\n x\n");
    let new = format!("{name}\n y\n");
    assert_eq!(
        hunks(old.as_bytes(), new.as_bytes(), 0),
        format!("@@ -2 +2 @@ {}\n- x\n+ y\n", "f".repeat(78))
    );
}

/// A set flag stops rendering before any output.
#[test]
fn cancellation_writes_nothing() {
    let (_directory, repo) = repository();
    let entries = entries(&repo, b"a\n", b"b\n");
    let mut out = Vec::new();
    let error = repo
        .objects()
        .unwrap()
        .write_patch_with(
            &mut out,
            &entries,
            Options::default(),
            None,
            Limits::default(),
            &AtomicBool::new(true),
        )
        .unwrap_err();
    assert!(matches!(error, Error::Cancelled));
    assert!(out.is_empty());
}

/// A content limit names the entry's path; the headers already written stay in the output.
#[test]
fn limit_failure_names_the_path_after_headers() {
    let (_directory, repo) = repository();
    let entries = entries(&repo, b"a\n", b"b\n");
    let mut out = Vec::new();
    let limits = Limits::default().with_diff(DiffLimits::default().with_max_input_bytes(3));
    let error = repo
        .objects()
        .unwrap()
        .write_patch_with(
            &mut out,
            &entries,
            Options::default(),
            None,
            limits,
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(&error, Error::Read { path, .. } if path == b"file"));
    assert!(out.starts_with(b"diff --git a/file b/file\nindex "));
}

/// A tree referencing a missing blob reports the read failure instead of an empty side.
#[test]
fn missing_blob_is_an_error() {
    let (_directory, repo) = repository();
    let loose = repo.loose_objects();
    let mut editor = TreeEditor::new(&loose);
    editor
        .set(b"file", ObjectId::for_blob(ObjectFormat::Sha1, b"absent"))
        .unwrap();
    let tree = editor.write().unwrap();
    let objects = repo.objects().unwrap();
    let change = objects.compare_trees(None, Some(tree)).unwrap().remove(0);
    let entries = [Entry::Change(change)];
    let error = objects.write_patch(Vec::new(), &entries).unwrap_err();
    assert!(matches!(
        error,
        Error::Read { source, .. } if matches!(*source, ContentReadError::Missing(_))
    ));
}
