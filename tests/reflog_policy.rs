//! Original Git CLI observations of conditional reflog publication in both reference backends.
mod git {
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    // UTC isolates publication policy from the independently tracked reftable timezone codec.
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
