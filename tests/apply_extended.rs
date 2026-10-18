//! Extended patch metadata from original fixtures and isolated Git CLI output.
#[path = "support/apply_git.rs"]
mod support;
use girt::apply::{FileOperation, Patch};
use girt::tree::TreeEditor;
use girt::{EntryMode, ObjectId, Repository, TreeValue};
use rstest::rstest;
use support::git;

struct Fixture {
    root: tempfile::TempDir,
    repo: Repository,
}
impl Fixture {
    fn new(format: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        git(
            root.path(),
            &[
                "init",
                "--bare",
                "--template=",
                &format!("--object-format={format}"),
                ".",
            ],
            b"",
        );
        let repo = Repository::open(root.path()).unwrap();
        Self { root, repo }
    }
    fn tree(&self, entries: &[(&[u8], EntryMode, &[u8])]) -> ObjectId {
        let loose = self.repo.loose_objects();
        let mut editor = TreeEditor::new(&loose);
        for (path, mode, bytes) in entries {
            editor
                .set_entry(
                    path,
                    TreeValue {
                        mode: *mode,
                        id: loose.write_blob(bytes).unwrap(),
                    },
                )
                .unwrap();
        }
        editor.write().unwrap()
    }
    fn compare_reverse(&self, before: ObjectId, after: ObjectId, args: &[&str]) {
        let before_hex = before.to_string();
        let after_hex = after.to_string();
        let mut command = vec![
            "diff-tree",
            "-r",
            "-p",
            "--no-commit-id",
            "--binary",
            "--full-index",
        ];
        command.extend(args);
        command.extend([before_hex.as_str(), after_hex.as_str()]);
        let bytes = git(self.root.path(), &command, b"");
        let reversed = Patch::parse(&bytes).unwrap().reversed().unwrap();

        let result = reversed
            .apply_tree(&self.repo.objects().unwrap(), after)
            .unwrap();
        assert_eq!(result.tree().id(), before);
        git(self.root.path(), &["read-tree", &after_hex], b"");
        let index = self.repo.read_index(Default::default()).unwrap().unwrap();
        let applied = reversed
            .apply_index(&self.repo.objects().unwrap(), &index)
            .unwrap();
        assert_eq!(applied.index().tree_id(), Some(before));
        git(self.root.path(), &["apply", "--cached", "-R"], &bytes);
        assert_eq!(
            git(self.root.path(), &["write-tree"], b""),
            format!("{before}\n").as_bytes()
        );

        let forward = reversed.reversed().unwrap();
        assert_eq!(
            forward
                .apply_tree(&self.repo.objects().unwrap(), before)
                .unwrap()
                .tree()
                .id(),
            after
        );
    }

    fn compare(&self, before: ObjectId, after: ObjectId, args: &[&str]) {
        let before_hex = before.to_string();
        let after_hex = after.to_string();
        let mut command = vec![
            "diff-tree",
            "-r",
            "-p",
            "--no-commit-id",
            "--binary",
            "--full-index",
        ];
        command.extend(args);
        command.extend([before_hex.as_str(), after_hex.as_str()]);
        let bytes = git(self.root.path(), &command, b"");
        let patch = Patch::parse(&bytes).unwrap();

        let result = patch
            .apply_tree(&self.repo.objects().unwrap(), before)
            .unwrap();
        result.install_objects(&self.repo.loose_objects()).unwrap();

        assert_eq!(
            result.tree().id(),
            after,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        git(self.root.path(), &["read-tree", &before_hex], b"");
        let index = self.repo.read_index(Default::default()).unwrap().unwrap();
        let applied = patch
            .apply_index(&self.repo.objects().unwrap(), &index)
            .unwrap();
        assert_eq!(applied.tree().id(), after);
        assert_eq!(applied.index().tree_id(), Some(after));
        git(self.root.path(), &["apply", "--cached"], &bytes);
        assert_eq!(
            git(self.root.path(), &["write-tree"], b""),
            format!("{after}\n").as_bytes()
        );
    }
}

#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn git_mode_add_delete_and_type_changes(#[case] format: &str) {
    let fixture = Fixture::new(format);
    let before = fixture.tree(&[
        (b"mode", EntryMode::Blob, b"same\n"),
        (b"gone", EntryMode::Blob, b""),
        (b"link", EntryMode::Blob, b"target"),
    ]);
    let after = fixture.tree(&[
        (b"mode", EntryMode::Executable, b"same\n"),
        (b"empty", EntryMode::Blob, b""),
        (b"link", EntryMode::Symlink, b"target"),
    ]);

    fixture.compare(before, after, &[]);
    fixture.compare_reverse(before, after, &[]);
}

