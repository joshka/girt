//! Original byte fixtures, compared only through Git's public CLI; no upstream test inputs.
use std::ops::Range;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::AtomicBool;

use girt::content_diff::{BinaryMode, BlobContent, ContentDiff, DiffLimits, diff};
use girt::{
    EntryMode, InitKind, PackLimits, ReadLimits, Repository, Tree, TreeCompareLimits, TreeEntry,
};
use rstest::rstest;

fn git(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn patch(old: &[u8], new: &[u8]) -> Vec<u8> {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("old"), old).unwrap();
    std::fs::write(root.path().join("new"), new).unwrap();
    let result = git(
        root.path(),
        &[
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--text",
            "--minimal",
            "--diff-algorithm=myers",
            "--no-indent-heuristic",
            "--unified=0",
            "--",
            "old",
            "new",
        ],
    );
    assert_eq!(result.status.code(), Some(1), "{:?}", result.stderr);
    result.stdout
}

#[derive(Debug)]
struct Hunk {
    old: Range<usize>,
    new: Range<usize>,
    removed: Vec<u8>,
    added: Vec<u8>,
}

fn range(field: &str) -> Range<usize> {
    let (start, count) = field[1..].split_once(',').unwrap_or((&field[1..], "1"));
    let count: usize = count.parse().unwrap();
    let start: usize = start.parse().unwrap();
    let offset = if count == 0 { start } else { start - 1 };
    offset..offset + count
}

fn hunks(patch: &[u8]) -> Vec<Hunk> {
    let mut result: Vec<Hunk> = Vec::new();
    let mut removed = false;
    for line in patch.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"@@ ") {
            let header = std::str::from_utf8(line).unwrap();
            let fields: Vec<_> = header.split_whitespace().collect();
            result.push(Hunk {
                old: range(fields[1]),
                new: range(fields[2]),
                removed: vec![],
                added: vec![],
            });
        } else if let Some(hunk) = result.last_mut() {
            match line[0] {
                b'-' => {
                    hunk.removed.extend_from_slice(&line[1..]);
                    removed = true;
                }
                b'+' => {
                    hunk.added.extend_from_slice(&line[1..]);
                    removed = false;
                }
                b'\\' => {
                    let bytes = if removed {
                        &mut hunk.removed
                    } else {
                        &mut hunk.added
                    };
                    assert_eq!(bytes.pop(), Some(b'\n'));
                }
                _ => panic!("unexpected zero-context patch line: {line:?}"),
            }
        }
    }
    assert!(!result.is_empty());
    result
}

#[rstest]
#[case::replace(b"head\nold\ntail\n", b"head\nnew\ntail\n")]
#[case::addition(b"", b"new\nlast")]
#[case::deletion(b"old\nlast", b"")]
#[case::insert_middle(b"a\nc\n", b"a\nb\nc\n")]
#[case::two_regions(b"a\nb\nc\nd\n", b"A\nb\nc\nD\n")]
#[case::missing_final_lf(b"same\nold", b"same\nnew")]
#[case::final_lf_change(b"last\n", b"last")]
#[case::crlf(b"same\r\nold\r\n", b"same\r\nnew\r\n")]
#[case::non_utf8(b"\xff\n", b"\xfe\n")]
#[case::forced_nul(b"a\0\n", b"b\0\n")]
fn simple_zero_context_spans_agree_with_git(#[case] old: &[u8], #[case] new: &[u8]) {
    let expected = hunks(&patch(old, new));
    let ContentDiff::Text(text) = diff(
        old,
        new,
        BinaryMode::Text,
        DiffLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap() else {
        panic!("text")
    };
    let actual: Vec<_> = text
        .edits()
        .iter()
        .map(|e| {
            (
                e.old_lines.clone(),
                e.new_lines.clone(),
                &old[e.old_bytes.clone()],
                &new[e.new_bytes.clone()],
            )
        })
        .collect();
    let expected: Vec<_> = expected
        .iter()
        .map(|h| {
            (
                h.old.clone(),
                h.new.clone(),
                h.removed.as_slice(),
                h.added.as_slice(),
            )
        })
        .collect();
    assert_eq!(actual, expected);
}

#[rstest]
#[case::swap(b"a\nb\n", b"b\na\n")]
#[case::repeated(b"x\na\nx\nb\nx\n", b"x\nb\nx\na\nx\n")]
#[case::blank_lines(b"\n\nx\n\ny\n", b"\nx\n\n\ny\n")]
fn ambiguous_alignments_agree_semantically(#[case] old: &[u8], #[case] new: &[u8]) {
    let git = hunks(&patch(old, new));
    let ContentDiff::Text(text) = diff(
        old,
        new,
        BinaryMode::Text,
        DiffLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap() else {
        panic!("text")
    };
    let ours: Vec<_> = text
        .edits()
        .iter()
        .map(|e| Hunk {
            old: e.old_lines.clone(),
            new: e.new_lines.clone(),
            removed: old[e.old_bytes.clone()].to_vec(),
            added: new[e.new_bytes.clone()].to_vec(),
        })
        .collect();
    assert_eq!(reconstruct(old, new, &git), new);
    assert_eq!(reconstruct(old, new, &ours), new);
    assert_eq!(cost(&ours), cost(&git));
}

fn offset(bytes: &[u8], line: usize) -> usize {
    bytes
        .split_inclusive(|b| *b == b'\n')
        .take(line)
        .map(<[u8]>::len)
        .sum()
}

fn reconstruct(old: &[u8], new: &[u8], hunks: &[Hunk]) -> Vec<u8> {
    let mut result = Vec::new();
    let (mut old_end, mut new_end) = (0, 0);
    for hunk in hunks {
        let a = offset(old, hunk.old.start)..offset(old, hunk.old.end);
        let b = offset(new, hunk.new.start)..offset(new, hunk.new.end);
        assert_eq!(hunk.removed, old[a.clone()]);
        assert_eq!(hunk.added, new[b.clone()]);
        assert_eq!(&old[old_end..a.start], &new[new_end..b.start]);
        result.extend_from_slice(&old[old_end..a.start]);
        result.extend_from_slice(&hunk.added);
        old_end = a.end;
        new_end = b.end;
    }
    assert_eq!(&old[old_end..], &new[new_end..]);
    result.extend_from_slice(&old[old_end..]);
    result
}

fn cost(hunks: &[Hunk]) -> usize {
    hunks.iter().map(|h| h.old.len() + h.new.len()).sum()
}

#[test]
fn ordinary_nul_binary_classification_agrees_with_git() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("old"), b"a\0").unwrap();
    std::fs::write(root.path().join("new"), b"b\0").unwrap();
    let git = git(
        root.path(),
        &[
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--",
            "old",
            "new",
        ],
    );
    assert_eq!(git.status.code(), Some(1));
    assert!(
        git.stdout
            .windows(b"Binary files".len())
            .any(|bytes| bytes == b"Binary files")
    );
    assert!(matches!(
        diff(
            b"a\0",
            b"b\0",
            BinaryMode::Auto,
            DiffLimits::default(),
            &AtomicBool::new(false)
        )
        .unwrap(),
        ContentDiff::Binary { .. }
    ));
}

