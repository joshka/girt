//! Replay fixtures generated independently through the Git executable.
use std::sync::atomic::AtomicBool;

use girt::Repository;
use girt::replay::{EmptyKind, EmptyPolicy, Options, Status};
use rstest::rstest;

#[path = "support/replay_git.rs"]
mod support;
use support::{Fixture, conflict_destination, git, git_extra, head, success, writer};

#[rstest]
#[case::sha1("sha1", false, "--no-edit")]
#[case::sha256("sha256", false, "--no-edit")]
#[case::origin("sha1", true, "-x")]
fn cherry_pick_matches_git_commit(#[case] format: &str, #[case] origin: bool, #[case] flag: &str) {
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
    let output = git_extra(
        fixture.root.path(),
        &["revert", "--no-edit", &fixture.source.to_string()],
        &[
            ("GIT_AUTHOR_NAME", "Replay Writer"),
            ("GIT_AUTHOR_EMAIL", "writer@example.com"),
            ("GIT_AUTHOR_DATE", "1700000100 +0000"),
        ],
    );
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
fn root_replay_retains_empty_tree_for_explicit_installation() {
    let fixture = Fixture::new("sha1");
    let repo = Repository::open(fixture.root.path()).unwrap();
    let empty = girt::Tree::new(repo.object_format(), vec![]).unwrap().id();
    let existed = repo.objects().unwrap().read_tree(empty).is_ok();
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
    assert_eq!(repo.objects().unwrap().read_tree(empty).is_ok(), existed);
    install_merge_objects(&repo, &replay.result);
    assert!(
        repo.objects()
            .unwrap()
            .read_tree(replay.result.tree.id())
            .unwrap()
            .entries()
            .is_empty()
    );
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

#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn rebase_sequence_matches_git_without_installing_intermediate_objects(#[case] format: &str) {
    let fixture = Fixture::new(format);
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.source.to_string()],
    );
    std::fs::write(fixture.root.path().join("file"), b"ONE\ntwo\nTHREE\n").unwrap();
    success(fixture.root.path(), &["commit", "-am", "second"]);
    let second = head(fixture.root.path());
    let repo = Repository::open(fixture.root.path()).unwrap();
    let replay = repo
        .objects()
        .unwrap()
        .rebase(
            &[fixture.source, second],
            fixture.onto,
            writer(),
            girt::replay::RebaseOptions::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(replay.stopped.is_none());
    assert_eq!(replay.completed.len(), 2);
    assert_eq!(head(fixture.root.path()), second);
    assert!(
        repo.objects()
            .unwrap()
            .read_commit(replay.completed[0].destination)
            .is_err()
    );
    assert!(repo.objects().unwrap().read_commit(replay.tip).is_err());
    success(
        fixture.root.path(),
        &[
            "rebase",
            "--onto",
            &fixture.onto.to_string(),
            &fixture.base.to_string(),
        ],
    );
    assert_eq!(replay.tip, head(fixture.root.path()));
    let loose = repo.loose_objects();
    install_rebase(&replay, &loose);
    assert_eq!(
        repo.objects()
            .unwrap()
            .read_commit(replay.tip)
            .unwrap()
            .parents(),
        &[replay.completed[0].destination]
    );
}

fn install_rebase(rebase: &girt::replay::Rebase, loose: &girt::LooseObjects) {
    for step in &rebase.completed {
        for (_, bytes) in &step.replay.result.blobs {
            loose.write_blob(bytes).unwrap();
        }
        for tree in &step.replay.result.trees {
            loose.write_tree(tree).unwrap();
        }
        if let Some(commit) = &step.replay.commit {
            loose.write_commit(commit).unwrap();
        }
    }
}

