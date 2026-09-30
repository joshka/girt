//! Original Git CLI observations of conditional reflog publication in both reference backends.
mod git {
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    // UTC keeps this fixture focused on publication policy.
    pub fn git(root: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let mut child = command
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
            .env("GIT_AUTHOR_NAME", "A. Writer")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
            .env("GIT_COMMITTER_NAME", "C. Recorder")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "@1700000123 +0000")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
}

use girt::refs::{Expected, RefEdit, RefName, Reflog, Target};
use girt::{ObjectFormat, ObjectId, Repository, Signature};
use rstest::rstest;

fn name(value: &str) -> RefName {
    RefName::new(value).unwrap()
}

fn oid(bytes: &[u8]) -> ObjectId {
    std::str::from_utf8(bytes).unwrap().trim().parse().unwrap()
}

fn fixture(
    format: ObjectFormat,
    backend: &str,
) -> (tempfile::TempDir, Repository, ObjectId, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    git::git(
        root.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            "--template=",
            &format!("--object-format={format}"),
            &format!("--ref-format={backend}"),
            ".",
        ],
        b"",
    );
    let tree = oid(&git::git(root.path(), &["mktree"], b""));
    let first = oid(&git::git(
        root.path(),
        &["commit-tree", &tree.to_string()],
        b"first\n",
    ));
    let second = oid(&git::git(
        root.path(),
        &["commit-tree", &tree.to_string(), "-p", &first.to_string()],
        b"second\n",
    ));
    let repo = Repository::open(root.path()).unwrap();
    (root, repo, first, second)
}

fn edit(reference: &str, target: ObjectId, expected: Expected) -> RefEdit {
    RefEdit {
        name: name(reference),
        dereference: false,
        target: Some(Target::Direct(target)),
        expected,
        reflog: Reflog::AppendIfChanged {
            committer: Signature {
                name: b"C. Recorder".to_vec(),
                email: b"committer@example.com".to_vec(),
                seconds: 1700000123,
                offset_minutes: 0,
            },
            message: b"conditional update".to_vec(),
        },
    }
}

fn git_update(root: &std::path::Path, reference: &str, target: ObjectId) {
    git::git(
        root,
        &[
            "update-ref",
            "--no-deref",
            "--create-reflog",
            "-m",
            "conditional update",
            reference,
            &target.to_string(),
        ],
        b"",
    );
}

#[rstest]
fn conditional_append_direct_updates_match_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("files", "reftable")] backend: &str,
    #[values(Expected::Any, Expected::Exists)] expected: Expected,
) {
    let (root, repo, first, second) = fixture(format, backend);
    let (oracle_root, oracle, oracle_first, oracle_second) = fixture(format, backend);
    assert_eq!(first, oracle_first);
    assert_eq!(second, oracle_second);
    git_update(root.path(), "refs/heads/main", first);
    git_update(oracle_root.path(), "refs/heads/main", first);
    let refs = repo.references().unwrap();
    let oracle_refs = oracle.references().unwrap();
    let before = refs.reflog(&name("refs/heads/main")).unwrap();
    let unchanged = refs
        .transaction(&[edit("refs/heads/main", first, expected.clone())])
        .unwrap();
    git_update(oracle_root.path(), "refs/heads/main", first);
    assert!(unchanged[0].logs.is_empty());
    assert_eq!(refs.reflog(&name("refs/heads/main")).unwrap(), before);
    assert_eq!(
        refs.reflog(&name("refs/heads/main")).unwrap(),
        oracle_refs.reflog(&name("refs/heads/main")).unwrap()
    );
    let changed = refs
        .transaction(&[edit("refs/heads/main", second, expected)])
        .unwrap();
    git_update(oracle_root.path(), "refs/heads/main", second);
    assert_eq!(changed[0].logs.len(), 1);
    assert_eq!(
        refs.reflog(&name("refs/heads/main")).unwrap(),
        oracle_refs.reflog(&name("refs/heads/main")).unwrap()
    );
    assert_eq!(
        refs.read(&name("refs/heads/main")).unwrap(),
        oracle_refs.read(&name("refs/heads/main")).unwrap()
    );
}

