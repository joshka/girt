//! Packing loose references, checked against `git pack-refs` on runtime-generated repositories.
//! No upstream source or fixtures are used. Packing is Unix-only; see `pack` for why.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use girt::refs::{
    Expected, PackRefsError, PackRefsOptions, PackScope, PackTrigger, RefName, ReferenceError,
    Target,
};
use girt::{ObjectFormat, PackLimits, Repository};
use rstest::rstest;

fn git_attempt(directory: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "A. Writer")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "C. Recorder")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("Git is required for interoperability tests")
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = git_attempt(directory, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn name(value: &str) -> RefName {
    RefName::new(value).unwrap()
}

const MISSING_SHA1: &str = "1234567890123456789012345678901234567890";
const MISSING_SHA256: &str = "1234567890123456789012345678901234567890123456789012345678901234";

/// Builds a repository covering every kind of loose reference pack-refs must treat differently.
fn fixture(format: ObjectFormat) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let format_name = match format {
        ObjectFormat::Sha1 => "sha1",
        ObjectFormat::Sha256 => "sha256",
        _ => panic!("unsupported hash format in test fixture"),
    };
    git(
        root.path(),
        &["init", "-q", "--object-format", format_name, "repo"],
    );
    let repo = root.path().join("repo");
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
    git(&repo, &["tag", "light"]);
    git(&repo, &["tag", "-a", "-m", "annotated", "annotated"]);
    git(&repo, &["tag", "-a", "-m", "nested", "nested", "annotated"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);
    git(&repo, &["branch", "topic"]);
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
    git(&repo, &["update-ref", "refs/jj/keep/one", "HEAD"]);
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(&repo, &["update-ref", "refs/bisect/bad", "HEAD"]);
    git(&repo, &["update-ref", "refs/worktree/private", "HEAD"]);
    // Partially packed: an older packed value that a loose file now shadows.
    git(&repo, &["pack-refs", "--include", "refs/heads/topic"]);
    git(&repo, &["update-ref", "refs/heads/topic", "HEAD~1"]);
    let missing = match format {
        ObjectFormat::Sha1 => MISSING_SHA1,
        ObjectFormat::Sha256 => MISSING_SHA256,
        _ => panic!("unsupported hash format in test fixture"),
    };
    fs::write(repo.join(".git/refs/heads/missing"), format!("{missing}\n")).unwrap();
    fs::write(repo.join(".git/refs/heads/malformed"), "garbage\n").unwrap();
    (root, repo)
}

fn copy(root: &Path, from: &Path, to: &str) -> PathBuf {
    let destination = root.join(to);
    let status = Command::new("cp")
        .arg("-R")
        .arg(from)
        .arg(&destination)
        .status()
        .unwrap();
    assert!(status.success());
    destination
}

fn loose_files(repo: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut pending = vec![repo.join(".git/refs")];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(
                    path.strip_prefix(repo.join(".git"))
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    files.sort();
    files
}

fn pack(repo: &Path, options: PackRefsOptions) -> girt::refs::PackRefsReport {
    let repository = Repository::open(repo).unwrap();
    repository
        .references()
        .unwrap()
        .pack(&repository.objects(PackLimits::default()).unwrap(), options)
        .unwrap()
}

/// Git's view of every ref, read from a copy without the fixture's deliberately broken refs, which
/// make `for-each-ref` stop early.
fn refs_seen_by_git(repo: &Path) -> String {
    let scratch = tempfile::tempdir().unwrap();
    let copy = copy(scratch.path(), repo, "view");
    for broken in ["missing", "malformed"] {
        let _ = fs::remove_file(copy.join(".git/refs/heads").join(broken));
    }
    git(
        &copy,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname) %(*objectname)",
        ],
    )
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn pack_all_matches_git_pack_refs_all(#[case] format: ObjectFormat) {
    let (root, original) = fixture(format);
    let before = refs_seen_by_git(&original);
    let expected = copy(root.path(), &original, "git");
    let output = git_attempt(&expected, &["pack-refs", "--all"]);
    assert!(output.status.success());

    let report = pack(&original, PackRefsOptions::ALL);

    assert_eq!(
        fs::read(original.join(".git/packed-refs")).unwrap(),
        fs::read(expected.join(".git/packed-refs")).unwrap()
    );
    assert_eq!(loose_files(&original), loose_files(&expected));
    assert!(report.rewritten);
    assert_eq!(report.packed, report.pruned);
    assert!(report.retained.is_empty());
    assert_eq!(
        report.skipped,
        [name("refs/heads/malformed"), name("refs/heads/missing")]
    );
    assert_eq!(refs_seen_by_git(&original), before);
    assert!(!original.join(".git/packed-refs.lock").exists());
}

#[test]
fn pack_tags_matches_git_default_pack_refs() {
    let (root, original) = fixture(ObjectFormat::Sha1);
    let expected = copy(root.path(), &original, "git");
    git(&expected, &["pack-refs"]);

    pack(&original, PackRefsOptions::ALL.with_scope(PackScope::Tags));

    assert_eq!(
        fs::read(original.join(".git/packed-refs")).unwrap(),
        fs::read(expected.join(".git/packed-refs")).unwrap()
    );
    assert_eq!(loose_files(&original), loose_files(&expected));
}

#[test]
fn git_reads_updates_deletes_and_repacks_girt_output() {
    let (_root, repo) = fixture(ObjectFormat::Sha1);
    pack(&repo, PackRefsOptions::ALL);
    git(&repo, &["show-ref", "--verify", "refs/tags/nested"]);
    assert_eq!(
        git(&repo, &["rev-parse", "nested^{}"]),
        git(&repo, &["rev-parse", "HEAD~1"])
    );
    git(&repo, &["update-ref", "-d", "refs/tags/light"]);
    git(&repo, &["update-ref", "refs/heads/topic", "HEAD"]);
    fs::remove_file(repo.join(".git/refs/heads/missing")).unwrap();
    fs::remove_file(repo.join(".git/refs/heads/malformed")).unwrap();
    git(&repo, &["fsck", "--no-progress"]);
    git(&repo, &["pack-refs", "--all"]);

    let repository = Repository::open(&repo).unwrap();
    let refs = repository.references().unwrap();
    assert_eq!(refs.read(&name("refs/tags/light")).unwrap(), None);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(
        refs.read(&name("refs/heads/topic")).unwrap(),
        Some(Target::Direct(head.trim().parse().unwrap()))
    );
}

#[test]
fn girt_deletion_of_packed_ref_is_invisible_to_git() {
    let (_root, repo) = fixture(ObjectFormat::Sha1);
    pack(&repo, PackRefsOptions::ALL);
    let repository = Repository::open(&repo).unwrap();
    let refs = repository.references().unwrap();
    let id = refs.read(&name("refs/jj/keep/one")).unwrap().unwrap();
    refs.delete_without_reflog(&name("refs/jj/keep/one"), Expected::Value(id))
        .unwrap();
    assert!(!refs_seen_by_git(&repo).contains("refs/jj/keep/one"));
    assert!(
        !fs::read_to_string(repo.join(".git/packed-refs"))
            .unwrap()
            .contains("refs/jj/keep/one")
    );
}

#[test]
fn held_packed_lock_fails_without_effects() {
    let (_root, repo) = fixture(ObjectFormat::Sha1);
    let before = loose_files(&repo);
    let packed = fs::read(repo.join(".git/packed-refs")).unwrap();
    fs::write(repo.join(".git/packed-refs.lock"), "").unwrap();
    let repository = Repository::open(&repo).unwrap();
    let result = repository.references().unwrap().pack(
        &repository.objects(PackLimits::default()).unwrap(),
        PackRefsOptions::ALL,
    );
    assert!(matches!(
        result,
        Err(PackRefsError::Reference(ReferenceError::Locked(_)))
    ));
    assert_eq!(loose_files(&repo), before);
    assert_eq!(fs::read(repo.join(".git/packed-refs")).unwrap(), packed);
}

#[test]
fn locked_loose_ref_is_packed_but_retained() {
    let (_root, repo) = fixture(ObjectFormat::Sha1);
    let before = refs_seen_by_git(&repo);
    fs::write(repo.join(".git/refs/tags/light.lock"), "").unwrap();
    let report = pack(&repo, PackRefsOptions::ALL);
    assert_eq!(report.retained, [name("refs/tags/light")]);
    assert!(repo.join(".git/refs/tags/light").exists());
    fs::remove_file(repo.join(".git/refs/tags/light.lock")).unwrap();
    assert_eq!(refs_seen_by_git(&repo), before);
}

#[test]
fn auto_packs_only_past_threshold() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "repo"]);
    let repo = root.path().join("repo");
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
    // With the branch, 15 loose refs stay below Git's observed threshold of 16.
    for index in 1..15 {
        git(&repo, &["tag", &format!("t{index}")]);
    }
    let auto = PackRefsOptions::ALL.with_trigger(PackTrigger::Auto);
    assert!(!pack(&repo, auto).rewritten);
    assert!(!repo.join(".git/packed-refs").exists());
    git(&repo, &["tag", "t15"]);
    let report = pack(&repo, auto);
    assert!(report.rewritten);
    assert_eq!(report.pruned, 16);
}