#[rstest]
#[case::loose(false)]
#[case::packed(true)]
fn tree_changes_load_verified_content_for_diff(#[case] packed: bool) {
    let fixture = tree_fixture(packed);
    let repo = Repository::open(fixture.0.path().join("repo")).unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    let cancel = AtomicBool::new(false);
    let changes = objects
        .compare_trees(
            Some(fixture.1),
            Some(fixture.2),
            TreeCompareLimits::default(),
            &cancel,
        )
        .unwrap();
    let change = &changes[0];
    assert_eq!(change.path, b"file");
    let content =
        BlobContent::read(&objects, change, ReadLimits::default(), 1024, &cancel).unwrap();
    assert_eq!(content.old, b"old\r\n");
    assert_eq!(content.new, b"new\xff");
    let ContentDiff::Text(text) = diff(
        &content.old,
        &content.new,
        BinaryMode::Auto,
        DiffLimits::default(),
        &cancel,
    )
    .unwrap() else {
        panic!("text")
    };
    assert_eq!(text.edits()[0].old_bytes, 0..5);
    assert_eq!(text.edits()[0].new_bytes, 0..4);
}

fn tree_fixture(packed: bool) -> (tempfile::TempDir, girt::ObjectId, girt::ObjectId) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(root.path().join("repo"), InitKind::Bare).unwrap();
    let path = root.path().join("repo");
    // Git independently writes the byte payloads; no filesystem text conversion is involved.
    std::fs::write(root.path().join("old"), b"old\r\n").unwrap();
    std::fs::write(root.path().join("new"), b"new\xff").unwrap();
    let output = git(
        &path,
        &["hash-object", "-w", "--no-filters", "../old", "../new"],
    );
    assert!(output.status.success(), "{:?}", output.stderr);
    let ids: Vec<girt::ObjectId> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|id| id.parse().unwrap())
        .collect();
    let store = repo.loose_objects().unwrap();
    let old = store
        .write_tree(
            &Tree::new(vec![TreeEntry {
                name: b"file".to_vec(),
                mode: EntryMode::Blob,
                id: ids[0],
            }])
            .unwrap(),
        )
        .unwrap();
    let new = store
        .write_tree(
            &Tree::new(vec![TreeEntry {
                name: b"file".to_vec(),
                mode: EntryMode::Executable,
                id: ids[1],
            }])
            .unwrap(),
        )
        .unwrap();
    if packed {
        // Git can retain trees directly in refs; girt's Windows ref backend is not involved.
        assert!(
            git(&path, &["update-ref", "refs/test/old", &old.to_string()])
                .status
                .success()
        );
        assert!(
            git(&path, &["update-ref", "refs/test/new", &new.to_string()])
                .status
                .success()
        );
        assert!(git(&path, &["repack", "-a", "-d"]).status.success());
        assert!(git(&path, &["prune-packed"]).status.success());
        assert!(
            std::fs::read_dir(path.join("objects/pack"))
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "pack"))
        );
        let blob = ids[0].to_string();
        assert!(
            !path
                .join("objects")
                .join(&blob[..2])
                .join(&blob[2..])
                .exists()
        );
    }
    // Keep the parent TempDir alive but hand Repository::open its repo child in the test.
    (root, old, new)
}