#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn git_renames_and_copies_with_edits(#[case] format: &str) {
    let fixture = Fixture::new(format);
    let before = fixture.tree(&[
        (
            b"old name",
            EntryMode::Blob,
            b"one\ntwo\nthree\nfour\nfive\n",
        ),
        (b"source", EntryMode::Blob, b"copy\nsource\ncontent\n"),
    ]);
    let after = fixture.tree(&[
        (
            b"new name",
            EntryMode::Executable,
            b"one\ntwo\nchanged\nfour\nfive\n",
        ),
        (b"source", EntryMode::Blob, b"copy\nsource\ncontent\n"),
        (b"copy", EntryMode::Blob, b"copy\nsource\ncontent\n"),
    ]);

    fixture.compare(
        before,
        after,
        &["--find-renames", "--find-copies", "--find-copies-harder"],
    );
}

#[test]
fn rename_cycle_reads_original_preimages() {
    let fixture = Fixture::new("sha1");
    let before = fixture.tree(&[
        (b"a", EntryMode::Blob, b"first\n"),
        (b"b", EntryMode::Blob, b"second\n"),
    ]);
    let after = fixture.tree(&[
        (b"a", EntryMode::Blob, b"second\n"),
        (b"b", EntryMode::Blob, b"first\n"),
    ]);
    let bytes = b"diff --git a/a b/b\nsimilarity index 100%\nrename from a\nrename to b\ndiff --git a/b b/a\nsimilarity index 100%\nrename from b\nrename to a\n";
    let patch = Patch::parse(bytes).unwrap();

    let result = patch
        .apply_tree(&fixture.repo.objects().unwrap(), before)
        .unwrap();

    assert_eq!(result.tree().id(), after);
    assert_eq!(patch.files()[0].operation(), FileOperation::Rename);
    assert_eq!(patch.files()[0].similarity(), Some(100));
    git(
        fixture.root.path(),
        &["read-tree", &before.to_string()],
        b"",
    );
    git(fixture.root.path(), &["apply", "--cached"], bytes);
    assert_eq!(
        git(fixture.root.path(), &["write-tree"], b""),
        format!("{after}\n").as_bytes()
    );
}

#[rstest]
#[case::duplicate_mode(b"diff --git a/f b/f\nold mode 100644\nold mode 100644\nnew mode 100755\n")]
#[case::incomplete_copy(b"diff --git a/a b/b\ncopy from a\n")]
#[case::bad_similarity(b"diff --git a/a b/b\nsimilarity index 101%\ncopy from a\ncopy to b\n")]
#[case::mixed_operations(b"diff --git a/a b/b\nrename from a\ncopy to b\n")]
fn refuses_inconsistent_metadata(#[case] bytes: &[u8]) {
    assert!(Patch::parse(bytes).is_err());
}

#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn git_binary_literals_and_deltas(#[case] format: &str) {
    let fixture = Fixture::new(format);
    let original: Vec<u8> = (0..8192).map(|value| (value % 256) as u8).collect();
    let mut changed = original.clone();
    changed[500..510].copy_from_slice(b"changed!!!");
    let before = fixture.tree(&[
        (b"delta", EntryMode::Blob, &original),
        (b"deleted", EntryMode::Blob, b"\0gone\xff"),
    ]);
    let literal = literal_bytes();
    let after = fixture.tree(&[
        (b"delta", EntryMode::Blob, &changed),
        (b"added", EntryMode::Blob, &literal),
    ]);

    fixture.compare(before, after, &[]);
    fixture.compare_reverse(before, after, &[]);
}

#[test]
fn binary_application_checks_preimage_and_result_identity() {
    let fixture = Fixture::new("sha1");
    let before = fixture.tree(&[(b"f", EntryMode::Blob, b"\0old\xff")]);
    let after = fixture.tree(&[(b"f", EntryMode::Blob, b"\0new\xfe")]);
    let bytes = git(
        fixture.root.path(),
        &[
            "diff-tree",
            "-r",
            "-p",
            "--binary",
            "--full-index",
            &before.to_string(),
            &after.to_string(),
        ],
        b"",
    );
    let patch = Patch::parse(&bytes).unwrap();

    assert_eq!(
        patch.files()[0].binary_kind(),
        Some(girt::apply::BinaryKind::Literal)
    );
    assert_eq!(
        patch.files()[0].apply(b"\0old\xff").unwrap().content,
        b"\0new\xfe"
    );
    assert!(matches!(
        patch.files()[0].apply(b"\0bad\xff"),
        Err(girt::apply::Error::Mismatch { .. })
    ));
    let wrong_id = ObjectId::for_blob(fixture.repo.object_format(), b"not the new blob");
    let new_id = ObjectId::for_blob(fixture.repo.object_format(), b"\0new\xfe");
    let corrupted = String::from_utf8(bytes)
        .unwrap()
        .replace(&new_id.to_string(), &wrong_id.to_string());
    let corrupted = Patch::parse(corrupted.as_bytes()).unwrap();
    assert!(matches!(
        corrupted.files()[0].apply(b"\0old\xff"),
        Err(girt::apply::Error::Mismatch { .. })
    ));
}