#[test]
fn reftable_is_unsupported() {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &["init", "-q", "--ref-format=reftable", "repo"],
    );
    let repository = Repository::open(root.path().join("repo")).unwrap();
    let result = repository.references().unwrap().pack(
        &repository.objects(PackLimits::default()).unwrap(),
        PackRefsOptions::ALL,
    );
    assert!(matches!(
        result,
        Err(PackRefsError::Reference(ReferenceError::Unsupported(_)))
    ));
}

fn object_path(repo: &Path, id: &str) -> PathBuf {
    repo.join(".git/objects").join(&id[..2]).join(&id[2..])
}

/// Git keeps an existing `^` line when it can no longer read the objects behind it.
#[rstest]
#[case::missing_annotation("annotated")]
#[case::missing_nested_target("nested")]
fn existing_peel_survives_missing_objects_like_git(#[case] tag: &str) {
    let (root, original) = fixture(ObjectFormat::Sha1);
    for broken in ["missing", "malformed"] {
        fs::remove_file(original.join(".git/refs/heads").join(broken)).unwrap();
    }
    git(&original, &["pack-refs", "--all"]);
    // For `nested`, removing the inner annotation leaves the outer tag pointing at nothing.
    let annotated = git(&original, &["rev-parse", "refs/tags/annotated"]);
    let nested = git(&original, &["rev-parse", "refs/tags/nested"]);
    fs::remove_file(object_path(&original, annotated.trim())).unwrap();
    if tag == "annotated" {
        fs::remove_file(object_path(&original, nested.trim())).unwrap();
    }
    let expected = copy(root.path(), &original, "git");
    let output = git_attempt(&expected, &["pack-refs", "--all"]);
    assert!(output.status.success());

    pack(&original, PackRefsOptions::ALL);

    assert_eq!(
        String::from_utf8(fs::read(original.join(".git/packed-refs")).unwrap()).unwrap(),
        String::from_utf8(fs::read(expected.join(".git/packed-refs")).unwrap()).unwrap()
    );
}

/// A directory such as `refs/tags/a.` is not a valid full name, but the refs below it are.
#[rstest]
#[case::all(PackScope::All, &["pack-refs", "--all"][..])]
#[case::tags(PackScope::Tags, &["pack-refs"][..])]
fn dot_ending_directories_are_traversed_like_git(
    #[case] scope: PackScope,
    #[case] git_args: &[&str],
) {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "repo"]);
    let repo = root.path().join("repo");
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
    git(&repo, &["update-ref", "refs/tags/a./b", "HEAD"]);
    git(&repo, &["update-ref", "refs/heads/c./d", "HEAD"]);
    let expected = copy(root.path(), &repo, "git");
    git(&expected, git_args);

    let report = pack(&repo, PackRefsOptions::ALL.with_scope(scope));

    assert!(report.skipped.is_empty());
    assert_eq!(
        fs::read(repo.join(".git/packed-refs")).unwrap(),
        fs::read(expected.join(".git/packed-refs")).unwrap()
    );
    assert_eq!(loose_files(&repo), loose_files(&expected));
}
