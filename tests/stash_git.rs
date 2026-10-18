//! Stash fixtures authored through isolated Git CLI operations, without upstream source input.
use std::sync::atomic::AtomicBool;

use girt::stash::{SaveInput, SaveMode, SaveOptions, Snapshot};
use girt::{EntryMode, ObjectId, Repository, Signature, Tree, TreeEntry};
use rstest::rstest;

#[path = "support/replay_git.rs"]
#[allow(dead_code)]
mod support;
use support::{Fixture, head, success, writer};

struct SavedFixture {
    fixture: Fixture,
    input: SaveInput,
}
impl SavedFixture {
    fn new(format: &str, untracked: bool) -> Self {
        let fixture = Fixture::new(format);
        let path = fixture.root.path();
        success(path, &["checkout", "-B", "topic"]);
        std::fs::write(path.join("file"), b"ONE\ntwo\nthree\n").unwrap();
        success(path, &["add", "file"]);
        let index = tree_id(path);
        std::fs::write(path.join("file"), b"ONE\ntwo\nTHREE\n").unwrap();
        success(path, &["add", "file"]);
        let worktree = tree_id(path);
        success(path, &["read-tree", &index.to_string()]);
        let untracked = untracked.then(|| {
            std::fs::write(path.join("untracked"), b"saved\n").unwrap();
            let repo = Repository::open(path).unwrap();
            let loose = repo.loose_objects();
            let blob = loose.write_blob(b"saved\n").unwrap();
            loose
                .write_tree(
                    &Tree::new(
                        repo.object_format(),
                        vec![TreeEntry {
                            mode: EntryMode::Blob,
                            name: b"untracked".to_vec(),
                            id: blob,
                        }],
                    )
                    .unwrap(),
                )
                .unwrap()
        });
        let input = SaveInput {
            head: head(path),
            index,
            worktree,
            untracked,
        };
        Self { fixture, input }
    }
    fn options(&self, mode: SaveMode, custom: bool) -> SaveOptions {
        let mut options = SaveOptions::default();
        options.branch = b"topic".to_vec();
        options.mode = mode;
        options.message = custom.then(|| b"saved task".to_vec());
        options
    }
}
fn tree_id(path: &std::path::Path) -> ObjectId {
    std::str::from_utf8(&success(path, &["write-tree"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
fn author() -> Signature {
    Signature {
        name: b"Original Author".to_vec(),
        email: b"author@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    }
}

#[rstest]
#[case::sha1("sha1", SaveMode::All, false, false)]
#[case::sha256("sha256", SaveMode::All, false, false)]
#[case::untracked("sha1", SaveMode::All, true, false)]
#[case::untracked_sha256("sha256", SaveMode::All, true, false)]
#[case::keep_index("sha1", SaveMode::KeepIndex, true, false)]
#[case::staged("sha1", SaveMode::Staged, false, false)]
#[case::custom("sha1", SaveMode::All, false, true)]
fn saved_commits_and_cleanup_match_git(
    #[case] format: &str,
    #[case] mode: SaveMode,
    #[case] untracked: bool,
    #[case] custom: bool,
) {
    let fixture = SavedFixture::new(format, untracked);
    let path = fixture.fixture.root.path();
    let repo = Repository::open(path).unwrap();
    let save = repo
        .objects()
        .unwrap()
        .create_stash(
            fixture.input,
            author(),
            writer(),
            fixture.options(mode, custom),
            &AtomicBool::new(false),
        )
        .unwrap()
        .unwrap();
    assert!(save.cleanup.is_clean());
    assert_eq!(head(path), fixture.input.head);
    assert!(
        support::git(path, &["rev-parse", "--verify", "refs/stash"])
            .status
            .code()
            .is_some_and(|code| code != 0)
    );
    let mut args = vec!["stash", "push"];
    if untracked {
        args.push("--include-untracked");
    }
    match mode {
        SaveMode::KeepIndex => args.push("--keep-index"),
        SaveMode::Staged => args.push("--staged"),
        _ => (),
    }
    if custom {
        args.extend(["-m", "saved task"]);
    }
    success(path, &args);
    let id: ObjectId = std::str::from_utf8(&success(path, &["rev-parse", "refs/stash"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(save.snapshot.id(), id);
    let snapshot = Snapshot::read(&repo.objects().unwrap(), id).unwrap();
    assert_eq!(snapshot.index.id(), save.snapshot.index.id());
    assert_eq!(
        snapshot.untracked.as_ref().map(|c| c.id()),
        save.snapshot.untracked.as_ref().map(|c| c.id())
    );
    assert_eq!(save.index_after, tree_id(path));
    success(path, &["add", "-u"]);
    assert_eq!(save.cleanup.tree.id(), tree_id(path));
    assert!(!path.join("untracked").exists());
}

#[test]
fn staged_cleanup_retains_overlap_without_publishing_saved_objects() {
    let fixture = SavedFixture::new("sha1", false);
    let path = fixture.fixture.root.path();
    std::fs::write(path.join("file"), b"overlap\ntwo\nthree\n").unwrap();
    success(path, &["add", "-u"]);
    let mut input = fixture.input;
    input.worktree = tree_id(path);
    success(path, &["read-tree", &input.index.to_string()]);
    let repo = Repository::open(path).unwrap();
    let save = repo
        .objects()
        .unwrap()
        .create_stash(
            input,
            author(),
            writer(),
            fixture.options(SaveMode::Staged, false),
            &AtomicBool::new(false),
        )
        .unwrap()
        .unwrap();
    assert!(!save.cleanup.is_clean());
    let output = support::git(path, &["stash", "push", "--staged"]);
    assert!(!output.status.success());
    let snapshot = Snapshot::read(&repo.objects().unwrap(), save.snapshot.id()).unwrap();
    assert_eq!(snapshot.id(), save.snapshot.id());
    assert_eq!(
        std::fs::read(path.join("file")).unwrap(),
        b"overlap\ntwo\nthree\n"
    );
}

#[test]
fn clean_selection_creates_no_stash_and_malformed_shape_is_refused() {
    let fixture = Fixture::new("sha1");
    let repo = Repository::open(fixture.root.path()).unwrap();
    let objects = repo.objects().unwrap();
    let tree = objects.read_commit(fixture.onto).unwrap().tree();
    let input = SaveInput {
        head: fixture.onto,
        index: tree,
        worktree: tree,
        untracked: None,
    };
    assert!(
        objects
            .create_stash(
                input,
                author(),
                writer(),
                SaveOptions::default(),
                &AtomicBool::new(false)
            )
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        Snapshot::read(&objects, fixture.onto),
        Err(girt::stash::Error::Shape)
    ));
}
