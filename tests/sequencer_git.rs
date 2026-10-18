//! State formats observed through independently generated Git operations.
use std::sync::atomic::AtomicBool;

use girt::replay::Options;
use girt::{ObjectId, Repository};

#[path = "support/replay_git.rs"]
mod support;
use support::{Fixture, conflict_destination, git, git_extra, head, success, writer};

fn sequencer(repo: &Repository) -> girt::sequencer::State {
    girt::sequencer::State::read(
        repo.git_dir(),
        repo.object_format(),
        girt::sequencer::Limits::default(),
    )
    .unwrap()
    .unwrap()
}

#[test]
fn reads_git_cherry_pick_sequence_and_prepares_skip() {
    let fixture = Fixture::new("sha1");
    let onto = conflict_destination(&fixture);
    assert!(
        !git(
            fixture.root.path(),
            &[
                "cherry-pick",
                &fixture.source.to_string(),
                &fixture.base.to_string()
            ]
        )
        .status
        .success()
    );
    let repo = Repository::open(fixture.root.path()).unwrap();
    let state = sequencer(&repo);
    assert_eq!(state.kind(), girt::sequencer::Kind::CherryPick);
    assert_eq!(state.pending(), Some(fixture.source));
    assert_eq!(state.original_head(), Some(onto));
    assert_eq!(state.todo().unwrap().lines().len(), 2);
    let skip = state.prepare(girt::sequencer::Action::Skip, onto).unwrap();
    assert_eq!(skip.destination, onto);
    let remaining = girt::replay::todo::Todo::parse(
        skip.file_after("sequencer/todo").unwrap(),
        girt::replay::todo::TodoLimits::default(),
    )
    .unwrap();
    assert_eq!(remaining.lines().len(), 1);
    assert!(
        matches!(&remaining.lines()[0].instruction, girt::replay::todo::Instruction::Pick(token) if fixture.base.to_string().as_bytes().starts_with(&token.id))
    );
    assert!(skip.file_after("CHERRY_PICK_HEAD").is_none());
    assert!(repo.git_dir().join("CHERRY_PICK_HEAD").exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn git_continues_girt_created_single_stop() {
    let fixture = Fixture::new("sha1");
    let onto = conflict_destination(&fixture);
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
    let checkout = replay
        .result
        .prepare_checkout(&objects, &girt::merge::MarkerOptions::default())
        .unwrap();
    let baseline = objects.read_commit(onto).unwrap().tree();
    repo.checkout_merge(Some(baseline), &checkout).unwrap();
    let start = girt::sequencer::Recovery::start_single(
        &repo,
        girt::sequencer::Kind::CherryPick,
        fixture.source,
        onto,
        b"source\n",
        girt::sequencer::Limits::default(),
    )
    .unwrap();
    start.publish_metadata(&repo).unwrap();
    std::fs::write(fixture.root.path().join("file"), b"resolved\n").unwrap();
    success(fixture.root.path(), &["add", "file"]);
    let output = git_extra(
        fixture.root.path(),
        &["cherry-pick", "--continue"],
        &[("GIT_EDITOR", "true")],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.objects()
            .unwrap()
            .read_commit(head(fixture.root.path()))
            .unwrap()
            .subject(),
        b"source"
    );
    assert!(!repo.git_dir().join("CHERRY_PICK_HEAD").exists());
}

#[test]
fn git_executes_girt_created_rebase_metadata() {
    let fixture = Fixture::new("sha1");
    let repo = Repository::open(fixture.root.path()).unwrap();
    let todo = format!("pick {} source\n", fixture.source);
    let start = girt::sequencer::Recovery::start_rebase(
        &repo,
        fixture.source,
        fixture.onto,
        None,
        todo.as_bytes(),
        girt::sequencer::Limits::default(),
    )
    .unwrap();
    start.publish_metadata(&repo).unwrap();
    let state = sequencer(&repo);
    assert_eq!(state.kind(), girt::sequencer::Kind::Rebase);
    assert_eq!(state.original_head(), Some(fixture.source));
    let expected = repo
        .objects()
        .unwrap()
        .cherry_pick(
            fixture.source,
            fixture.onto,
            writer(),
            Options::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .commit
        .unwrap()
        .id();
    let output = git_extra(
        fixture.root.path(),
        &["rebase", "--continue"],
        &[("GIT_EDITOR", "true")],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(head(fixture.root.path()), expected);
    assert!(!repo.git_dir().join("rebase-merge").exists());
}

fn publish_head(repo: &Repository, old: ObjectId, new: ObjectId, message: Vec<u8>) {
    let edit = girt::refs::RefEdit::update(
        girt::refs::RefName::HEAD,
        old,
        new,
        girt::refs::Reflog::Append {
            committer: writer(),
            message,
        },
    );
    repo.references().unwrap().transaction(&[edit]).unwrap();
}

#[test]
fn girt_resolves_git_rebase_stop_then_git_finishes() {
    let fixture = Fixture::new("sha1");
    let onto = conflict_destination(&fixture);
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.source.to_string()],
    );
    assert!(
        !git(
            fixture.root.path(),
            &[
                "rebase",
                "--onto",
                &onto.to_string(),
                &fixture.base.to_string()
            ]
        )
        .status
        .success()
    );
    let repo = Repository::open(fixture.root.path()).unwrap();
    let state = sequencer(&repo);
    assert_eq!(state.pending(), Some(fixture.source));
    std::fs::write(fixture.root.path().join("file"), b"resolved\n").unwrap();
    success(fixture.root.path(), &["add", "file"]);
    let tree: ObjectId = std::str::from_utf8(&success(fixture.root.path(), &["write-tree"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let source = repo.objects().unwrap().read_commit(fixture.source).unwrap();
    let commit = girt::Commit::builder(tree, source.to_fields().unwrap().author, source.message())
        .parent(onto)
        .committer(writer())
        .build()
        .unwrap();
    repo.loose_objects().write_commit(&commit).unwrap();
    let recovery = state
        .prepare(girt::sequencer::Action::Continue(commit.id()), onto)
        .unwrap();
    publish_head(&repo, onto, commit.id(), recovery.reflog_for(&commit));
    recovery.publish_metadata(&repo).unwrap();
    let output = git_extra(
        fixture.root.path(),
        &["rebase", "--continue"],
        &[("GIT_EDITOR", "true")],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(head(fixture.root.path()), commit.id());
    let reflog = success(fixture.root.path(), &["reflog", "-2", "--format=%gs"]);
    assert_eq!(reflog, git_resolved_rebase_reflog());
}

#[test]
fn metadata_rejects_changed_state_before_removing_files() {
    let fixture = Fixture::new("sha1");
    let onto = conflict_destination(&fixture);
    assert!(
        !git(
            fixture.root.path(),
            &["cherry-pick", &fixture.source.to_string()]
        )
        .status
        .success()
    );
    let repo = Repository::open(fixture.root.path()).unwrap();
    let recovery = sequencer(&repo)
        .prepare(girt::sequencer::Action::Skip, onto)
        .unwrap();
    std::fs::write(repo.git_dir().join("MERGE_MSG"), b"edited message\n").unwrap();
    assert!(matches!(
        recovery.publish_metadata(&repo),
        Err(girt::sequencer::Error::Changed)
    ));
    assert!(repo.git_dir().join("CHERRY_PICK_HEAD").exists());
}

#[test]
fn girt_aborts_git_rebase_and_preserves_orig_head() {
    let fixture = Fixture::new("sha1");
    let onto = conflict_destination(&fixture);
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.source.to_string()],
    );
    assert!(
        !git(
            fixture.root.path(),
            &[
                "rebase",
                "--onto",
                &onto.to_string(),
                &fixture.base.to_string()
            ]
        )
        .status
        .success()
    );
    let repo = Repository::open(fixture.root.path()).unwrap();
    let recovery = sequencer(&repo)
        .prepare(girt::sequencer::Action::Abort, onto)
        .unwrap();
    assert_eq!(recovery.destination, fixture.source);
    // Explicit discard of the unresolved checkout is separate from sequencer metadata.
    success(
        fixture.root.path(),
        &[
            "restore",
            "--source",
            &fixture.source.to_string(),
            "--staged",
            "--worktree",
            ".",
        ],
    );
    publish_head(&repo, onto, fixture.source, recovery.reflog_message.clone());
    recovery.publish_metadata(&repo).unwrap();
    assert_eq!(head(fixture.root.path()), fixture.source);
    assert!(!repo.git_dir().join("rebase-merge").exists());
    assert!(repo.git_dir().join("ORIG_HEAD").exists());
    assert!(success(fixture.root.path(), &["status", "--porcelain"]).is_empty());
}

#[test]
fn girt_skips_git_single_pick_after_explicit_checkout_restore() {
    let fixture = Fixture::new("sha1");
    let onto = conflict_destination(&fixture);
    assert!(
        !git(
            fixture.root.path(),
            &["cherry-pick", &fixture.source.to_string()]
        )
        .status
        .success()
    );
    let repo = Repository::open(fixture.root.path()).unwrap();
    let recovery = sequencer(&repo)
        .prepare(girt::sequencer::Action::Skip, onto)
        .unwrap();
    success(
        fixture.root.path(),
        &["restore", "--source=HEAD", "--staged", "--worktree", "."],
    );
    recovery.publish_metadata(&repo).unwrap();
    assert_eq!(head(fixture.root.path()), onto);
    assert!(success(fixture.root.path(), &["status", "--porcelain"]).is_empty());
    assert!(!repo.git_dir().join("CHERRY_PICK_HEAD").exists());
}

#[test]
fn unknown_state_file_is_preserved_and_refused() {
    let fixture = Fixture::new("sha1");
    let onto = conflict_destination(&fixture);
    assert!(
        !git(
            fixture.root.path(),
            &[
                "cherry-pick",
                &fixture.source.to_string(),
                &fixture.base.to_string()
            ]
        )
        .status
        .success()
    );
    let repo = Repository::open(fixture.root.path()).unwrap();
    let note = repo.git_dir().join("sequencer/unknown-state");
    std::fs::write(&note, b"retain these bytes\n").unwrap();
    assert!(matches!(
        girt::sequencer::State::read(
            repo.git_dir(),
            repo.object_format(),
            girt::sequencer::Limits::default()
        ),
        Err(girt::sequencer::Error::Unsupported(_))
    ));
    assert_eq!(std::fs::read(note).unwrap(), b"retain these bytes\n");
    assert_eq!(head(fixture.root.path()), onto);
}

#[test]
fn metadata_lock_conflict_leaves_operation_bytes_unchanged() {
    let fixture = Fixture::new("sha1");
    let onto = conflict_destination(&fixture);
    assert!(
        !git(
            fixture.root.path(),
            &["cherry-pick", &fixture.source.to_string()]
        )
        .status
        .success()
    );
    let repo = Repository::open(fixture.root.path()).unwrap();
    let state = sequencer(&repo);
    let before = state.file("CHERRY_PICK_HEAD").unwrap().to_vec();
    let recovery = state.prepare(girt::sequencer::Action::Skip, onto).unwrap();
    let lock = repo.git_dir().join("CHERRY_PICK_HEAD.lock");
    std::fs::write(&lock, b"another writer\n").unwrap();
    assert!(matches!(
        recovery.publish_metadata(&repo),
        Err(girt::sequencer::Error::Io { .. })
    ));
    assert_eq!(
        std::fs::read(repo.git_dir().join("CHERRY_PICK_HEAD")).unwrap(),
        before
    );
    assert_eq!(std::fs::read(lock).unwrap(), b"another writer\n");
}

#[cfg(unix)]
#[test]
fn state_symlink_is_refused_without_reading_its_target() {
    let fixture = Fixture::new("sha1");
    let repo = Repository::open(fixture.root.path()).unwrap();
    let outside = fixture.root.path().join("outside-state");
    std::fs::write(&outside, b"retain\n").unwrap();
    std::os::unix::fs::symlink(&outside, repo.git_dir().join("CHERRY_PICK_HEAD")).unwrap();
    assert!(matches!(
        girt::sequencer::State::read(
            repo.git_dir(),
            repo.object_format(),
            girt::sequencer::Limits::default()
        ),
        Err(girt::sequencer::Error::Unsupported(_))
    ));
    assert_eq!(std::fs::read(outside).unwrap(), b"retain\n");
}

#[rstest::rstest]
#[case::cherry_pick("cherry-pick")]
#[case::revert("revert")]
fn continuation_reflog_matches_git(#[case] command: &str) {
    let fixture = Fixture::new("sha1");
    let onto = conflict_destination(&fixture);
    assert!(
        !git(fixture.root.path(), &[command, &fixture.source.to_string()])
            .status
            .success()
    );
    let repo = Repository::open(fixture.root.path()).unwrap();
    let state = sequencer(&repo);
    std::fs::write(fixture.root.path().join("file"), b"resolved\n").unwrap();
    success(fixture.root.path(), &["add", "file"]);
    let output = git_extra(
        fixture.root.path(),
        &[command, "--continue"],
        &[("GIT_EDITOR", "true")],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let commit = repo
        .objects()
        .unwrap()
        .read_commit(head(fixture.root.path()))
        .unwrap();
    let recovery = state
        .prepare(girt::sequencer::Action::Continue(commit.id()), onto)
        .unwrap();
    let expected = success(fixture.root.path(), &["reflog", "-1", "--format=%gs"]);
    assert_eq!(
        recovery.reflog_for(&commit),
        expected.strip_suffix(b"\n").unwrap()
    );
}

#[test]
fn sequencer_kind_uses_first_parsed_instruction_after_comments() {
    let fixture = Fixture::new("sha1");
    let repo = Repository::open(fixture.root.path()).unwrap();
    let directory = repo.git_dir().join("sequencer");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("head"), format!("{}\n", fixture.onto)).unwrap();
    std::fs::write(
        directory.join("todo"),
        format!("# edited sequence\nrevert {} source\n", fixture.source),
    )
    .unwrap();
    assert_eq!(sequencer(&repo).kind(), girt::sequencer::Kind::Revert);
}

fn git_resolved_rebase_reflog() -> Vec<u8> {
    let fixture = Fixture::new("sha1");
    let onto = conflict_destination(&fixture);
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.source.to_string()],
    );
    assert!(
        !git(
            fixture.root.path(),
            &[
                "rebase",
                "--onto",
                &onto.to_string(),
                &fixture.base.to_string()
            ]
        )
        .status
        .success()
    );
    std::fs::write(fixture.root.path().join("file"), b"resolved\n").unwrap();
    success(fixture.root.path(), &["add", "file"]);
    let output = git_extra(
        fixture.root.path(),
        &["rebase", "--continue"],
        &[("GIT_EDITOR", "true")],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    success(fixture.root.path(), &["reflog", "-2", "--format=%gs"])
}

#[test]
fn git_and_girt_accept_noop_rebase_instructions() {
    let fixture = Fixture::new("sha1");
    let repo = Repository::open(fixture.root.path()).unwrap();
    let start = girt::sequencer::Recovery::start_rebase(
        &repo,
        fixture.source,
        fixture.onto,
        None,
        b"noop\nbreak\n",
        girt::sequencer::Limits::default(),
    )
    .unwrap();
    start.publish_metadata(&repo).unwrap();
    let output = git_extra(
        fixture.root.path(),
        &["rebase", "--continue"],
        &[("GIT_EDITOR", "true")],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state = sequencer(&repo);
    assert!(
        state
            .file("rebase-merge/done")
            .unwrap()
            .starts_with(b"noop\n")
    );
    assert!(state.todo().unwrap().lines().is_empty());
}
