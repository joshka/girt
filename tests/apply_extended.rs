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