#[rstest]
fn conditional_append_stored_head_detachment_matches_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("files", "reftable")] backend: &str,
) {
    let (root, repo, first, _second) = fixture(format, backend);
    let (oracle_root, oracle, oracle_first, _oracle_second) = fixture(format, backend);
    assert_eq!(first, oracle_first);
    git_update(root.path(), "refs/heads/main", first);
    git_update(oracle_root.path(), "refs/heads/main", first);
    let refs = repo.references().unwrap();
    let oracle_refs = oracle.references().unwrap();
    let branch_before = refs.reflog(&name("refs/heads/main")).unwrap();
    let result = refs
        .transaction(&[edit("HEAD", first, Expected::Exists)])
        .unwrap();
    git_update(oracle_root.path(), "HEAD", first);
    assert_eq!(
        result[0].logs,
        vec![(name("HEAD"), girt::refs::LogOutcome::Appended)]
    );
    assert_eq!(
        refs.read(&name("HEAD")).unwrap(),
        Some(Target::Direct(first))
    );
    assert_eq!(
        refs.reflog(&name("HEAD")).unwrap(),
        oracle_refs.reflog(&name("HEAD")).unwrap()
    );
    let entries = refs.reflog(&name("HEAD")).unwrap().unwrap();
    assert_eq!(entries.last().unwrap().old, first);
    assert_eq!(entries.last().unwrap().new, first);
    assert_eq!(
        refs.reflog(&name("refs/heads/main")).unwrap(),
        branch_before
    );
    assert_eq!(
        refs.reflog(&name("refs/heads/main")).unwrap(),
        oracle_refs.reflog(&name("refs/heads/main")).unwrap()
    );
}

#[rstest]
fn conditional_append_absent_creation_matches_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("files", "reftable")] backend: &str,
) {
    let (_root, repo, first, _second) = fixture(format, backend);
    let (oracle_root, oracle, oracle_first, _oracle_second) = fixture(format, backend);
    assert_eq!(first, oracle_first);
    let refs = repo.references().unwrap();
    let result = refs
        .transaction(&[edit("refs/heads/new", first, Expected::Absent)])
        .unwrap();
    git_update(oracle_root.path(), "refs/heads/new", first);
    assert_eq!(result[0].logs.len(), 1);
    assert_eq!(
        refs.reflog(&name("refs/heads/new")).unwrap(),
        oracle
            .references()
            .unwrap()
            .reflog(&name("refs/heads/new"))
            .unwrap()
    );
}

#[rstest]
fn conditional_append_unchanged_target_does_not_create_log_matching_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("files", "reftable")] backend: &str,
) {
    let (root, repo, first, _second) = fixture(format, backend);
    let (oracle_root, oracle, oracle_first, _oracle_second) = fixture(format, backend);
    assert_eq!(first, oracle_first);
    git::git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    git::git(
        oracle_root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    let refs = repo.references().unwrap();
    let result = refs
        .transaction(&[edit("refs/heads/main", first, Expected::Any)])
        .unwrap();
    git_update(oracle_root.path(), "refs/heads/main", first);
    assert!(result[0].logs.is_empty());
    assert_eq!(refs.reflog(&name("refs/heads/main")).unwrap(), None);
    assert_eq!(
        refs.reflog(&name("refs/heads/main")).unwrap(),
        oracle
            .references()
            .unwrap()
            .reflog(&name("refs/heads/main"))
            .unwrap()
    );
}

fn existing_edit(reference: &str, target: ObjectId, expected: Expected) -> RefEdit {
    let mut operation = edit(reference, target, expected);
    let Reflog::AppendIfChanged { committer, message } = operation.reflog else {
        unreachable!()
    };
    operation.reflog = Reflog::AppendExistingIfChanged { committer, message };
    operation
}

fn git_existing_update(root: &std::path::Path, reference: &str, target: ObjectId) {
    git::git(
        root,
        &[
            "-c",
            "core.logAllRefUpdates=false",
            "update-ref",
            "--no-deref",
            "-m",
            "conditional update",
            reference,
            &target.to_string(),
        ],
        b"",
    );
}

#[rstest]
fn existing_append_never_creates_missing_logs_matching_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("files", "reftable")] backend: &str,
) {
    let (root, repo, first, second) = fixture(format, backend);
    let (oracle_root, oracle, _, _) = fixture(format, backend);
    let refs = repo.references().unwrap();
    let oracle_refs = oracle.references().unwrap();
    let created = refs
        .transaction(&[existing_edit("refs/heads/main", first, Expected::Absent)])
        .unwrap();
    git_existing_update(oracle_root.path(), "refs/heads/main", first);
    assert!(created[0].logs.is_empty());
    assert!(!refs.has_reflog(&name("refs/heads/main")).unwrap());
    // Detaching a symbolic HEAD at the same terminal ID changes its stored target,
    // but absence of the HEAD log still forbids creating it.
    let detached = refs
        .transaction(&[existing_edit("HEAD", first, Expected::Exists)])
        .unwrap();
    git_existing_update(oracle_root.path(), "HEAD", first);
    assert!(detached[0].logs.is_empty());
    let advanced = refs
        .transaction(&[existing_edit("HEAD", second, Expected::Any)])
        .unwrap();
    git_existing_update(oracle_root.path(), "HEAD", second);
    assert!(advanced[0].logs.is_empty());
    assert_eq!(
        refs.read(&name("HEAD")).unwrap(),
        oracle_refs.read(&name("HEAD")).unwrap()
    );
    assert_eq!(
        refs.reflog(&name("HEAD")).unwrap(),
        oracle_refs.reflog(&name("HEAD")).unwrap()
    );
    assert!(!refs.has_reflog(&name("HEAD")).unwrap());
    assert!(!root.path().join("logs/HEAD").exists());
}