#[test]
fn binary_decode_and_application_have_independent_bounds() {
    let fixture = Fixture::new("sha1");
    let before = fixture.tree(&[(b"f", EntryMode::Blob, b"\0old\xff")]);
    let after = fixture.tree(&[(b"f", EntryMode::Blob, b"\0new\xfe")]);
    let bytes = git(
        fixture.root.path(),
        &[
            "diff-tree",
            "-r",
            "-p",
            "--binary",
            "--full-index",
            &before.to_string(),
            &after.to_string(),
        ],
        b"",
    );
    let mut limits = girt::apply::Limits::default();
    limits.max_binary_bytes = 5;

    assert!(matches!(
        Patch::parse_with(&bytes, limits),
        Err(girt::apply::Error::Limit("binary decoded bytes"))
    ));
    let patch = Patch::parse(&bytes).unwrap();
    limits = girt::apply::Limits::default();
    limits.max_output_bytes = 1;
    assert!(matches!(
        patch.files()[0].apply_with(b"\0old\xff", limits),
        Err(girt::apply::Error::Limit("output bytes"))
    ));
}

#[rstest]
#[case::base85_digit("H0000000000", "H.000000000")]
#[case::base85_overflow("H0000000000", "H~~~~~~~~~~")]
#[case::encoded_length("H0000000000", "A0000000000")]
#[case::zlib("H0000000000", "H0000000000")]
fn refuses_malformed_binary_payloads(#[case] original: &str, #[case] replacement: &str) {
    let bytes = format!(
        "diff --git a/f b/f\nindex {}..{} 100644\nGIT binary patch\nliteral 1\n{}\n\n",
        "1".repeat(40),
        "2".repeat(40),
        original
    );
    let bytes = bytes.replace(original, replacement);

    assert!(Patch::parse(bytes.as_bytes()).is_err());
}

#[test]
fn delta_patch_reports_kind_and_rejects_small_output_bound() {
    let fixture = Fixture::new("sha1");
    let original: Vec<u8> = (0..8192).map(|value| (value % 256) as u8).collect();
    let mut changed = original.clone();
    changed[500..510].copy_from_slice(b"changed!!!");
    let before = fixture.tree(&[(b"f", EntryMode::Blob, &original)]);
    let after = fixture.tree(&[(b"f", EntryMode::Blob, &changed)]);
    let bytes = git(
        fixture.root.path(),
        &[
            "diff-tree",
            "-r",
            "-p",
            "--binary",
            "--full-index",
            &before.to_string(),
            &after.to_string(),
        ],
        b"",
    );
    let patch = Patch::parse(&bytes).unwrap();
    let mut limits = girt::apply::Limits::default();
    limits.max_output_bytes = 100;

    assert_eq!(
        patch.files()[0].binary_kind(),
        Some(girt::apply::BinaryKind::Delta)
    );
    assert!(patch.files()[0].reverse_binary_kind().is_some());
    assert_eq!(patch.files()[0].apply(&original).unwrap().content, changed);
    assert!(matches!(
        patch.files()[0].apply_with(&original, limits),
        Err(girt::apply::Error::Limit("output bytes"))
    ));
}

fn literal_bytes() -> Vec<u8> {
    let mut state = 0x4d59_5df4_d0f3_3173u64;
    let mut bytes = vec![0];
    for _ in 0..4096 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.push(state as u8);
    }
    bytes
}

#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn file_can_become_directory_with_original_source_reads(#[case] format: &str) {
    let fixture = Fixture::new(format);
    let before = fixture.tree(&[(b"x", EntryMode::Blob, b"old\n")]);
    let after = fixture.tree(&[(b"x/y", EntryMode::Blob, b"new\n")]);

    fixture.compare(before, after, &[]);
}

