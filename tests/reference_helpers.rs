//! Git CLI observations of the reference convenience helpers in both reference backends.
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::AtomicBool;

use girt::refs::{FilesTransactionOptions, RefEdit, RefName, ReferenceError, References, Reflog};
use girt::{ObjectFormat, ObjectId, Repository, Signature};
use rstest::rstest;

fn git_attempt(root: &Path, args: &[&str]) -> Output {
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
        .expect("Git is required for interoperability tests");
    child.stdin.take().unwrap().write_all(b"").unwrap();
    child.wait_with_output().unwrap()
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = git_attempt(root, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// A non-bare repository whose `main` branch has two commits, created by Git.
fn fixture(
    format: ObjectFormat,
    backend: &str,
) -> (tempfile::TempDir, Repository, ObjectId, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--quiet",
            &format!("--object-format={format}"),
            &format!("--ref-format={backend}"),
            "--initial-branch=main",
            ".",
        ],
    );
    let commit = |message: &str| {
        git(
            root.path(),
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                message,
            ],
        );
        git(root.path(), &["rev-parse", "HEAD"])
            .trim()
            .parse()
            .unwrap()
    };
    let first = commit("first");
    let second = commit("second");
    let repo = Repository::open(root.path()).unwrap();
    (root, repo, first, second)
}

fn identity() -> Signature {
    Signature {
        name: b"C. Recorder".to_vec(),
        email: b"committer@example.com".to_vec(),
        seconds: 1_700_000_200,
        offset_minutes: 0,
    }
}

/// Every file under the Git directory with its bytes, to detect any write.
fn snapshot(directory: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(path.clone(), std::fs::read(path).unwrap());
            }
        }
    }
    files
}

// Git decides branch logging from core.logAllRefUpdates; the helpers publish what Git reads back.
#[rstest]
#[case::files_sha1(ObjectFormat::Sha1, "files")]
#[case::files_sha256(ObjectFormat::Sha256, "files")]
#[case::reftable_sha1(ObjectFormat::Sha1, "reftable")]
#[case::reftable_sha256(ObjectFormat::Sha256, "reftable")]
fn branch_create_update_delete_match_git(#[case] format: ObjectFormat, #[case] backend: &str) {
    let (root, repo, first, second) = fixture(format, backend);
    let refs = repo.references().unwrap();
    let branch = RefName::branch("topic").unwrap();
    let reflog = |message| repo.reflog_for(&branch, identity(), message).unwrap();

    refs.transaction(&[RefEdit::create(branch.clone(), first, reflog("created"))])
        .unwrap();
    refs.transaction(&[RefEdit::update(
        branch.clone(),
        first,
        second,
        reflog("moved"),
    )])
    .unwrap();
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/heads/topic"]).trim(),
        second.to_string()
    );
    assert_eq!(
        git(
            root.path(),
            &["reflog", "show", "--format=%gs", "refs/heads/topic"]
        ),
        "moved\ncreated\n"
    );

    let stale = refs.transaction(&[RefEdit::delete(branch.clone(), first, Reflog::Delete)]);
    assert!(stale.is_err());
    refs.transaction(&[RefEdit::delete(branch, second, Reflog::Delete)])
        .unwrap();
    assert!(
        !git_attempt(
            root.path(),
            &["rev-parse", "--verify", "--quiet", "refs/heads/topic"]
        )
        .status
        .success()
    );
    assert!(
        !git_attempt(root.path(), &["reflog", "exists", "refs/heads/topic"])
            .status
            .success()
    );
}

// `git update-ref -m` records nothing when the value does not change.
#[rstest]
#[case::files(ObjectFormat::Sha1, "files")]
#[case::reftable(ObjectFormat::Sha256, "reftable")]
fn unchanged_update_records_nothing(#[case] format: ObjectFormat, #[case] backend: &str) {
    let (root, repo, _, second) = fixture(format, backend);
    let main = RefName::branch("main").unwrap();
    let entries = || {
        git(root.path(), &["reflog", "show", "refs/heads/main"])
            .lines()
            .count()
    };
    let before = entries();
    let reflog = repo.reflog_for(&main, identity(), "unchanged").unwrap();
    repo.references()
        .unwrap()
        .transaction(&[RefEdit::update(main, second, second, reflog)])
        .unwrap();
    assert_eq!(entries(), before);
}

// Git's default policy leaves tags without a log in a non-bare repository.
#[rstest]
#[case::files(ObjectFormat::Sha1, "files")]
#[case::reftable(ObjectFormat::Sha1, "reftable")]
fn tag_creation_follows_git_default_policy(#[case] format: ObjectFormat, #[case] backend: &str) {
    let (root, repo, first, _) = fixture(format, backend);
    let tag = RefName::tag("v1").unwrap();
    let reflog = repo.reflog_for(&tag, identity(), "tagged").unwrap();
    assert_eq!(reflog, Reflog::Preserve);
    repo.references()
        .unwrap()
        .transaction(&[RefEdit::create(tag, first, reflog)])
        .unwrap();
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/tags/v1"]).trim(),
        first.to_string()
    );
    assert!(
        !git_attempt(root.path(), &["reflog", "exists", "refs/tags/v1"])
            .status
            .success()
    );
}