#[rstest]
fn existing_append_records_changes_and_skips_noops_matching_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("files", "reftable")] backend: &str,
) {
    let (root, repo, first, second) = fixture(format, backend);
    let (oracle_root, oracle, _, _) = fixture(format, backend);
    git_update(root.path(), "HEAD", first);
    git_update(oracle_root.path(), "HEAD", first);
    let refs = repo.references().unwrap();
    let oracle_refs = oracle.references().unwrap();
    let advanced = refs
        .transaction(&[existing_edit("HEAD", second, Expected::Exists)])
        .unwrap();
    git_existing_update(oracle_root.path(), "HEAD", second);
    assert_eq!(
        advanced[0].logs,
        vec![(name("HEAD"), girt::refs::LogOutcome::Appended)]
    );
    assert_eq!(
        refs.reflog(&name("HEAD")).unwrap(),
        oracle_refs.reflog(&name("HEAD")).unwrap()
    );
    let before = refs.reflog(&name("HEAD")).unwrap();
    let unchanged = refs
        .transaction(&[existing_edit("HEAD", second, Expected::Any)])
        .unwrap();
    git_existing_update(oracle_root.path(), "HEAD", second);
    assert!(unchanged[0].logs.is_empty());
    assert_eq!(refs.reflog(&name("HEAD")).unwrap(), before);
    assert_eq!(
        refs.reflog(&name("HEAD")).unwrap(),
        oracle_refs.reflog(&name("HEAD")).unwrap()
    );
}

#[rstest]
fn existing_append_does_not_revive_deleted_logs_matching_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("files", "reftable")] backend: &str,
) {
    let (root, repo, first, second) = fixture(format, backend);
    let (oracle_root, oracle, _, _) = fixture(format, backend);
    git_update(root.path(), "refs/heads/main", first);
    git_update(oracle_root.path(), "refs/heads/main", first);
    let refs = repo.references().unwrap();
    refs.transaction(&[RefEdit {
        name: name("refs/heads/main"),
        dereference: false,
        target: None,
        expected: Expected::Exists,
        reflog: Reflog::Delete,
    }])
    .unwrap();
    git::git(
        oracle_root.path(),
        &["update-ref", "-d", "refs/heads/main"],
        b"",
    );
    let result = refs
        .transaction(&[existing_edit("refs/heads/main", second, Expected::Absent)])
        .unwrap();
    git_existing_update(oracle_root.path(), "refs/heads/main", second);
    assert!(result[0].logs.is_empty());
    assert!(!refs.has_reflog(&name("refs/heads/main")).unwrap());
    assert_eq!(
        refs.reflog(&name("refs/heads/main")).unwrap(),
        oracle
            .references()
            .unwrap()
            .reflog(&name("refs/heads/main"))
            .unwrap()
    );
}

