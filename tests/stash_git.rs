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

fn stash_id(path: &std::path::Path) -> ObjectId {
    std::str::from_utf8(&success(path, &["rev-parse", "refs/stash"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
fn install_result(repo: &Repository, result: &girt::merge::TreeMerge) {
    let loose = repo.loose_objects();
    for (_, bytes) in &result.blobs {
        loose.write_blob(bytes).unwrap();
    }
    for tree in &result.trees {
        loose.write_tree(tree).unwrap();
    }
}

#[rstest]
#[case::ordinary(false, false)]
#[case::index(true, false)]
#[case::untracked(false, true)]
#[case::untracked_index(true, true)]
fn apply_working_and_index_trees_match_git(#[case] restore_index: bool, #[case] untracked: bool) {
    let fixture = SavedFixture::new("sha1", untracked);
    let path = fixture.fixture.root.path();
    // Include a staged new file: ordinary apply stages additions but leaves existing paths
    // unstaged.
    std::fs::write(path.join("added"), b"new file\n").unwrap();
    success(path, &["add", "added"]);
    let args = if untracked {
        vec!["stash", "push", "-u"]
    } else {
        vec!["stash", "push"]
    };
    success(path, &args);
    let repo = Repository::open(path).unwrap();
    let objects = repo.objects().unwrap();
    let saved = Snapshot::read(&objects, stash_id(path)).unwrap();
    let input = girt::stash::ApplyInput {
        index: tree_id(path),
        occupied: vec![],
    };
    let mut options = girt::stash::ApplyOptions::default();
    options.restore_index = restore_index;
    let apply = objects
        .apply_stash(&saved, input, options, &AtomicBool::new(false))
        .unwrap();
    assert!(apply.is_clean());
    if restore_index {
        success(path, &["stash", "apply", "--index"]);
    } else {
        success(path, &["stash", "apply"]);
    }
    assert_eq!(
        apply.index.as_ref().unwrap().tree_id().unwrap(),
        tree_id(path)
    );
    success(path, &["add", "-A"]);
    assert_eq!(apply.working().tree.id(), tree_id(path));
    install_result(&repo, &apply.tracked);
    if let Some(result) = &apply.with_untracked {
        install_result(&repo, result);
    }
    repo.objects()
        .unwrap()
        .read_tree(apply.working().tree.id())
        .unwrap();
}

#[test]
fn apply_conflicts_preserve_original_sides_and_saved_entry() {
    let fixture = SavedFixture::new("sha1", false);
    let path = fixture.fixture.root.path();
    success(path, &["stash", "push"]);
    let saved_id = stash_id(path);
    std::fs::write(path.join("file"), b"different\ntwo\nthree\n").unwrap();
    success(path, &["add", "file"]);
    let repo = Repository::open(path).unwrap();
    let objects = repo.objects().unwrap();
    let saved = Snapshot::read(&objects, saved_id).unwrap();
    let input = girt::stash::ApplyInput {
        index: tree_id(path),
        occupied: vec![],
    };
    let apply = objects
        .apply_stash(
            &saved,
            input,
            girt::stash::ApplyOptions::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(!apply.is_clean());
    let conflict = &apply.tracked.conflicts[0];
    assert!(conflict.base.is_some() && conflict.ours.is_some() && conflict.theirs.is_some());
    assert!(!support::git(path, &["stash", "pop"]).status.success());
    assert_eq!(stash_id(path), saved_id);
    let expected = success(path, &["ls-files", "--stage"]);
    let actual = apply.index.as_ref().unwrap();
    for stage in [
        girt::index::Stage::Base,
        girt::index::Stage::Ours,
        girt::index::Stage::Theirs,
    ] {
        let entry = actual
            .entries()
            .iter()
            .find(|entry| entry.stage == stage)
            .unwrap();
        assert!(
            expected
                .windows(entry.id.to_string().len())
                .any(|bytes| bytes == entry.id.to_string().as_bytes())
        );
    }
}

#[test]
fn restore_index_conflict_stops_before_working_merge() {
    let fixture = SavedFixture::new("sha1", false);
    let path = fixture.fixture.root.path();
    success(path, &["stash", "push"]);
    std::fs::write(path.join("file"), b"different\ntwo\nthree\n").unwrap();
    success(path, &["add", "file"]);
    let repo = Repository::open(path).unwrap();
    let objects = repo.objects().unwrap();
    let saved = Snapshot::read(&objects, stash_id(path)).unwrap();
    let input = girt::stash::ApplyInput {
        index: tree_id(path),
        occupied: vec![],
    };
    let mut options = girt::stash::ApplyOptions::default();
    options.restore_index = true;
    let apply = objects
        .apply_stash(&saved, input.clone(), options, &AtomicBool::new(false))
        .unwrap();
    assert!(!apply.is_clean());
    assert!(
        apply
            .index_merge
            .as_ref()
            .is_some_and(|result| !result.is_clean())
    );
    assert!(apply.index.is_none());
    assert_eq!(apply.working().tree.id(), input.index);
    assert!(
        !support::git(path, &["stash", "apply", "--index"])
            .status
            .success()
    );
    assert_eq!(tree_id(path), input.index);
}

#[rstest]
#[case::identical(b"untracked".as_slice())]
#[case::parent(b"untracked/child".as_slice())]
fn untracked_occupied_paths_are_refused_before_publication(#[case] occupied: &[u8]) {
    let fixture = SavedFixture::new("sha1", true);
    let path = fixture.fixture.root.path();
    success(path, &["stash", "push", "-u"]);
    let repo = Repository::open(path).unwrap();
    let objects = repo.objects().unwrap();
    let saved = Snapshot::read(&objects, stash_id(path)).unwrap();
    let input = girt::stash::ApplyInput {
        index: tree_id(path),
        occupied: vec![occupied.to_vec()],
    };
    assert!(matches!(
        objects.apply_stash(
            &saved,
            input,
            girt::stash::ApplyOptions::default(),
            &AtomicBool::new(false)
        ),
        Err(girt::stash::Error::UntrackedCollision(_))
    ));
    if occupied == b"untracked" {
        std::fs::write(path.join("untracked"), b"saved\n").unwrap();
        assert!(!support::git(path, &["stash", "apply"]).status.success());
        assert_eq!(std::fs::read(path.join("untracked")).unwrap(), b"saved\n");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[rstest]
#[case::ordinary(false)]
#[case::index(true)]
fn guarded_apply_then_drop_completes_pop(#[case] restore_index: bool) {
    let fixture = SavedFixture::new("sha1", true);
    let path = fixture.fixture.root.path();
    success(path, &["stash", "push", "-u"]);
    let repo = Repository::open(path).unwrap();
    let history = girt::stash::History::read(&repo, girt::stash::LogLimits::default()).unwrap();
    let objects = repo.objects().unwrap();
    let saved = Snapshot::read(&objects, history.entries().next().unwrap().new).unwrap();
    let baseline = tree_id(path);
    let input = girt::stash::ApplyInput {
        index: baseline,
        occupied: vec![],
    };
    let mut options = girt::stash::ApplyOptions::default();
    options.restore_index = restore_index;
    let apply = objects
        .apply_stash(&saved, input, options, &AtomicBool::new(false))
        .unwrap();
    let plan = apply
        .prepare_checkout(&objects, &girt::merge::MarkerOptions::default())
        .unwrap();
    let report = repo.checkout_merge(Some(baseline), &plan).unwrap();
    assert!(report.index_published);
    assert_eq!(
        repo.read_index(Default::default())
            .unwrap()
            .unwrap()
            .tree_id(),
        apply.index.as_ref().unwrap().tree_id()
    );
    assert_eq!(
        std::fs::read(path.join("file")).unwrap(),
        b"ONE\ntwo\nTHREE\n"
    );
    assert_eq!(std::fs::read(path.join("untracked")).unwrap(), b"saved\n");
    // Drop is separate and follows only a successful working-file/index publication.
    history.drop(0).unwrap().publish(&repo).unwrap();
    assert!(success(path, &["stash", "list"]).is_empty());
    assert_eq!(head(path), fixture.input.head);
}

#[rstest]
#[case::file_to_directory(true)]
#[case::directory_to_file(false)]
fn ordinary_apply_displaces_index_parent_child_clashes(#[case] file_to_directory: bool) {
    let fixture = Fixture::new("sha1");
    let path = fixture.root.path();
    if !file_to_directory {
        success(path, &["rm", "file"]);
        std::fs::create_dir(path.join("file")).unwrap();
        std::fs::write(path.join("file/child"), b"old child\n").unwrap();
        success(path, &["add", "-A"]);
        success(path, &["commit", "-m", "directory base"]);
    }
    success(path, &["rm", "-r", "file"]);
    if file_to_directory {
        std::fs::create_dir(path.join("file")).unwrap();
        std::fs::write(path.join("file/child"), b"new child\n").unwrap();
    } else {
        std::fs::write(path.join("file"), b"replacement file\n").unwrap();
    }
    success(path, &["add", "-A"]);
    success(path, &["stash", "push"]);
    std::fs::write(path.join("other"), b"preserved staged edit\n").unwrap();
    success(path, &["add", "other"]);
    let repo = Repository::open(path).unwrap();
    let objects = repo.objects().unwrap();
    let saved = Snapshot::read(&objects, stash_id(path)).unwrap();
    let input = girt::stash::ApplyInput {
        index: tree_id(path),
        occupied: vec![],
    };
    let apply = objects
        .apply_stash(
            &saved,
            input,
            girt::stash::ApplyOptions::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(apply.is_clean());
    success(path, &["stash", "apply"]);
    assert_eq!(
        apply.index.as_ref().unwrap().tree_id().unwrap(),
        tree_id(path)
    );
    success(path, &["add", "-A"]);
    assert_eq!(apply.working().tree.id(), tree_id(path));
}
