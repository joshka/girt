//! Original Git-created am recovery fixtures; all Git processes use isolated configuration.
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use girt::am::CommitLimits;
use girt::refs::{Dereference, RefEdit, RefName, Reflog};
use girt::sequencer::{Action, Kind, Limits, State};
use girt::{ObjectId, OperationLimits, Repository, Signature};
use rstest::rstest;

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
    fs::write(root.join("file"), b"base\n").unwrap();
    git(root, &["add", "file"], b"");
    git(root, &["commit", "-m", "base"], b"");
    let base = head(root);
    if partial_series {
        fs::write(root.join("first"), b"first\n").unwrap();
        git(root, &["add", "first"], b"");
        git(root, &["commit", "-m", "first"], b"");
    }
    fs::write(root.join("blocked"), b"last\n").unwrap();
    git(root, &["add", "blocked"], b"");
    git(root, &["commit", "-m", "last"], b"");
    let mail = git(
        root,
        &[
            "format-patch",
            "--stdout",
            if partial_series { "-2" } else { "-1" },
        ],
        b"",
    );
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
    let state = State::read_repository(&repo, Limits::default())
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
        State::read_repository(&repo, Limits::default())
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
#[case("original_missing")]
#[case("original_null")]
#[case("simultaneous")]
#[case("rebasing")]
#[case("limit")]
fn unsafe_recovery_preserves_active_state(#[case] failure: &str) {
    let root = tempfile::tempdir().unwrap();
    stopped(root.path(), "sha1", "files", false);
    let repo = Repository::open(root.path()).unwrap();
    let dir = repo.git_dir().join("rebase-apply");
    let read = || State::read_repository(&repo, Limits::default());
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
        "original_missing" | "original_null" => {
            let original = repo.git_dir().join("ORIG_HEAD");
            if failure == "original_missing" {
                fs::remove_file(original).unwrap();
            } else {
                fs::write(
                    original,
                    format!("{}\n", ObjectId::null(repo.object_format())),
                )
                .unwrap();
            }
            assert!(matches!(
                read().unwrap().unwrap().prepare(Action::Abort, old),
                Err(girt::sequencer::Error::Unsupported(_))
            ));
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
    let state = State::read_repository(&a, Limits::default())
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

#[test]
fn changed_reftable_original_head_preserves_am_state() {
    let root = tempfile::tempdir().unwrap();
    stopped(root.path(), "sha1", "reftable", false);
    let repo = Repository::open(root.path()).unwrap();
    let mut limits = Limits::default();
    limits.reftable.tables = 0;
    assert!(matches!(
        State::read_repository(&repo, limits),
        Err(girt::sequencer::Error::Reference(_))
    ));
    let state = State::read_repository(&repo, Limits::default())
        .unwrap()
        .unwrap();
    let recovery = state.prepare(Action::Abort, head(root.path())).unwrap();
    let source = git(root.path(), &["rev-parse", "main"], b"");
    git(
        root.path(),
        &[
            "update-ref",
            "ORIG_HEAD",
            std::str::from_utf8(&source).unwrap().trim(),
        ],
        b"",
    );
    assert!(matches!(
        recovery.publish_metadata(&repo),
        Err(girt::sequencer::Error::Changed)
    ));
    assert!(repo.git_dir().join("rebase-apply/applying").exists());
}

#[rstest]
fn linked_worktree_uses_private_am_and_original_head(
    #[values("sha1", "sha256")] format: &str,
    #[values("files", "reftable")] backend: &str,
) {
    let temporary = tempfile::tempdir().unwrap();
    let main = temporary.path().join("main");
    let linked = temporary.path().join("linked");
    fs::create_dir(&main).unwrap();
    git(
        &main,
        &[
            "init",
            "-b",
            "main",
            &format!("--object-format={format}"),
            &format!("--ref-format={backend}"),
        ],
        b"",
    );
    fs::write(main.join("file"), b"base\n").unwrap();
    git(&main, &["add", "file"], b"");
    git(&main, &["commit", "-m", "base"], b"");
    let base = head(&main);
    fs::write(main.join("blocked"), b"source\n").unwrap();
    git(&main, &["add", "blocked"], b"");
    git(&main, &["commit", "-m", "source"], b"");
    let source = head(&main);
    let mail = git(&main, &["format-patch", "--stdout", "-1"], b"");
    git(
        &main,
        &["update-ref", "ORIG_HEAD", &source.to_string()],
        b"",
    );
    git(
        &main,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            &base.to_string(),
        ],
        b"",
    );
    fs::write(linked.join("blocked"), b"untracked\n").unwrap();
    assert!(!git_output(&linked, &["am"], &mail).status.success());
    fs::remove_file(linked.join("blocked")).unwrap();
    let repo = Repository::open(&linked).unwrap();
    let state = State::read_repository(&repo, Limits::default())
        .unwrap()
        .unwrap();
    assert_eq!(state.original_head(), Some(base));
    let recovery = state.prepare(Action::Skip, base).unwrap();
    recovery.publish_metadata(&repo).unwrap();
    assert!(!repo.git_dir().join("rebase-apply").exists());
    assert_eq!(head(&main), source);
    assert_eq!(
        git(&main, &["rev-parse", "ORIG_HEAD"], b"").trim_ascii(),
        source.to_string().as_bytes()
    );
    assert_eq!(head(&linked), base);
}

#[test]
fn changed_destination_head_preserves_active_mail() {
    let root = tempfile::tempdir().unwrap();
    stopped(root.path(), "sha1", "files", false);
    let repo = Repository::open(root.path()).unwrap();
    let state = State::read_repository(&repo, Limits::default())
        .unwrap()
        .unwrap();
    let recovery = state.prepare(Action::Skip, head(root.path())).unwrap();
    let source = git(root.path(), &["rev-parse", "main"], b"");
    git(
        root.path(),
        &[
            "update-ref",
            "HEAD",
            std::str::from_utf8(&source).unwrap().trim(),
        ],
        b"",
    );
    assert!(matches!(
        recovery.publish_metadata(&repo),
        Err(girt::sequencer::Error::Changed)
    ));
    assert!(repo.git_dir().join("rebase-apply/applying").exists());
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"], b""), source);
}

fn queued_stop(root: &Path, format: &str, backend: &str) {
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
    fs::write(root.join("base"), b"base\n").unwrap();
    git(root, &["add", "."], b"");
    git(root, &["commit", "-m", "base"], b"");
    let base = head(root);
    for name in ["one", "two", "three"] {
        fs::write(root.join(name), format!("{name}\n")).unwrap();
        git(root, &["add", name], b"");
        git(
            root,
            &["commit", "-m", &format!("{name}\n\nBody of {name}.")],
            b"",
        );
    }
    let mail = git(root, &["format-patch", "--stdout", "-3"], b"");
    git(
        root,
        &["checkout", "-b", "destination", &base.to_string()],
        b"",
    );
    for name in ["one", "two"] {
        fs::write(root.join(name), b"obstruction\n").unwrap();
    }
    assert!(!git_output(root, &["am"], &mail).status.success());
    fs::remove_file(root.join("one")).unwrap();
}

#[rstest]
fn queued_mail_is_prepared_like_git_and_git_can_finish(
    #[values("sha1", "sha256")] format: &str,
    #[values("files", "reftable")] backend: &str,
    #[values(false, true)] continue_current: bool,
) {
    let actual = tempfile::tempdir().unwrap();
    let expected = tempfile::tempdir().unwrap();
    queued_stop(actual.path(), format, backend);
    queued_stop(expected.path(), format, backend);
    for root in [actual.path(), expected.path()]
        .into_iter()
        .filter(|_| backend == "files")
    {
        fs::write(root.join(".git/MERGE_MSG"), b"stale merge message\n").unwrap();
        fs::write(root.join(".git/AUTO_MERGE"), format!("{}\n", head(root))).unwrap();
    }
    let repo = Repository::open(actual.path()).unwrap();
    let old = head(actual.path());
    let state = State::read_repository(&repo, Limits::default())
        .unwrap()
        .unwrap();
    let action = if continue_current {
        stage(actual.path());
        stage(expected.path());
        let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
        let mail = girt::am::State::from_operation(&snapshot).unwrap().unwrap();
        let tree = git(actual.path(), &["write-tree"], b"");
        let tree = std::str::from_utf8(&tree).unwrap().trim().parse().unwrap();
        let writer =
            Signature::parse(git(actual.path(), &["var", "GIT_COMMITTER_IDENT"], b"").trim_ascii())
                .unwrap();
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
        let edit = RefEdit::update(
            RefName::HEAD,
            old,
            commit.id(),
            Reflog::Append {
                committer: writer,
                message: [b"am: ".as_slice(), &commit.subject()].concat(),
            },
        )
        .with_dereference(Dereference::Follow);
        repo.references().unwrap().transaction(&[edit]).unwrap();
        Action::Continue(commit.id())
    } else {
        Action::Skip
    };
    let plan = state
        .prepare_next_am(
            &repo,
            action,
            old,
            girt::mailinfo::Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        plan.file_after("rebase-apply/next"),
        Some(b"2\n".as_slice())
    );
    plan.publish_metadata(&repo).unwrap();
    assert!(
        !git_output(
            expected.path(),
            &[
                "am",
                if continue_current {
                    "--continue"
                } else {
                    "--skip"
                }
            ],
            b""
        )
        .status
        .success()
    );
    for name in [
        "next",
        "last",
        "abort-safety",
        "author-script",
        "final-commit",
        "patch",
        "info",
        "msg",
        "0001",
        "0002",
        "0003",
    ] {
        assert_eq!(
            fs::read(repo.git_dir().join("rebase-apply").join(name)).unwrap(),
            fs::read(expected.path().join(".git/rebase-apply").join(name)).unwrap(),
            "{name}"
        );
    }
    for root in [actual.path(), expected.path()] {
        assert_eq!(
            root.join(".git/MERGE_MSG").exists(),
            backend == "files" && continue_current
        );
        assert_eq!(
            root.join(".git/AUTO_MERGE").exists(),
            backend == "files" && continue_current
        );
        fs::remove_file(root.join("two")).unwrap();
        stage(root);
        git(root, &["am", "--continue"], b"");
    }
    assert_eq!(head(actual.path()), head(expected.path()));
    assert_eq!(
        git(actual.path(), &["ls-files", "--stage"], b""),
        git(expected.path(), &["ls-files", "--stage"], b"")
    );
    git(actual.path(), &["fsck", "--no-reflogs"], b"");
}

#[rstest]
#[case("missing")]
#[case("bad_mail")]
#[case("option")]
#[case("limit")]
#[case("cancel")]
#[case("stale")]
fn queued_failure_retains_numbered_mail_and_progress(#[case] failure: &str) {
    let root = tempfile::tempdir().unwrap();
    queued_stop(root.path(), "sha1", "files");
    let repo = Repository::open(root.path()).unwrap();
    let dir = repo.git_dir().join("rebase-apply");
    match failure {
        "missing" => fs::remove_file(dir.join("0002")).unwrap(),
        "bad_mail" => fs::write(dir.join("0002"), b"broken mail\n").unwrap(),
        "option" => fs::write(dir.join("sign"), b"t\n").unwrap(),
        _ => (),
    }
    let state = State::read_repository(&repo, Limits::default())
        .unwrap()
        .unwrap();
    let limits = if failure == "limit" {
        girt::mailinfo::Limits::default().with_max_input_bytes(0)
    } else {
        girt::mailinfo::Limits::default()
    };
    let plan = state.prepare_next_am(
        &repo,
        Action::Skip,
        head(root.path()),
        limits,
        &AtomicBool::new(failure == "cancel"),
    );
    if failure == "stale" {
        fs::write(dir.join("0002"), b"changed\n").unwrap();
        assert!(matches!(
            plan.unwrap().publish_metadata(&repo),
            Err(girt::sequencer::Error::Changed)
        ));
    } else {
        assert!(plan.is_err(), "{failure}");
    }
    assert_eq!(fs::read(dir.join("next")).unwrap(), b"1\n");
    assert!(dir.join("0001").exists());
    assert!(dir.join("0003").exists());
}

#[test]
fn generated_next_artifacts_obey_the_combined_metadata_budget() {
    let root = tempfile::tempdir().unwrap();
    queued_stop(root.path(), "sha1", "files");
    let dir = root.path().join(".git/rebase-apply");
    let raw = fs::read_to_string(dir.join("0002")).unwrap();
    fs::write(
        dir.join("0002"),
        raw.replace(
            "Subject: [PATCH 2/3] two",
            &format!("Subject: [PATCH 2/3] {}", "large".repeat(1000)),
        ),
    )
    .unwrap();
    let bytes: usize = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len() as usize)
        .sum::<usize>()
        + fs::metadata(root.path().join(".git/ORIG_HEAD"))
            .unwrap()
            .len() as usize;
    let repo = Repository::open(root.path()).unwrap();
    let mut limits = Limits::default();
    limits.max_bytes = bytes;
    let state = State::read_repository(&repo, limits).unwrap().unwrap();
    assert!(matches!(
        state.prepare_next_am(
            &repo,
            Action::Skip,
            head(root.path()),
            girt::mailinfo::Limits::default(),
            &AtomicBool::new(false)
        ),
        Err(girt::sequencer::Error::Limit)
    ));
    assert_eq!(fs::read(dir.join("next")).unwrap(), b"1\n");
}

#[rstest]
#[case("mailinfo.scissors", "true")]
#[case("mailinfo.quotedCr", "strip")]
#[case("i18n.commitEncoding", "ISO-8859-1")]
fn queued_extraction_refuses_unresolved_config_policy(#[case] key: &str, #[case] value: &str) {
    let root = tempfile::tempdir().unwrap();
    queued_stop(root.path(), "sha1", "files");
    git(root.path(), &["config", key, value], b"");
    let repo = Repository::open(root.path()).unwrap();
    let state = State::read_repository(&repo, Limits::default())
        .unwrap()
        .unwrap();
    assert!(matches!(
        state.prepare_next_am(
            &repo,
            Action::Skip,
            head(root.path()),
            girt::mailinfo::Limits::default(),
            &AtomicBool::new(false)
        ),
        Err(girt::sequencer::Error::Unsupported(_))
    ));
    assert_eq!(
        fs::read(repo.git_dir().join("rebase-apply/next")).unwrap(),
        b"1\n"
    );
}