#[rstest]
#[case::files("files", "logs/HEAD.lock")]
#[case::reftable("reftable", "reftable/tables.list.lock")]
fn existing_append_requires_locks_even_when_log_is_absent(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] backend: &str,
    #[case] lock: &str,
) {
    let (root, repo, first, _) = fixture(format, backend);
    let refs = repo.references().unwrap();
    let before = refs.read(&name("HEAD")).unwrap();
    let lock = root.path().join(lock);
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, b"foreign lock").unwrap();
    let err = refs
        .transaction(&[existing_edit("HEAD", first, Expected::Exists)])
        .unwrap_err();
    assert!(matches!(
        err,
        girt::refs::TransactionError::Prepare {
            source: girt::refs::ReferenceError::Locked(_),
            ..
        }
    ));
    assert_eq!(refs.read(&name("HEAD")).unwrap(), before);
    assert!(!refs.has_reflog(&name("HEAD")).unwrap());
    assert_eq!(std::fs::read(lock).unwrap(), b"foreign lock");
}

#[rstest]
fn existing_append_detaches_head_with_existing_history_matching_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("files", "reftable")] backend: &str,
) {
    let (root, repo, first, _) = fixture(format, backend);
    let (oracle_root, oracle, _, _) = fixture(format, backend);
    git_existing_update(root.path(), "refs/heads/main", first);
    git_existing_update(oracle_root.path(), "refs/heads/main", first);
    let seed = [
        "-c",
        "core.logAllRefUpdates=true",
        "symbolic-ref",
        "-m",
        "seed",
        "HEAD",
        "refs/heads/main",
    ];
    git::git(root.path(), &seed, b"");
    git::git(oracle_root.path(), &seed, b"");
    let refs = repo.references().unwrap();
    assert!(refs.has_reflog(&name("HEAD")).unwrap());
    let result = refs
        .transaction(&[existing_edit("HEAD", first, Expected::Exists)])
        .unwrap();
    git_existing_update(oracle_root.path(), "HEAD", first);
    assert_eq!(
        result[0].logs,
        vec![(name("HEAD"), girt::refs::LogOutcome::Appended)]
    );
    assert_eq!(
        refs.reflog(&name("HEAD")).unwrap(),
        oracle.references().unwrap().reflog(&name("HEAD")).unwrap()
    );
    let entries = refs.reflog(&name("HEAD")).unwrap().unwrap();
    assert_eq!(entries.last().unwrap().old, first);
    assert_eq!(entries.last().unwrap().new, first);
    assert!(!refs.has_reflog(&name("refs/heads/main")).unwrap());
}

#[rstest]
fn existing_append_selects_each_log_in_resolved_head_chain(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("files", "reftable")] backend: &str,
) {
    let (root, repo, first, second) = fixture(format, backend);
    git_existing_update(root.path(), "refs/heads/main", first);
    git::git(
        root.path(),
        &[
            "-c",
            "core.logAllRefUpdates=true",
            "symbolic-ref",
            "-m",
            "seed",
            "HEAD",
            "refs/heads/main",
        ],
        b"",
    );
    let refs = repo.references().unwrap();
    let before = refs.reflog(&name("HEAD")).unwrap().unwrap();
    let mut operation = existing_edit("HEAD", second, Expected::Exists);
    operation.dereference = true;
    let result = refs.transaction(&[operation]).unwrap();
    assert_eq!(
        result[0].logs,
        vec![(name("HEAD"), girt::refs::LogOutcome::Appended)]
    );
    assert!(!refs.has_reflog(&name("refs/heads/main")).unwrap());
    let entries = refs.reflog(&name("HEAD")).unwrap().unwrap();
    assert_eq!(entries.len(), before.len() + 1);
    assert_eq!(entries.last().unwrap().old, first);
    assert_eq!(entries.last().unwrap().new, second);
    assert_eq!(
        refs.read(&name("HEAD")).unwrap(),
        Some(Target::Symbolic(name("refs/heads/main")))
    );
    assert_eq!(
        refs.read(&name("refs/heads/main")).unwrap(),
        Some(Target::Direct(second))
    );
}

