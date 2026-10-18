//! Original Git-created am recovery fixtures; all Git processes use isolated configuration.
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use girt::am::CommitLimits;
use girt::refs::{Dereference, RefEdit, RefName, Reflog};
use girt::sequencer::{Action, Kind, Limits, State};
use girt::{ObjectId, OperationLimits, Repository, Signature};
use rstest::rstest;

#[path = "support/am_git.rs"]
mod fixture;
#[path = "support/apply_git.rs"]
mod oracle;
use oracle::{git, git_output};

fn head(root: &Path) -> ObjectId {
    std::str::from_utf8(&git(root, &["rev-parse", "HEAD"], b""))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
fn stopped(root: &Path, format: &str, backend: &str, partial_series: bool) {
    git(
        root,
        &[
            "init",
            "-b",
            "main",
            &format!("--object-format={format}"),
            &format!("--ref-format={backend}"),
        ],
        b"",
    );
    if !partial_series {
        fixture::stopped(root, format, false);
        return;
    }
    fs::write(root.join("file"), b"base\n").unwrap();
    git(root, &["add", "file"], b"");
    git(root, &["commit", "-m", "base"], b"");
    let base = head(root);
    fs::write(root.join("first"), b"first\n").unwrap();
    git(root, &["add", "first"], b"");
    git(root, &["commit", "-m", "first"], b"");
    fs::write(root.join("blocked"), b"last\n").unwrap();
    git(root, &["add", "blocked"], b"");
    git(root, &["commit", "-m", "last"], b"");
    let mail = git(root, &["format-patch", "--stdout", "-2"], b"");
    git(
        root,
        &["checkout", "-b", "destination", &base.to_string()],
        b"",
    );
    fs::write(root.join("blocked"), b"untracked\n").unwrap();
    assert!(!git_output(root, &["am"], &mail).status.success());
    fs::remove_file(root.join("blocked")).unwrap();
}
fn stage(root: &Path) {
    let patch = fs::read(root.join(".git/rebase-apply/patch")).unwrap();
    git(root, &["apply", "--index"], &patch);
}

#[rstest]
fn final_recovery_matches_git(
    #[values("sha1", "sha256")] format: &str,
    #[values("files", "reftable")] backend: &str,
    #[values(false, true)] partial_series: bool,
    #[values("continue", "skip", "abort")] action: &str,
) {
    let actual = tempfile::tempdir().unwrap();
    let expected = tempfile::tempdir().unwrap();
    stopped(actual.path(), format, backend, partial_series);
    stopped(expected.path(), format, backend, partial_series);
    let repo = Repository::open(actual.path()).unwrap();
    let state = State::read(repo.git_dir(), repo.object_format(), Limits::default())
        .unwrap()
        .unwrap();
    assert_eq!(state.kind(), Kind::Am);
    let old = head(actual.path());
    let writer =
        Signature::parse(git(actual.path(), &["var", "GIT_COMMITTER_IDENT"], b"").trim_ascii())
            .unwrap();
    let commit = if action == "continue" {
        stage(actual.path());
        stage(expected.path());
        let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
        let mail = girt::am::State::from_operation(&snapshot).unwrap().unwrap();
        let tree = git(actual.path(), &["write-tree"], b"");
        let tree = std::str::from_utf8(&tree).unwrap().trim().parse().unwrap();
        let commit = mail
            .prepare_commit(
                tree,
                Some(old),
                writer.clone(),
                CommitLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        repo.loose_objects().write_commit(&commit).unwrap();
        commit
    } else {
        repo.objects().unwrap().read_commit(old).unwrap()
    };
    let action_kind = match action {
        "continue" => Action::Continue(commit.id()),
        "skip" => Action::Skip,
        _ => Action::Abort,
    };
    let recovery = state.prepare(action_kind, old).unwrap();
    assert!(repo.git_dir().join("rebase-apply").exists());
    if action != "continue" {
        git(
            actual.path(),
            &[
                "read-tree",
                "--reset",
                "-u",
                &recovery.destination.to_string(),
            ],
            b"",
        );
    }
    let message = recovery.reflog_for(&commit);
    let reflog = if message.is_empty() {
        Reflog::Preserve
    } else {
        Reflog::Append {
            committer: writer,
            message,
        }
    };
    let edit = RefEdit::update(RefName::HEAD, old, recovery.destination, reflog)
        .with_dereference(Dereference::Follow);
    repo.references().unwrap().transaction(&[edit]).unwrap();
    recovery.publish_metadata(&repo).unwrap();
    git(expected.path(), &["am", &format!("--{action}")], b"");
    assert_eq!(head(actual.path()), head(expected.path()));
    for args in [
        vec!["ls-files", "--stage"],
        vec!["status", "--porcelain"],
        vec!["reflog", "-1", "--format=%gs"],
        vec!["rev-parse", "ORIG_HEAD"],
    ] {
        assert_eq!(
            git(actual.path(), &args, b""),
            git(expected.path(), &args, b""),
            "{action} {args:?}"
        );
    }
    assert!(!repo.git_dir().join("rebase-apply").exists());
    assert!(
        State::read(repo.git_dir(), repo.object_format(), Limits::default())
            .unwrap()
            .is_none()
    );
    git(actual.path(), &["fsck", "--no-reflogs"], b"");
}

#[rstest]
#[case("changed")]
#[case("unknown")]
#[case("range")]
#[case("mail")]
#[case("queued")]
#[case("safety")]
#[case("simultaneous")]
#[case("rebasing")]
#[case("limit")]
fn unsafe_recovery_preserves_active_state(#[case] failure: &str) {
    let root = tempfile::tempdir().unwrap();
    stopped(root.path(), "sha1", "files", false);
    let repo = Repository::open(root.path()).unwrap();
    let dir = repo.git_dir().join("rebase-apply");
    let read = || State::read(repo.git_dir(), repo.object_format(), Limits::default());
    let state = read().unwrap().unwrap();
    let old = head(root.path());
    match failure {
        "changed" => {
            let plan = state.prepare(Action::Skip, old).unwrap();
            fs::write(dir.join("patch"), b"independent data").unwrap();
            assert!(matches!(
                plan.publish_metadata(&repo),
                Err(girt::sequencer::Error::Changed)
            ));
            assert_eq!(fs::read(dir.join("patch")).unwrap(), b"independent data");
        }
        "unknown" => {
            fs::write(dir.join("unknown"), b"retain").unwrap();
            assert!(matches!(
                read(),
                Err(girt::sequencer::Error::Unsupported(_))
            ));
        }
        "range" => {
            fs::write(dir.join("next"), b"3\n").unwrap();
            assert!(matches!(
                read(),
                Err(girt::sequencer::Error::Unsupported(_))
            ));
        }
        "mail" => {
            fs::remove_file(dir.join("0001")).unwrap();
            assert!(matches!(
                read(),
                Err(girt::sequencer::Error::Unsupported(_))
            ));
        }
        "queued" => {
            fs::write(dir.join("last"), b"2\n").unwrap();
            fs::write(dir.join("0002"), b"next mail").unwrap();
            assert!(matches!(
                read().unwrap().unwrap().prepare(Action::Skip, old),
                Err(girt::sequencer::Error::Unsupported(_))
            ));
        }
        "safety" => {
            fs::write(
                dir.join("abort-safety"),
                format!("{}\n", ObjectId::null(repo.object_format())),
            )
            .unwrap();
            assert!(
                read()
                    .unwrap()
                    .unwrap()
                    .prepare(Action::Abort, old)
                    .is_err()
            );
        }
        "simultaneous" => {
            fs::write(repo.git_dir().join("CHERRY_PICK_HEAD"), format!("{old}\n")).unwrap();
            assert!(matches!(
                read(),
                Err(girt::sequencer::Error::Unsupported(_))
            ));
        }
        "rebasing" => {
            fs::write(dir.join("rebasing"), b"").unwrap();
            assert!(matches!(
                read(),
                Err(girt::sequencer::Error::Unsupported(_))
            ));
        }
        "limit" => {
            let mut limits = Limits::default();
            limits.max_bytes = 1;
            assert!(matches!(
                State::read(repo.git_dir(), repo.object_format(), limits),
                Err(girt::sequencer::Error::Limit)
            ));
        }
        _ => unreachable!(),
    }
    assert!(dir.join("applying").exists());
    assert_eq!(head(root.path()), old);
}

#[test]
fn foreign_repository_cannot_publish_am_metadata() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    stopped(a.path(), "sha1", "files", false);
    stopped(b.path(), "sha1", "files", false);
    let a = Repository::open(a.path()).unwrap();
    let b = Repository::open(b.path()).unwrap();
    let state = State::read(a.git_dir(), a.object_format(), Limits::default())
        .unwrap()
        .unwrap();
    let recovery = state
        .prepare(Action::Skip, b.head().unwrap().id.unwrap())
        .unwrap();
    assert!(matches!(
        recovery.publish_metadata(&b),
        Err(girt::sequencer::Error::Changed)
    ));
    assert!(a.git_dir().join("rebase-apply/applying").exists());
    assert!(b.git_dir().join("rebase-apply/applying").exists());
}