#[test]
fn rebase_stops_at_first_conflict_with_completed_progress() {
    let fixture = Fixture::new("sha1");
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.source.to_string()],
    );
    std::fs::write(fixture.root.path().join("other"), b"conflicting source\n").unwrap();
    success(fixture.root.path(), &["add", "."]);
    success(fixture.root.path(), &["commit", "-m", "conflicting second"]);
    let second = head(fixture.root.path());
    let repo = Repository::open(fixture.root.path()).unwrap();
    let replay = repo
        .objects()
        .unwrap()
        .rebase(
            &[fixture.source, second, fixture.base],
            fixture.onto,
            writer(),
            girt::replay::RebaseOptions::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(replay.completed.len(), 1);
    assert_eq!(replay.stopped.as_ref().unwrap().source, second);
    assert_eq!(replay.stopped.as_ref().unwrap().status, Status::Conflicted);
    assert_eq!(replay.tip, replay.completed[0].destination);
    assert_eq!(head(fixture.root.path()), second);
}

#[test]
fn rebase_skips_redundant_inputs_and_bounds_the_sequence() {
    let fixture = Fixture::new("sha1");
    let repo = Repository::open(fixture.root.path()).unwrap();
    let objects = repo.objects().unwrap();
    let mut options = girt::replay::RebaseOptions::default();
    options.replay.redundant = EmptyPolicy::Skip;
    let replay = objects
        .rebase(
            &[fixture.source],
            fixture.source,
            writer(),
            options,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(replay.tip, fixture.source);
    assert_eq!(
        replay.completed[0].replay.status,
        Status::Skipped(EmptyKind::Redundant)
    );
    options.max_commits = 0;
    assert!(matches!(
        objects.rebase(
            &[fixture.source],
            fixture.onto,
            writer(),
            options,
            &AtomicBool::new(false)
        ),
        Err(girt::replay::RebaseError::Limit)
    ));
    assert!(matches!(
        objects.rebase(
            &[fixture.source],
            fixture.onto,
            writer(),
            girt::replay::RebaseOptions::default(),
            &AtomicBool::new(true)
        ),
        Err(girt::replay::RebaseError::Cancelled)
    ));
}

struct Hooks {
    messages: usize,
    commands: Vec<Vec<u8>>,
}
impl girt::replay::TodoHooks for Hooks {
    fn message(
        &mut self,
        _: &girt::replay::todo::Instruction,
        proposed: &[u8],
    ) -> Result<Vec<u8>, girt::replay::HookError> {
        self.messages += 1;
        Ok(proposed.to_vec())
    }
    fn exec(&mut self, command: &[u8], _: &girt::Commit) -> Result<(), girt::replay::HookError> {
        self.commands.push(command.to_vec());
        Ok(())
    }
}

#[test]
fn todo_executes_fixup_and_plans_updates_without_publication() {
    let fixture = Fixture::new("sha1");
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.source.to_string()],
    );
    std::fs::write(fixture.root.path().join("file"), b"ONE\ntwo\nTHREE\n").unwrap();
    success(fixture.root.path(), &["commit", "-am", "second"]);
    let second = head(fixture.root.path());
    let bytes = format!(
        "pick {} source\nupdate-ref refs/heads/side\nfixup {} second\nexec check proposed tip\n",
        fixture.source, second
    );
    let todo = girt::replay::todo::Todo::parse(
        bytes.as_bytes(),
        girt::replay::todo::TodoLimits::default(),
    )
    .unwrap();
    let repo = Repository::open(fixture.root.path()).unwrap();
    let mut hooks = Hooks {
        messages: 0,
        commands: vec![],
    };
    let run = repo
        .objects()
        .unwrap()
        .execute_todo(
            &todo,
            fixture.onto,
            writer(),
            Options::default(),
            &AtomicBool::new(false),
            &mut hooks,
        )
        .unwrap();
    assert_eq!(run.tip.parents(), &[fixture.onto]);
    assert_eq!(run.tip.message(), b"source\n");
    assert_eq!(run.rewritten[&fixture.source], run.tip.id());
    assert_eq!(run.rewritten[&second], run.tip.id());
    assert_eq!(
        run.updates[b"refs/heads/side".as_slice()],
        run.commits[0].id()
    );
    assert_eq!(hooks.commands, vec![b"check proposed tip".to_vec()]);
    assert_eq!(head(fixture.root.path()), second);
    let observed = std::collections::BTreeMap::from([
        (b"refs/heads/side".to_vec(), fixture.source),
        (b"refs/heads/current".to_vec(), second),
    ]);
    let excluded = std::collections::BTreeSet::from([b"refs/heads/current".to_vec()]);
    assert_eq!(
        run.plan_updates(&observed, &excluded).unwrap(),
        std::collections::BTreeMap::from([(b"refs/heads/side".to_vec(), run.tip.id())])
    );
}