#[rstest]
fn existing_append_treats_empty_files_log_as_present_matching_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let (root, repo, first, _) = fixture(format, "files");
    let (oracle_root, oracle, _, _) = fixture(format, "files");
    std::fs::create_dir_all(root.path().join("logs")).unwrap();
    std::fs::write(root.path().join("logs/HEAD"), b"").unwrap();
    std::fs::create_dir_all(oracle_root.path().join("logs")).unwrap();
    std::fs::write(oracle_root.path().join("logs/HEAD"), b"").unwrap();
    let refs = repo.references().unwrap();
    let result = refs
        .transaction(&[existing_edit("HEAD", first, Expected::Exists)])
        .unwrap();
    git_existing_update(oracle_root.path(), "HEAD", first);
    assert_eq!(
        result[0].logs,
        vec![(name("HEAD"), girt::refs::LogOutcome::Appended)]
    );
    assert_eq!(
        refs.reflog(&name("HEAD")).unwrap(),
        oracle.references().unwrap().reflog(&name("HEAD")).unwrap()
    );
}

/// `Repository::logs_updates_to` agrees with whether `git update-ref` creates a log.
#[rstest]
fn update_logging_matches_git(
    #[values(false, true)] bare: bool,
    #[values(None, Some("true"), Some("false"), Some("always"), Some("Always"))] setting: Option<
        &str,
    >,
    #[values(
        "HEAD",
        "refs/heads/main",
        "refs/remotes/origin/main",
        "refs/notes/commits",
        "refs/tags/v1",
        "refs/stash",
        "refs/jj/keep/1"
    )]
    reference: &str,
) {
    let root = tempfile::tempdir().unwrap();
    let mut init = vec!["init", "--quiet"];
    if bare {
        init.push("--bare");
    }
    init.push("repo");
    git::git(root.path(), &init, b"");
    let path = root.path().join("repo");
    if let Some(setting) = setting {
        git::git(&path, &["config", "core.logAllRefUpdates", setting], b"");
    }
    let repo = Repository::open(&path).unwrap();
    let predicted = repo.logs_updates_to(&name(reference)).unwrap();
    let tree = git::git(&path, &["mktree"], b"");
    let tree = String::from_utf8(tree).unwrap();
    let commit = git::git(&path, &["commit-tree", tree.trim(), "-m", "x"], b"");
    let commit = String::from_utf8(commit).unwrap();
    git::git(
        &path,
        &[
            "update-ref",
            "--no-deref",
            "-m",
            "update",
            reference,
            commit.trim(),
        ],
        b"",
    );
    let logs = repo.git_dir().join("logs").join(reference);
    assert_eq!(predicted, logs.is_file());
}

/// An existing log is appended to even when configuration disables new logs.
#[test]
fn existing_log_is_updated_when_logging_is_disabled() {
    let root = tempfile::tempdir().unwrap();
    git::git(root.path(), &["init", "--quiet", "repo"], b"");
    let path = root.path().join("repo");
    git::git(&path, &["config", "core.logAllRefUpdates", "false"], b"");
    let logs = path.join(".git/logs/refs/tags");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("v1"), b"").unwrap();
    let repo = Repository::open(&path).unwrap();
    assert!(repo.logs_updates_to(&name("refs/tags/v1")).unwrap());
    assert!(!repo.logs_updates_to(&name("refs/tags/v2")).unwrap());
}

/// Git refuses reference updates when the setting is invalid or has no value.
#[rstest]
#[case::invalid("[core]\n\tlogAllRefUpdates = sometimes\n")]
#[case::implicit("[core]\n\tlogAllRefUpdates\n")]
fn invalid_logging_setting_is_an_error(#[case] config: &str) {
    let root = tempfile::tempdir().unwrap();
    git::git(root.path(), &["init", "--quiet", "--bare", "repo"], b"");
    let path = root.path().join("repo");
    let mut bytes = std::fs::read(path.join("config")).unwrap();
    bytes.extend_from_slice(config.as_bytes());
    std::fs::write(path.join("config"), bytes).unwrap();
    let repo = Repository::open(&path).unwrap();
    assert!(matches!(
        repo.logs_updates_to(&name("refs/heads/main")),
        Err(girt::ReflogPolicyError::Config(_))
    ));
}
