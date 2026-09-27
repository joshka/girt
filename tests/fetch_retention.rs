//! Original fixtures generated with Git CLI commands; no upstream fixture or source input.
#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::ops::ControlFlow;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use girt::fetch::{FetchLimits, receive_local};
use girt::refs::{Expected, RefName, Target};
use girt::{InitKind, ObjectFormat, ObjectId, PackLimits, Repository};
use rstest::rstest;

fn git(path: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    let mut child = command
        .current_dir(path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Retention fixture")
        .env("GIT_AUTHOR_EMAIL", "retention@example.com")
        .env("GIT_COMMITTER_NAME", "Retention fixture")
        .env("GIT_COMMITTER_EMAIL", "retention@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
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
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn id(bytes: Vec<u8>) -> ObjectId {
    std::str::from_utf8(&bytes).unwrap().trim().parse().unwrap()
}

fn source(path: &Path, format: &str) -> ObjectId {
    fs::create_dir(path).unwrap();
    git(
        path,
        &[
            "init",
            "--bare",
            "--template=",
            "--initial-branch=main",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
    let blob = id(git(
        path,
        &["hash-object", "-w", "--stdin"],
        b"retained original payload\n",
    ));
    let tree = id(git(
        path,
        &["mktree"],
        format!("100644 blob {blob}\tfile\n").as_bytes(),
    ));
    let commit = id(git(
        path,
        &["commit-tree", &tree.to_string()],
        b"original retained commit\n",
    ));
    git(
        path,
        &["update-ref", "refs/heads/main", &commit.to_string()],
        b"",
    );
    commit
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1, "sha1")]
#[case::sha256(ObjectFormat::Sha256, "sha256")]
fn git_collection_between_installation_and_ref_publication_preserves_history(
    #[case] format: ObjectFormat,
    #[case] git_format: &str,
) {
    let root = tempfile::tempdir().unwrap();
    let source_path = root.path().join("source");
    let tip = source(&source_path, git_format);
    let repository =
        Repository::init(format, root.path().join("destination"), InitKind::Bare).unwrap();
    let cancel = AtomicBool::new(false);
    let received = receive_local(
        &source_path,
        |_| vec![tip],
        FetchLimits::default(),
        &cancel,
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    // After validation the transfer owns the complete history; even losing the source cannot
    // invalidate its objects. This also rules out accidental dependence on source storage.
    fs::remove_dir_all(&source_path).unwrap();
    let (_, retention) = received
        .install_retained(&repository, PackLimits::default(), &cancel)
        .unwrap();
    let keep = retention.path().to_owned();
    git(repository.git_dir(), &["repack", "-Ad"], b"");
    git(repository.git_dir(), &["prune", "--expire=now"], b"");
    assert!(keep.exists());
    assert_eq!(
        git(repository.git_dir(), &["show", &format!("{tip}:file")], b""),
        b"retained original payload\n"
    );
    repository
        .references()
        .unwrap()
        .update_without_reflog(
            &RefName::new("refs/remotes/origin/main").unwrap(),
            Target::Direct(tip),
            Expected::Absent,
        )
        .unwrap();
    retention.release().unwrap();
    assert!(!keep.exists());
    git(repository.git_dir(), &["gc", "--prune=now"], b"");
    git(
        repository.git_dir(),
        &["fsck", "--full", "--no-reflogs"],
        b"",
    );
    assert_eq!(
        git(
            repository.git_dir(),
            &["show", "refs/remotes/origin/main:file"],
            b""
        ),
        b"retained original payload\n"
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1, "sha1")]
#[case::sha256(ObjectFormat::Sha256, "sha256")]
fn failed_ref_publication_leaves_explicit_retention_until_abandoned(
    #[case] format: ObjectFormat,
    #[case] git_format: &str,
) {
    let root = tempfile::tempdir().unwrap();
    let source_path = root.path().join("source");
    let tip = source(&source_path, git_format);
    let repository =
        Repository::init(format, root.path().join("destination"), InitKind::Bare).unwrap();
    let cancel = AtomicBool::new(false);
    let received = receive_local(
        &source_path,
        |_| vec![tip],
        FetchLimits::default(),
        &cancel,
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let (_, retention) = received
        .install_retained(&repository, PackLimits::default(), &cancel)
        .unwrap();
    fs::create_dir_all(repository.git_dir().join("refs/remotes/origin")).unwrap();
    let lock = repository.git_dir().join("refs/remotes/origin/main.lock");
    fs::write(&lock, b"independent writer").unwrap();
    let refs = repository.references().unwrap();
    let name = RefName::new("refs/remotes/origin/main").unwrap();
    assert!(
        refs.update_without_reflog(&name, Target::Direct(tip), Expected::Absent)
            .is_err()
    );
    assert!(refs.read(&name).unwrap().is_none());
    git(repository.git_dir(), &["repack", "-Ad"], b"");
    assert_eq!(
        git(repository.git_dir(), &["show", &format!("{tip}:file")], b""),
        b"retained original payload\n"
    );
    assert_eq!(fs::read(&lock).unwrap(), b"independent writer");
    let keep = retention.path().to_owned();
    retention.release().unwrap();
    assert!(!keep.exists());
    // Once abandoned, the same collection must be able to reclaim the unreferenced history.
    git(repository.git_dir(), &["gc", "--prune=now"], b"");
    assert_eq!(
        git(
            repository.git_dir(),
            &["cat-file", "--batch-check"],
            format!("{tip}\n").as_bytes()
        ),
        format!("{tip} missing\n").as_bytes(),
    );
}
