//! Original fixtures generated through Git's public CLI; no upstream implementation/test source.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use girt::index::{Entry, Limits, Mode};
use girt::refs::{Expected, RefName, Reflog, Target, TransactionError};
use girt::{ColocationError, InitKind, ObjectFormat, ObjectId, OperationLimits, Repository};
use rstest::rstest;

fn command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(root)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "index.version=2",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Index Fixture")
        .env("GIT_AUTHOR_EMAIL", "index@example.invalid")
        .env("GIT_COMMITTER_NAME", "Index Fixture")
        .env("GIT_COMMITTER_EMAIL", "index@example.invalid")
        .env("LC_ALL", "C");
    command
}
fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = command(root, args).output().unwrap();
    success(output)
}
fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn repository(format: girt::ObjectFormat) -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Worktree).unwrap();
    (root, repo)
}

fn commit(root: &Path, content: &[u8], message: &str) -> ObjectId {
    fs::write(root.join("file"), content).unwrap();
    git(root, &["add", "file"]);
    git(root, &["commit", "-m", message]);
    String::from_utf8(git(root, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn publishes_index_and_detached_head_without_touching_branch_or_files(
    #[case] format: ObjectFormat,
) {
    let (_temp, repo) = repository(format);
    let root = repo.worktree().unwrap();
    let first = commit(root, b"first\n", "first");
    let second = commit(root, b"second\n", "second");
    let refs = repo.references().unwrap();
    let head = RefName::new(b"HEAD").unwrap();
    let original = refs.read(&head).unwrap().unwrap();
    let blob = repo.loose_objects().write_blob(b"first\n").unwrap();
    let mut edit = repo.edit_colocation(Limits::default()).unwrap();
    edit.index_mut()
        .replace_entries(vec![Entry::new(b"file".to_vec(), Mode::Regular, blob)])
        .unwrap();
    edit.commit(
        Target::Direct(first),
        Expected::Value(original.clone()),
        Reflog::Preserve,
    )
    .unwrap();
    assert_eq!(refs.read(&head).unwrap(), Some(Target::Direct(first)));
    let Target::Symbolic(branch) = original else {
        panic!("expected initial branch")
    };
    assert_eq!(refs.read(&branch).unwrap(), Some(Target::Direct(second)));
    assert_eq!(fs::read(root.join("file")).unwrap(), b"second\n");
    assert!(git(root, &["diff", "--cached", "--name-only"]).is_empty());
    assert_eq!(git(root, &["diff", "--name-only"]), b"file\n");
    git(root, &["fsck", "--no-reflogs"]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn head_conflict_preserves_original_index(#[case] format: ObjectFormat) {
    let (_temp, repo) = repository(format);
    let id = commit(repo.worktree().unwrap(), b"one", "one");
    let mut edit = repo.edit_colocation(Limits::default()).unwrap();
    edit.index_mut().replace_entries(vec![]).unwrap();
    let error = edit
        .commit(Target::Direct(id), Expected::Absent, Reflog::Preserve)
        .unwrap_err();
    assert!(matches!(
        error,
        ColocationError::Prepare {
            source: TransactionError::Prepare { .. },
            ..
        }
    ));
    assert_eq!(
        repo.read_index(Limits::default())
            .unwrap()
            .unwrap()
            .entries()
            .len(),
        1
    );
    assert_eq!(
        git(repo.worktree().unwrap(), &["rev-parse", "HEAD"]),
        format!("{id}\n").as_bytes()
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn concurrent_index_change_prevents_head_publication(#[case] format: ObjectFormat) {
    let (_temp, repo) = repository(format);
    let id = commit(repo.worktree().unwrap(), b"one", "one");
    let edit = repo.edit_colocation(Limits::default()).unwrap();
    fs::write(repo.git_dir().join("index"), b"concurrent writer").unwrap();
    let error = edit
        .commit(Target::Direct(id), Expected::Any, Reflog::Preserve)
        .unwrap_err();
    assert!(matches!(error, ColocationError::Index(_)));
    assert_eq!(
        fs::read(repo.git_dir().join("index")).unwrap(),
        b"concurrent writer"
    );
    assert!(
        String::from_utf8(git(repo.worktree().unwrap(), &["symbolic-ref", "HEAD"]))
            .unwrap()
            .starts_with("refs/heads/")
    );
    assert!(!repo.git_dir().join("index.lock").exists());
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn unborn_head_and_extended_flags_remain_caller_choices(#[case] format: ObjectFormat) {
    let (_temp, repo) = repository(format);
    let root = repo.worktree().unwrap();
    let blob = repo.loose_objects().write_blob(b"").unwrap();
    let mut placeholder = Entry::new(b"new".to_vec(), Mode::Regular, blob);
    placeholder.intent_to_add = true;
    let mut omitted = Entry::new(b"omitted".to_vec(), Mode::Regular, blob);
    omitted.skip_worktree = true;
    let mut edit = repo.edit_colocation(Limits::default()).unwrap();
    edit.index_mut()
        .replace_entries(vec![placeholder, omitted])
        .unwrap();
    let branch = RefName::new(b"refs/heads/unborn").unwrap();
    edit.commit(Target::Symbolic(branch), Expected::Exists, Reflog::Preserve)
        .unwrap();
    assert_eq!(git(root, &["symbolic-ref", "HEAD"]), b"refs/heads/unborn\n");
    assert!(
        !command(root, &["rev-parse", "--verify", "HEAD"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(git(root, &["ls-files", "-t"]), b"H new\nS omitted\n");
    let debug = String::from_utf8(git(root, &["ls-files", "--debug", "new"])).unwrap();
    assert!(debug.contains("20004000"));
    assert!(!root.join("new").exists());
    assert!(!root.join("omitted").exists());
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn reuses_git_stat_only_for_identical_entries(#[case] format: ObjectFormat) {
    let (_temp, repo) = repository(format);
    commit(repo.worktree().unwrap(), b"cached", "cached");
    let mut edit = repo.edit_index(Limits::default()).unwrap();
    let old = edit.index().entries()[0].clone();
    assert_ne!(old.stat, Default::default());
    let same = Entry::new(old.path.clone(), old.mode, old.id);
    edit.replace_entries_reusing_stat(vec![same.clone()])
        .unwrap();
    assert_eq!(edit.index().entries()[0].stat, old.stat);
    let mut flagged = same;
    flagged.skip_worktree = true;
    edit.replace_entries_reusing_stat(vec![flagged]).unwrap();
    assert_eq!(edit.index().entries()[0].stat, Default::default());
    edit.commit().unwrap();
    assert_eq!(
        git(repo.worktree().unwrap(), &["ls-files", "-t"]),
        b"S file\n"
    );
}

fn conflict(repo: &Repository, operation: &str) {
    let root = repo.worktree().unwrap();
    commit(root, b"base\n", "base");
    git(root, &["checkout", "-b", "side"]);
    let side = commit(root, b"side\n", "side");
    git(root, &["checkout", "-b", "other", "HEAD~"]);
    commit(root, b"other\n", "other");
    let output = command(root, &[operation, &side.to_string()])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[rstest]
#[case::sha1_merge(ObjectFormat::Sha1, "merge", "MERGE_HEAD")]
#[case::sha256_merge(ObjectFormat::Sha256, "merge", "MERGE_HEAD")]
#[case::sha1_pick(ObjectFormat::Sha1, "cherry-pick", "CHERRY_PICK_HEAD")]
#[case::sha256_pick(ObjectFormat::Sha256, "cherry-pick", "CHERRY_PICK_HEAD")]
#[case::sha1_rebase(ObjectFormat::Sha1, "rebase", "rebase-merge")]
#[case::sha256_rebase(ObjectFormat::Sha256, "rebase", "rebase-merge")]
fn inspects_and_cleans_git_generated_operation_without_reset(
    #[case] format: ObjectFormat,
    #[case] operation: &str,
    #[case] marker: &str,
) {
    let (_temp, repo) = repository(format);
    conflict(&repo, operation);
    let state = repo.operation_state(OperationLimits::default()).unwrap();
    assert!(state.roots().any(|root| root == marker));
    let index = fs::read(repo.git_dir().join("index")).unwrap();
    let head = fs::read(repo.git_dir().join("HEAD")).unwrap();
    let work = fs::read(repo.worktree().unwrap().join("file")).unwrap();
    state.cleanup().unwrap();
    assert!(
        repo.operation_state(OperationLimits::default())
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), index);
    assert_eq!(fs::read(repo.git_dir().join("HEAD")).unwrap(), head);
    assert_eq!(
        fs::read(repo.worktree().unwrap().join("file")).unwrap(),
        work
    );
    assert!(repo.git_dir().join("ORIG_HEAD").exists() || operation == "cherry-pick");
    git(repo.worktree().unwrap(), &["status", "--porcelain"]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn linked_worktree_routes_index_head_and_state_privately(#[case] format: ObjectFormat) {
    let (temp, repo) = repository(format);
    let id = commit(repo.worktree().unwrap(), b"base", "base");
    let linked_path = temp.path().join("linked");
    git(
        repo.worktree().unwrap(),
        &["worktree", "add", "--detach", linked_path.to_str().unwrap()],
    );
    let linked = Repository::open(&linked_path).unwrap();
    fs::write(repo.git_dir().join("MERGE_HEAD"), b"common marker").unwrap();
    fs::write(linked.git_dir().join("MERGE_HEAD"), format!("{id}\n")).unwrap();
    let original_index = fs::read(repo.git_dir().join("index")).unwrap();
    let original_head = fs::read(repo.git_dir().join("HEAD")).unwrap();
    let mut edit = linked.edit_colocation(Limits::default()).unwrap();
    edit.index_mut().replace_entries(vec![]).unwrap();
    edit.commit(
        Target::Symbolic(RefName::new(b"refs/heads/linked-unborn").unwrap()),
        Expected::Value(Target::Direct(id)),
        Reflog::Preserve,
    )
    .unwrap();
    linked
        .operation_state(OperationLimits::default())
        .unwrap()
        .cleanup()
        .unwrap();
    assert_eq!(
        fs::read(repo.git_dir().join("MERGE_HEAD")).unwrap(),
        b"common marker"
    );
    assert_eq!(
        fs::read(repo.git_dir().join("index")).unwrap(),
        original_index
    );
    assert_eq!(
        fs::read(repo.git_dir().join("HEAD")).unwrap(),
        original_head
    );
    assert!(git(&linked_path, &["ls-files"]).is_empty());
    assert_eq!(
        git(&linked_path, &["symbolic-ref", "HEAD"]),
        b"refs/heads/linked-unborn\n"
    );
}

#[rstest]
#[case::sha1_merge(ObjectFormat::Sha1, "merge", "MERGE_HEAD")]
#[case::sha256_merge(ObjectFormat::Sha256, "merge", "MERGE_HEAD")]
#[case::sha1_pick(ObjectFormat::Sha1, "cherry-pick", "CHERRY_PICK_HEAD")]
#[case::sha256_pick(ObjectFormat::Sha256, "cherry-pick", "CHERRY_PICK_HEAD")]
#[case::sha1_rebase(ObjectFormat::Sha1, "rebase", "rebase-merge")]
#[case::sha256_rebase(ObjectFormat::Sha256, "rebase", "rebase-merge")]
fn git_quit_removes_primary_state_without_resetting_index(
    #[case] format: ObjectFormat,
    #[case] operation: &str,
    #[case] marker: &str,
) {
    let (_temp, repo) = repository(format);
    conflict(&repo, operation);
    let before = fs::read(repo.git_dir().join("index")).unwrap();
    git(repo.worktree().unwrap(), &[operation, "--quit"]);
    assert!(!repo.git_dir().join(marker).exists());
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), before);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn cleans_git_apply_rebase_metadata(#[case] format: ObjectFormat) {
    let (_temp, repo) = repository(format);
    let root = repo.worktree().unwrap();
    commit(root, b"base\n", "base");
    git(root, &["checkout", "-b", "side"]);
    let side = commit(root, b"side\n", "side");
    git(root, &["checkout", "-b", "other", "HEAD~"]);
    commit(root, b"other\n", "other");
    let output = command(root, &["rebase", "--apply", &side.to_string()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let state = repo.operation_state(OperationLimits::default()).unwrap();
    assert!(state.roots().any(|name| name == "rebase-apply"));
    let before = git(root, &["ls-files", "-u"]);
    assert!(!before.is_empty());
    state.cleanup().unwrap();
    assert!(!repo.git_dir().join("rebase-apply").exists());
    assert_eq!(git(root, &["ls-files", "-u"]), before);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn cleans_git_sequencer_and_revert_state(#[case] format: ObjectFormat) {
    let (_temp, repo) = repository(format);
    let root = repo.worktree().unwrap();
    commit(root, b"base\n", "base");
    let second = commit(root, b"second\n", "second");
    let third = commit(root, b"third\n", "third");
    commit(root, b"fourth\n", "fourth");
    let output = command(
        root,
        &[
            "revert",
            "--no-edit",
            &third.to_string(),
            &second.to_string(),
        ],
    )
    .output()
    .unwrap();
    assert!(!output.status.success());
    let state = repo.operation_state(OperationLimits::default()).unwrap();
    assert!(state.roots().any(|name| name == "sequencer"));
    assert!(state.roots().any(|name| name == "REVERT_HEAD"));
    let before = git(root, &["ls-files", "-u"]);
    state.cleanup().unwrap();
    assert_eq!(git(root, &["ls-files", "-u"]), before);
    assert!(!repo.git_dir().join("sequencer").exists());
}