#[rstest]
#[case::ancestor_first(b"--- /dev/null\n+++ b/new\n@@ -0,0 +1 @@\n+file\n--- /dev/null\n+++ b/new/child\n@@ -0,0 +1 @@\n+child\n")]
#[case::descendant_first(b"--- /dev/null\n+++ b/new/child\n@@ -0,0 +1 @@\n+child\n--- /dev/null\n+++ b/new\n@@ -0,0 +1 @@\n+file\n")]
fn refuses_destinations_that_are_also_directories(#[case] bytes: &[u8]) {
    let fixture = Fixture::new("sha1");
    let base = fixture.tree(&[]);

    assert!(matches!(
        Patch::parse(bytes)
            .unwrap()
            .apply_tree(&fixture.repo.objects().unwrap(), base),
        Err(girt::apply::TreeError::Path(_))
    ));
    git(fixture.root.path(), &["read-tree", &base.to_string()], b"");
    git(
        fixture.root.path(),
        &["apply", "--cached", "--check"],
        bytes,
    );
    assert!(
        !support::git_output(fixture.root.path(), &["apply", "--cached"], bytes)
            .status
            .success()
    );
    assert_eq!(
        git(fixture.root.path(), &["write-tree"], b""),
        format!("{base}\n").as_bytes()
    );
}

#[test]
fn dissimilarity_is_explicitly_unsupported() {
    assert!(matches!(
        Patch::parse(b"diff --git a/f b/f\ndissimilarity index 99%\n"),
        Err(girt::apply::Error::Unsupported {
            feature: "dissimilarity index",
            ..
        })
    ));
}

#[test]
fn binary_blocks_require_terminating_blank_line() {
    let fixture = Fixture::new("sha1");
    let before = fixture.tree(&[(b"f", EntryMode::Blob, b"\0old\xff")]);
    let after = fixture.tree(&[(b"f", EntryMode::Blob, b"\0new\xfe")]);
    let mut bytes = git(
        fixture.root.path(),
        &[
            "diff-tree",
            "-r",
            "-p",
            "--binary",
            "--full-index",
            &before.to_string(),
            &after.to_string(),
        ],
        b"",
    );
    assert_eq!(bytes.pop(), Some(b'\n'));

    assert!(matches!(
        Patch::parse(&bytes),
        Err(girt::apply::Error::Malformed {
            reason: "missing binary block terminator",
            ..
        })
    ));
}

#[test]
fn reverse_text_rename_preserves_missing_newline() {
    let fixture = Fixture::new("sha1");
    let before = fixture.tree(&[(b"old", EntryMode::Blob, b"one\ntwo\nthree\nfour")]);
    let after = fixture.tree(&[(b"new", EntryMode::Executable, b"one\nchanged\nthree\nfour")]);

    fixture.compare_reverse(before, after, &["--find-renames=20%"]);
}

#[test]
fn reverse_copy_requires_absent_destination_like_git() {
    let fixture = Fixture::new("sha1");
    let before = fixture.tree(&[(b"source", EntryMode::Blob, b"one\ntwo\n")]);
    let after = fixture.tree(&[
        (b"source", EntryMode::Blob, b"one\ntwo\n"),
        (b"copy", EntryMode::Blob, b"one\ntwo\n"),
    ]);
    let bytes =
        b"diff --git a/source b/copy\nsimilarity index 100%\ncopy from source\ncopy to copy\n";
    let reversed = Patch::parse(bytes).unwrap().reversed().unwrap();

    assert!(
        reversed
            .apply_tree(&fixture.repo.objects().unwrap(), after)
            .is_err()
    );
    git(fixture.root.path(), &["read-tree", &after.to_string()], b"");
    assert!(
        !support::git_output(fixture.root.path(), &["apply", "--cached", "-R"], bytes)
            .status
            .success()
    );
    assert_eq!(
        git(fixture.root.path(), &["write-tree"], b""),
        format!("{after}\n").as_bytes()
    );

    let only_copy = fixture.tree(&[(b"copy", EntryMode::Blob, b"one\ntwo\n")]);
    assert_eq!(
        reversed
            .apply_tree(&fixture.repo.objects().unwrap(), only_copy)
            .unwrap()
            .tree()
            .id(),
        fixture.tree(&[
            (b"copy", EntryMode::Blob, b"one\ntwo\n"),
            (b"source", EntryMode::Blob, b"one\ntwo\n")
        ])
    );
    assert_ne!(only_copy, before);
}

#[test]
fn binary_reversal_requires_reverse_data() {
    let fixture = Fixture::new("sha1");
    let before = fixture.tree(&[(b"f", EntryMode::Blob, b"\0old")]);
    let after = fixture.tree(&[(b"f", EntryMode::Blob, b"\0new")]);
    let bytes = git(
        fixture.root.path(),
        &[
            "diff-tree",
            "-r",
            "-p",
            "--binary",
            "--full-index",
            &before.to_string(),
            &after.to_string(),
        ],
        b"",
    );
    let first_end = bytes.windows(2).position(|pair| pair == b"\n\n").unwrap() + 2;
    let patch = Patch::parse(&bytes[..first_end]).unwrap();

    assert!(matches!(
        patch.reversed(),
        Err(girt::apply::Error::Unsupported {
            feature: "binary reversal without a reverse block",
            ..
        })
    ));
}