#[rstest]
#[case::files(ObjectFormat::Sha1, "files")]
#[case::reftable(ObjectFormat::Sha256, "reftable")]
fn empty_transactions_write_nothing(#[case] format: ObjectFormat, #[case] backend: &str) {
    let (_root, repo, _, _) = fixture(format, backend);
    let refs = repo.references().unwrap();
    let before = snapshot(repo.git_dir());
    assert!(refs.transaction(&[]).unwrap().is_empty());
    assert!(
        refs.transaction_controlled(&[], &AtomicBool::new(false))
            .unwrap()
            .is_empty()
    );
    assert_eq!(snapshot(repo.git_dir()), before);
}

#[test]
fn empty_files_transactions_with_options_write_nothing() {
    let (_root, repo, _, _) = fixture(ObjectFormat::Sha1, "files");
    let refs = repo.references().unwrap();
    let before = snapshot(repo.git_dir());
    let options = FilesTransactionOptions::GIT_DEFAULT;
    let cancel = AtomicBool::new(false);
    assert!(
        refs.transaction_files_with_options(&[], options, &cancel)
            .unwrap()
            .is_empty()
    );
    assert!(
        refs.transaction_files_with_stored_log_ids(&[], options, &cancel)
            .unwrap()
            .is_empty()
    );
    assert_eq!(snapshot(repo.git_dir()), before);
}

/// Names listed for `prefix` from refs Git wrote, some packed and some loose.
fn listed(refs: &References<'_>, prefix: &str) -> Vec<String> {
    refs.list_prefix(prefix)
        .unwrap()
        .into_iter()
        .map(|reference| String::from_utf8(reference.name.into_bytes()).unwrap())
        .collect()
}

#[rstest]
#[case::partial_component("refs/heads/fe", &["refs/heads/fe/x", "refs/heads/feature"])]
#[case::directory("refs/heads/fe/", &["refs/heads/fe/x"])]
#[case::namespace_slash(
    "refs/heads/",
    &["refs/heads/fe/x", "refs/heads/feature", "refs/heads/main"]
)]
#[case::exact("refs/heads/main", &["refs/heads/main"])]
#[case::other_namespace("refs/t", &["refs/tags/fe"])]
#[case::short_prefix(
    "re",
    &["refs/heads/fe/x", "refs/heads/feature", "refs/heads/main", "refs/tags/fe"]
)]
#[case::everything(
    "",
    &["refs/heads/fe/x", "refs/heads/feature", "refs/heads/main", "refs/tags/fe"]
)]
#[case::head("HEAD", &[])]
#[case::below_head("HEAD/x", &[])]
#[case::invalid_directory("refs/a..b/c", &[])]
fn list_prefix_selects_by_bytes(
    #[values("files", "reftable")] backend: &str,
    #[case] prefix: &str,
    #[case] expected: &[&str],
) {
    let (root, repo, first, _) = fixture(ObjectFormat::Sha1, backend);
    let id = first.to_string();
    git(root.path(), &["update-ref", "refs/heads/feature", &id]);
    git(root.path(), &["update-ref", "refs/tags/fe", &id]);
    git(root.path(), &["pack-refs", "--all"]);
    git(root.path(), &["update-ref", "refs/heads/fe/x", &id]);
    let refs = repo.references().unwrap();
    assert_eq!(listed(&refs, prefix), expected);
}

#[test]
fn list_prefix_observations_keep_packed_peeled_hints() {
    let (root, repo, first, _) = fixture(ObjectFormat::Sha1, "files");
    git(
        root.path(),
        &["tag", "-a", "-m", "annotated", "v1", &first.to_string()],
    );
    git(root.path(), &["pack-refs", "--all"]);
    let observations = repo
        .references()
        .unwrap()
        .list_prefix_observations("refs/tags/v")
        .unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].peeled_hint, Some(first));
}

/// A chain `refs/s<depth>` -> ... -> `refs/s1` -> `refs/heads/main`, made with Git.
fn symbolic_chain(root: &Path, depth: usize) -> RefName {
    let mut target = "refs/heads/main".to_owned();
    for hop in 1..=depth {
        let name = format!("refs/s{hop}");
        git(root, &["symbolic-ref", &name, &target]);
        target = name;
    }
    RefName::new(target).unwrap()
}

// `git rev-parse` follows four symbolic references and treats a fifth as dangling.
#[rstest]
fn default_resolution_depth_matches_git(#[values("files", "reftable")] backend: &str) {
    let (root, repo, _, second) = fixture(ObjectFormat::Sha1, backend);
    let deepest = symbolic_chain(root.path(), 5);
    let refs = repo.references().unwrap();

    let four = RefName::new("refs/s4").unwrap();
    assert_eq!(refs.resolve(&four).unwrap().id, Some(second));
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/s4"]).trim(),
        second.to_string()
    );

    assert!(matches!(
        refs.resolve(&deepest),
        Err(ReferenceError::Depth(References::DEFAULT_SYMREF_DEPTH))
    ));
    assert!(
        !git_attempt(
            root.path(),
            &["rev-parse", "--verify", "--quiet", "refs/s5"]
        )
        .status
        .success()
    );
    assert_eq!(refs.resolve_with(&deepest, 5).unwrap().id, Some(second));
}
