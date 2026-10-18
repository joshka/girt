//! Replay fixtures generated independently through the Git executable.
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::AtomicBool;

use girt::replay::{EmptyKind, EmptyPolicy, Options, Status};
use girt::{ObjectId, Repository, Signature};
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
        .env("GIT_AUTHOR_NAME", "Original Author")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Replay Writer")
        .env("GIT_COMMITTER_EMAIL", "writer@example.com")
        .env("GIT_COMMITTER_DATE", "1700000100 +0000")
        .output()
        .unwrap()
}
fn success(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = git(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn head(root: &Path) -> ObjectId {
    std::str::from_utf8(&success(root, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
fn writer() -> Signature {
    Signature {
        name: b"Replay Writer".to_vec(),
        email: b"writer@example.com".to_vec(),
        seconds: 1_700_000_100,
        offset_minutes: 0,
    }
}
struct Fixture {
    root: tempfile::TempDir,
    base: ObjectId,
    source: ObjectId,
    onto: ObjectId,
}
impl Fixture {
    fn new(format: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        success(
            path,
            &[
                "init",
                "--template=",
                &format!("--object-format={format}"),
                ".",
            ],
        );
        std::fs::write(path.join("file"), b"one\ntwo\nthree\n").unwrap();
        success(path, &["add", "."]);
        success(path, &["commit", "-m", "base"]);
        let base = head(path);
        std::fs::write(path.join("file"), b"ONE\ntwo\nthree\n").unwrap();
        success(path, &["commit", "-am", "source"]);
        let source = head(path);
        success(path, &["checkout", "--detach", &base.to_string()]);
        std::fs::write(path.join("other"), b"destination\n").unwrap();
        success(path, &["add", "."]);
        success(path, &["commit", "-m", "destination"]);
        let onto = head(path);
        Self {
            root,
            base,
            source,
            onto,
        }
    }
}

#[rstest]
#[case::sha1("sha1", false)]
#[case::sha256("sha256", false)]
#[case::origin("sha1", true)]
fn cherry_pick_matches_git_commit(#[case] format: &str, #[case] origin: bool) {
    let fixture = Fixture::new(format);
    let repo = Repository::open(fixture.root.path()).unwrap();
    let mut options = Options::default();
    options.record_origin = origin;
    let replay = repo
        .objects()
        .unwrap()
        .cherry_pick(
            fixture.source,
            fixture.onto,
            writer(),
            options,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(replay.status, Status::Ready);
    let flag = if origin { "-x" } else { "--no-edit" };
    success(
        fixture.root.path(),
        &["cherry-pick", flag, &fixture.source.to_string()],
    );
    assert_eq!(replay.commit.unwrap().id(), head(fixture.root.path()));
}

#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn revert_matches_git_commit(#[case] format: &str) {
    let fixture = Fixture::new(format);
    let repo = Repository::open(fixture.root.path()).unwrap();
    let replay = repo
        .objects()
        .unwrap()
        .revert(
            fixture.source,
            fixture.source,
            writer(),
            Options::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.source.to_string()],
    );
    // Revert author is the operation identity, rather than the original source author.
    let output = {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command
            .current_dir(fixture.root.path())
            .args(["revert", "--no-edit", &fixture.source.to_string()])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                fixture.root.path().join("absent-config"),
            )
            .env("GIT_AUTHOR_NAME", "Replay Writer")
            .env("GIT_AUTHOR_EMAIL", "writer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000100 +0000")
            .env("GIT_COMMITTER_NAME", "Replay Writer")
            .env("GIT_COMMITTER_EMAIL", "writer@example.com")
            .env("GIT_COMMITTER_DATE", "1700000100 +0000")
            .output()
            .unwrap()
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(replay.commit.unwrap().id(), head(fixture.root.path()));
}

#[rstest]
#[case::stop(EmptyPolicy::Stop, Status::Empty(EmptyKind::Redundant), false)]
#[case::keep(EmptyPolicy::Keep, Status::KeptEmpty(EmptyKind::Redundant), true)]
#[case::skip(EmptyPolicy::Skip, Status::Skipped(EmptyKind::Redundant), false)]
fn redundant_policy_is_explicit(
    #[case] policy: EmptyPolicy,
    #[case] status: Status,
    #[case] commit: bool,
) {
    let fixture = Fixture::new("sha1");
    let repo = Repository::open(fixture.root.path()).unwrap();
    let mut options = Options::default();
    options.redundant = policy;
    let replay = repo
        .objects()
        .unwrap()
        .cherry_pick(
            fixture.source,
            fixture.source,
            writer(),
            options,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(replay.status, status);
    assert_eq!(replay.commit.is_some(), commit);
    assert_eq!(head(fixture.root.path()), fixture.onto);
    assert_eq!(
        repo.objects()
            .unwrap()
            .read_commit(fixture.base)
            .unwrap()
            .subject(),
        b"base"
    );
}

#[test]
fn conflict_retains_original_sides_without_publication() {
    let fixture = Fixture::new("sha1");
    std::fs::write(fixture.root.path().join("file"), b"OTHER\ntwo\nthree\n").unwrap();
    success(fixture.root.path(), &["commit", "-am", "overlap"]);
    let onto = head(fixture.root.path());
    let repo = Repository::open(fixture.root.path()).unwrap();
    let objects = repo.objects().unwrap();
    let replay = objects
        .cherry_pick(
            fixture.source,
            onto,
            writer(),
            Options::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(replay.status, Status::Conflicted);
    assert!(replay.commit.is_none());
    assert_eq!(replay.result.conflicts.len(), 1);
    let conflict = &replay.result.conflicts[0];
    assert_eq!(
        objects.read_blob(conflict.base.unwrap().id).unwrap(),
        b"one\ntwo\nthree\n"
    );
    assert_eq!(
        objects.read_blob(conflict.ours.unwrap().id).unwrap(),
        b"OTHER\ntwo\nthree\n"
    );
    assert_eq!(
        objects.read_blob(conflict.theirs.unwrap().id).unwrap(),
        b"ONE\ntwo\nthree\n"
    );
    assert_eq!(head(fixture.root.path()), onto);
    assert!(!fixture.root.path().join(".git/CHERRY_PICK_HEAD").exists());
    assert!(
        !git(
            fixture.root.path(),
            &["cherry-pick", &fixture.source.to_string()]
        )
        .status
        .success()
    );
}

#[test]
fn root_replay_uses_unstored_empty_base() {
    let fixture = Fixture::new("sha1");
    let repo = Repository::open(fixture.root.path()).unwrap();
    let replay = repo
        .objects()
        .unwrap()
        .revert(
            fixture.base,
            fixture.base,
            writer(),
            Options::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(replay.status, Status::Ready);
    assert!(replay.result.tree.entries().is_empty());
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.base.to_string()],
    );
    // Compare tree identity; the separate revert fixture verifies operation identities.
    success(
        fixture.root.path(),
        &["revert", "--no-edit", &fixture.base.to_string()],
    );
    let git_commit = repo
        .objects()
        .unwrap()
        .read_commit(head(fixture.root.path()))
        .unwrap();
    assert_eq!(replay.result.tree.id(), git_commit.tree());
}

#[test]
fn merge_source_requires_valid_mainline() {
    let fixture = Fixture::new("sha1");
    success(
        fixture.root.path(),
        &[
            "merge",
            "--no-ff",
            "-m",
            "merge source",
            &fixture.source.to_string(),
        ],
    );
    let source = head(fixture.root.path());
    let repo = Repository::open(fixture.root.path()).unwrap();
    let objects = repo.objects().unwrap();
    assert!(matches!(
        objects.cherry_pick(
            source,
            fixture.base,
            writer(),
            Options::default(),
            &AtomicBool::new(false)
        ),
        Err(girt::replay::Error::MainlineRequired)
    ));
    let mut options = Options::default();
    options.mainline = std::num::NonZeroUsize::new(3);
    assert!(matches!(
        objects.cherry_pick(
            source,
            fixture.base,
            writer(),
            options,
            &AtomicBool::new(false)
        ),
        Err(girt::replay::Error::InvalidMainline)
    ));
    options.mainline = std::num::NonZeroUsize::new(1);
    let replay = objects
        .cherry_pick(
            source,
            fixture.onto,
            writer(),
            options,
            &AtomicBool::new(false),
        )
        .unwrap();
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.onto.to_string()],
    );
    success(
        fixture.root.path(),
        &["cherry-pick", "-m", "1", &source.to_string()],
    );
    assert_eq!(replay.commit.unwrap().id(), head(fixture.root.path()));
}

#[test]
fn originally_empty_is_separate_from_redundant() {
    let fixture = Fixture::new("sha1");
    success(
        fixture.root.path(),
        &["commit", "--allow-empty", "-m", "empty source"],
    );
    let source = head(fixture.root.path());
    let repo = Repository::open(fixture.root.path()).unwrap();
    let objects = repo.objects().unwrap();
    let mut options = Options::default();
    options.originally_empty = EmptyPolicy::Keep;
    let replay = objects
        .cherry_pick(
            source,
            fixture.base,
            writer(),
            options,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(replay.status, Status::KeptEmpty(EmptyKind::Original));
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.base.to_string()],
    );
    success(
        fixture.root.path(),
        &["cherry-pick", "--allow-empty", &source.to_string()],
    );
    assert_eq!(replay.commit.unwrap().id(), head(fixture.root.path()));
}

#[test]
fn reverting_a_merge_names_the_selected_parent() {
    let fixture = Fixture::new("sha1");
    success(
        fixture.root.path(),
        &[
            "merge",
            "--no-ff",
            "-m",
            "merge source",
            &fixture.source.to_string(),
        ],
    );
    let source = head(fixture.root.path());
    let repo = Repository::open(fixture.root.path()).unwrap();
    let mut options = Options::default();
    options.mainline = std::num::NonZeroUsize::new(1);
    let replay = repo
        .objects()
        .unwrap()
        .revert(source, source, writer(), options, &AtomicBool::new(false))
        .unwrap();
    success(
        fixture.root.path(),
        &["revert", "--no-edit", "-m", "1", &source.to_string()],
    );
    let expected = repo
        .objects()
        .unwrap()
        .read_commit(head(fixture.root.path()))
        .unwrap();
    assert_eq!(
        replay.commit.as_ref().unwrap().message(),
        expected.message()
    );
    assert_eq!(replay.result.tree.id(), expected.tree());
}
