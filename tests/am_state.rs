//! Original Git CLI fixtures; no upstream implementation or test sources are used.
use std::fs;

use girt::am::{Error, State};
use girt::{OperationLimits, Repository};
use rstest::rstest;

#[path = "support/apply_git.rs"]
mod oracle;
use oracle::{git, git_output};

#[rstest]
fn stopped_git_am_remains_resumable(
    #[values("sha1", "sha256")] format: &str,
    #[values("--keep", "--keep-non-patch", "--message-id")] policy: &str,
    #[values(false, true)] resume: bool,
) {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    git(
        root,
        &["init", "-b", "main", &format!("--object-format={format}")],
        b"",
    );
    fs::write(root.join("file"), b"base\n").unwrap();
    git(root, &["add", "file"], b"");
    git(root, &["commit", "-m", "base"], b"");
    let base = git(root, &["rev-parse", "HEAD"], b"");
    let base = std::str::from_utf8(&base).unwrap().trim();
    fs::write(root.join("file"), b"patch\n").unwrap();
    git(
        root,
        &["commit", "-am", "[topic] patch subject\n\nbody"],
        b"",
    );
    let mail = git(root, &["format-patch", "--stdout", "-1"], b"");
    git(root, &["checkout", "-b", "destination", base], b"");
    fs::write(root.join("file"), b"destination\n").unwrap();
    git(root, &["commit", "-am", "destination"], b"");
    let original = git(root, &["rev-parse", "HEAD"], b"");
    let failed = git_output(root, &["am", policy], &mail);
    assert!(!failed.status.success());
    let repo = Repository::open(root).unwrap();
    let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
    let state = State::from_operation(&snapshot).unwrap().unwrap();
    assert_eq!((state.next, state.last), (1, 1));
    assert_eq!(
        state.mail.unwrap(),
        git(root, &["am", "--show-current-patch=raw"], b"")
    );
    assert_eq!(
        state.patch.unwrap(),
        git(root, &["am", "--show-current-patch=diff"], b"")
    );
    for (name, actual) in [
        ("0001", state.mail),
        ("patch", state.patch),
        ("final-commit", state.message),
        ("author-script", state.author_script),
    ] {
        assert_eq!(
            actual.unwrap(),
            fs::read(repo.git_dir().join("rebase-apply").join(name)).unwrap()
        );
    }
    assert!(
        repo.operation_state(OperationLimits::default().with_max_bytes(1))
            .is_err()
    );
    if resume {
        fs::write(root.join("file"), b"resolved\n").unwrap();
        git(root, &["add", "file"], b"");
        git(root, &["am", "--continue"], b"");
        assert_eq!(git(root, &["show", "HEAD:file"], b""), b"resolved\n");
    } else {
        git(root, &["am", "--abort"], b"");
        assert_eq!(git(root, &["rev-parse", "HEAD"], b""), original);
        assert_eq!(fs::read(root.join("file")).unwrap(), b"destination\n");
    }
    let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
    assert!(State::from_operation(&snapshot).unwrap().is_none());
}

#[rstest]
#[case(b"", b"1\n")]
#[case(b"0\n", b"1\n")]
#[case(b"-1\n", b"1\n")]
#[case(b"1\n\n", b"1\n")]
#[case(b" 1\n", b"1\n")]
#[case(b"99999999999999999999999999999999", b"1\n")]
#[case(b"3\n", b"1\n")]
#[case(b"1\n", b"0\n")]
fn malformed_counters_are_refused(#[case] next: &[u8], #[case] last: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init"], b"");
    let repo = Repository::open(root.path()).unwrap();
    let state_dir = repo.git_dir().join("rebase-apply");
    fs::create_dir(&state_dir).unwrap();
    for (name, bytes) in [
        ("applying", b"".as_slice()),
        ("next", next),
        ("last", last),
        ("0001", b"mail"),
    ] {
        fs::write(state_dir.join(name), bytes).unwrap();
    }
    let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
    assert!(matches!(
        State::from_operation(&snapshot),
        Err(Error::Malformed(_))
    ));
}

#[test]
fn interrupted_preparation_and_completed_range_are_observations() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init"], b"");
    let repo = Repository::open(root.path()).unwrap();
    let dir = repo.git_dir().join("rebase-apply");
    fs::create_dir(&dir).unwrap();
    let read = || repo.operation_state(OperationLimits::default()).unwrap();
    assert!(matches!(State::from_operation(&read()), Err(Error::NotAm)));
    fs::write(dir.join("applying"), b"").unwrap();
    assert!(matches!(
        State::from_operation(&read()),
        Err(Error::Malformed("next"))
    ));
    fs::write(dir.join("next"), b"10000\n").unwrap();
    fs::write(dir.join("last"), b"10000\n").unwrap();
    assert!(matches!(
        State::from_operation(&read()),
        Err(Error::Malformed("current mail"))
    ));
    fs::write(dir.join("10000"), b"opaque mail").unwrap();
    let snapshot = read();
    let state = State::from_operation(&snapshot).unwrap().unwrap();
    assert_eq!(state.mail, Some(b"opaque mail".as_slice()));
    assert!(state.patch.is_none() && state.message.is_none() && state.author_script.is_none());
    fs::write(dir.join("next"), b"10001\n").unwrap();
    assert!(
        State::from_operation(&read())
            .unwrap()
            .unwrap()
            .mail
            .is_none()
    );
    fs::write(dir.join("rebasing"), b"").unwrap();
    assert!(matches!(State::from_operation(&read()), Err(Error::NotAm)));
}

#[rstest]
fn git_progress_and_rebase_classification(#[values("sha1", "sha256")] format: &str) {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    git(
        root,
        &["init", "-b", "main", &format!("--object-format={format}")],
        b"",
    );
    fs::write(root.join("file"), b"base\n").unwrap();
    git(root, &["add", "file"], b"");
    git(root, &["commit", "-m", "base"], b"");
    let base = git(root, &["rev-parse", "HEAD"], b"");
    let base = std::str::from_utf8(&base).unwrap().trim();
    fs::write(root.join("new"), b"clean first mail\n").unwrap();
    git(root, &["add", "new"], b"");
    git(root, &["commit", "-m", "first"], b"");
    fs::write(root.join("file"), b"patch\n").unwrap();
    git(root, &["commit", "-am", "second"], b"");
    let mail = git(root, &["format-patch", "--stdout", "-2"], b"");
    git(root, &["checkout", "-b", "destination", base], b"");
    fs::write(root.join("file"), b"destination\n").unwrap();
    git(root, &["commit", "-am", "destination"], b"");
    assert!(!git_output(root, &["am"], &mail).status.success());
    let repo = Repository::open(root).unwrap();
    let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
    let state = State::from_operation(&snapshot).unwrap().unwrap();
    assert_eq!((state.next, state.last), (2, 2));
    assert_eq!(
        state.mail.unwrap(),
        git(root, &["am", "--show-current-patch=raw"], b"")
    );
    git(root, &["am", "--abort"], b"");
    git(root, &["checkout", "main"], b"");
    assert!(
        !git_output(root, &["rebase", "--apply", "destination"], b"")
            .status
            .success()
    );
    let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
    assert!(matches!(
        State::from_operation(&snapshot),
        Err(Error::NotAm)
    ));
    git(root, &["rebase", "--abort"], b"");
}