#[rstest]
#[case::merge("merge -C abcd side", true)]
#[case::reset("reset side", false)]
fn topology_is_refused_before_external_hooks(#[case] command: &str, #[case] merge: bool) {
    let fixture = Fixture::new("sha1");
    let todo = girt::replay::todo::Todo::parse(
        format!("exec must not run\n{command}\n").as_bytes(),
        girt::replay::todo::TodoLimits::default(),
    )
    .unwrap();
    let repo = Repository::open(fixture.root.path()).unwrap();
    let mut hooks = Hooks {
        messages: 0,
        commands: vec![],
    };
    let error = repo
        .objects()
        .unwrap()
        .execute_todo(
            &todo,
            fixture.onto,
            writer(),
            Options::default(),
            &AtomicBool::new(false),
            &mut hooks,
        )
        .unwrap_err();
    assert_eq!(
        matches!(error, girt::replay::TodoExecutionError::MergeUnsupported),
        merge
    );
    assert!(hooks.commands.is_empty());
    assert_eq!(head(fixture.root.path()), fixture.onto);
}

#[test]
fn autosquash_moves_fixup_after_target() {
    let fixture = Fixture::new("sha1");
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.source.to_string()],
    );
    success(
        fixture.root.path(),
        &["commit", "--allow-empty", "-m", "unrelated"],
    );
    let unrelated = head(fixture.root.path());
    success(
        fixture.root.path(),
        &["commit", "--allow-empty", "-m", "fixup! source"],
    );
    let fixup = head(fixture.root.path());
    let repo = Repository::open(fixture.root.path()).unwrap();
    let todo = repo
        .objects()
        .unwrap()
        .autosquash(&[fixture.source, unrelated, fixup], 10)
        .unwrap();
    assert!(
        matches!(&todo.lines()[0].instruction, girt::replay::todo::Instruction::Pick(token) if token.id == fixture.source.to_string().as_bytes())
    );
    assert!(
        matches!(&todo.lines()[1].instruction, girt::replay::todo::Instruction::Fixup(token, _) if token.id == fixup.to_string().as_bytes())
    );
    assert!(
        matches!(&todo.lines()[2].instruction, girt::replay::todo::Instruction::Pick(token) if token.id == unrelated.to_string().as_bytes())
    );
}

#[rstest]
#[case::single(false)]
#[case::duplicate(true)]
fn todo_fixup_matches_git_interactive_rebase(#[case] duplicate_update: bool) {
    let fixture = Fixture::new("sha1");
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.source.to_string()],
    );
    std::fs::write(fixture.root.path().join("file"), b"ONE\ntwo\nTHREE\n").unwrap();
    success(fixture.root.path(), &["commit", "-am", "second"]);
    let second = head(fixture.root.path());
    success(
        fixture.root.path(),
        &["branch", "side", &fixture.source.to_string()],
    );
    let mut plan = format!(
        "pick {} source\nupdate-ref refs/heads/side\nfixup {} second\n",
        fixture.source, second
    );
    if duplicate_update {
        plan.push_str("update-ref refs/heads/side\n");
    }
    let todo =
        girt::replay::todo::Todo::parse(plan.as_bytes(), girt::replay::todo::TodoLimits::default())
            .unwrap();
    let repo = Repository::open(fixture.root.path()).unwrap();
    let mut hooks = Hooks {
        messages: 0,
        commands: vec![],
    };
    let run = repo
        .objects()
        .unwrap()
        .execute_todo(
            &todo,
            fixture.onto,
            writer(),
            Options::default(),
            &AtomicBool::new(false),
            &mut hooks,
        )
        .unwrap();
    // Git runs this independently authored editor through its normal shell interface.
    std::fs::write(fixture.root.path().join(".git/replay-todo"), plan).unwrap();
    let editor = "cat .git/replay-todo >";
    let output = git_extra(
        fixture.root.path(),
        &[
            "rebase",
            "-i",
            "--onto",
            &fixture.onto.to_string(),
            &fixture.base.to_string(),
        ],
        &[("GIT_SEQUENCE_EDITOR", editor), ("GIT_EDITOR", "true")],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(run.tip.id(), head(fixture.root.path()));
    let actual = success(fixture.root.path(), &["rev-parse", "refs/heads/side"]);
    assert_eq!(
        run.updates[b"refs/heads/side".as_slice()]
            .to_string()
            .as_bytes(),
        actual.trim_ascii()
    );
}

struct ContentProbe {
    calls: usize,
}
impl girt::merge::TreeDriver for ContentProbe {
    fn merge(
        &mut self,
        path: &[u8],
        _: &[u8],
        _: &[u8],
        _: &[u8],
        _: girt::merge::ContentOptions,
        _: &AtomicBool,
    ) -> Result<girt::merge::ContentMerge, Box<dyn std::error::Error + Send + Sync>> {
        assert_eq!(path, b"file");
        self.calls += 1;
        Ok(girt::merge::ContentMerge::resolved(
            b"custom resolution\n".to_vec(),
        ))
    }
}

#[test]
fn replay_uses_explicit_path_driver_without_publication() {
    let fixture = Fixture::new("sha1");
    let onto = conflict_destination(&fixture);
    let repo = Repository::open(fixture.root.path()).unwrap();
    let mut driver = ContentProbe { calls: 0 };
    let replay = repo
        .objects()
        .unwrap()
        .cherry_pick_with_driver(
            fixture.source,
            onto,
            writer(),
            Options::default(),
            &AtomicBool::new(false),
            &mut driver,
        )
        .unwrap();
    assert_eq!(driver.calls, 1);
    assert_eq!(replay.status, Status::Ready);
    assert!(
        replay
            .result
            .blobs
            .iter()
            .any(|(_, bytes)| bytes == b"custom resolution\n")
    );
    assert_eq!(head(fixture.root.path()), onto);
}

#[rstest]
#[case::no_final_lf(b"subject")]
#[case::trailer(b"subject\n\nSigned-off-by: Original Author <author@example.com>\n")]
#[case::review(b"subject\n\nReviewed-by: Reader <reader@example.com>\n")]
#[case::body_trailer(b"subject\n\nbody\nSigned-off-by: Original Author <author@example.com>\n")]
#[case::body_colon(b"subject\n\nbody\nKey: value\n")]
fn provenance_spacing_matches_git(#[case] message: &[u8]) {
    let fixture = Fixture::new("sha1");
    let repo = Repository::open(fixture.root.path()).unwrap();
    let mut fields = repo
        .objects()
        .unwrap()
        .read_commit(fixture.source)
        .unwrap()
        .to_fields()
        .unwrap();
    fields.message = message.to_vec();
    let source = repo
        .loose_objects()
        .write_commit(&girt::Commit::new(fields).unwrap())
        .unwrap();
    let mut options = Options::default();
    options.record_origin = true;
    let replay = repo
        .objects()
        .unwrap()
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
        &["cherry-pick", "-x", &source.to_string()],
    );
    assert_eq!(replay.commit.unwrap().id(), head(fixture.root.path()));
}

#[test]
fn autosquash_amend_message_matches_git() {
    let fixture = Fixture::new("sha1");
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.source.to_string()],
    );
    std::fs::write(fixture.root.path().join("file"), b"ONE\ntwo\nTHREE\n").unwrap();
    success(
        fixture.root.path(),
        &[
            "commit",
            "-a",
            &format!("--fixup=amend:{}", fixture.source),
            "--no-edit",
        ],
    );
    let amend = head(fixture.root.path());
    let repo = Repository::open(fixture.root.path()).unwrap();
    let todo = repo
        .objects()
        .unwrap()
        .autosquash(&[fixture.source, amend], 10)
        .unwrap();
    let mut hooks = Hooks {
        messages: 0,
        commands: vec![],
    };
    let run = repo
        .objects()
        .unwrap()
        .execute_todo(
            &todo,
            fixture.onto,
            writer(),
            Options::default(),
            &AtomicBool::new(false),
            &mut hooks,
        )
        .unwrap();
    let output = git_extra(
        fixture.root.path(),
        &[
            "rebase",
            "-i",
            "--autosquash",
            "--onto",
            &fixture.onto.to_string(),
            &fixture.base.to_string(),
        ],
        &[("GIT_SEQUENCE_EDITOR", "true"), ("GIT_EDITOR", "true")],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(run.tip.id(), head(fixture.root.path()));
}

fn install_merge_objects(repo: &Repository, result: &girt::merge::TreeMerge) {
    let loose = repo.loose_objects();
    for (_, bytes) in &result.blobs {
        loose.write_blob(bytes).unwrap();
    }
    for tree in &result.trees {
        loose.write_tree(tree).unwrap();
    }
}

#[test]
fn root_cherry_pick_matches_git() {
    let fixture = Fixture::new("sha1");
    let repo = Repository::open(fixture.root.path()).unwrap();
    let empty = repo
        .loose_objects()
        .write_tree(&girt::Tree::new(repo.object_format(), vec![]).unwrap())
        .unwrap();
    let destination = repo
        .loose_objects()
        .write_commit(
            &girt::Commit::builder(empty, writer(), b"empty destination\n")
                .build()
                .unwrap(),
        )
        .unwrap();
    let replay = repo
        .objects()
        .unwrap()
        .cherry_pick(
            fixture.base,
            destination,
            writer(),
            Options::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    success(
        fixture.root.path(),
        &["checkout", "--detach", &destination.to_string()],
    );
    success(
        fixture.root.path(),
        &["cherry-pick", &fixture.base.to_string()],
    );
    assert_eq!(replay.commit.unwrap().id(), head(fixture.root.path()));
}

#[test]
fn reverting_a_revert_matches_git_reapply_title() {
    let fixture = Fixture::new("sha1");
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.source.to_string()],
    );
    success(
        fixture.root.path(),
        &["revert", "--no-edit", &fixture.source.to_string()],
    );
    let source = head(fixture.root.path());
    let repo = Repository::open(fixture.root.path()).unwrap();
    let replay = repo
        .objects()
        .unwrap()
        .revert(
            source,
            source,
            writer(),
            Options::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let output = git_extra(
        fixture.root.path(),
        &["revert", "--no-edit", &source.to_string()],
        &[
            ("GIT_AUTHOR_NAME", "Replay Writer"),
            ("GIT_AUTHOR_EMAIL", "writer@example.com"),
            ("GIT_AUTHOR_DATE", "1700000100 +0000"),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(replay.commit.unwrap().id(), head(fixture.root.path()));
}

#[test]
fn caller_can_choose_a_distinct_revert_author() {
    let fixture = Fixture::new("sha1");
    let repo = Repository::open(fixture.root.path()).unwrap();
    let source = repo.objects().unwrap().read_commit(fixture.source).unwrap();
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
    let mut fields = replay.commit.unwrap().to_fields().unwrap();
    fields.author = source.to_fields().unwrap().author;
    let commit = girt::Commit::new(fields).unwrap();
    success(
        fixture.root.path(),
        &["checkout", "--detach", &fixture.source.to_string()],
    );
    success(
        fixture.root.path(),
        &["revert", "--no-edit", &fixture.source.to_string()],
    );
    assert_eq!(commit.id(), head(fixture.root.path()));
}
