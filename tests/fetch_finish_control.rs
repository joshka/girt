//! Controlled completion refuses admission before installing a bundle or publishing references.
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use girt::bundle::Bundle;
use girt::fetch::{
    FetchFinishFailure, FetchFinishLimits, FetchFinishOptions, FetchLimits, FetchPlanError,
    FetchPrepareLimits, FetchReady, FetchRequest, KnownHistory,
};
use girt::refs::{RefName, Reflog, Target};
use girt::remote::{Direction, Refspecs};
use girt::{InitKind, ObjectFormat, ObjectId, OpenLimits, Repository};

#[path = "support/maintenance_git.rs"]
mod oracle;

fn git(root: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut isolated = vec!["-c", "maintenance.auto=false", "-c", "gc.auto=0"];
    isolated.extend_from_slice(args);
    oracle::git(root, &isolated, input)
}

fn oid(bytes: &[u8]) -> ObjectId {
    std::str::from_utf8(bytes).unwrap().trim().parse().unwrap()
}

fn bundle(format: ObjectFormat) -> (tempfile::TempDir, Vec<u8>, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--template=",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
    let blob = oid(&git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"finish fixture\n",
    ));
    let tree = oid(&git(
        root.path(),
        &["mktree"],
        format!("100644 blob {blob}\tfile\n").as_bytes(),
    ));
    let commit = oid(&git(
        root.path(),
        &["commit-tree", "--no-gpg-sign", &tree.to_string()],
        b"fixture\n",
    ));
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &commit.to_string()],
        b"",
    );
    git(
        root.path(),
        &["bundle", "create", "fixture.bundle", "refs/heads/main"],
        b"",
    );
    let bytes = fs::read(root.path().join("fixture.bundle")).unwrap();
    (root, bytes, commit)
}

fn destination(format: ObjectFormat) -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("r.git"), InitKind::Bare).unwrap();
    (root, repo)
}

fn ready(repo: &Repository, bytes: &[u8]) -> FetchReady {
    FetchRequest::prepare_controlled(
        Repository::open(repo.git_dir()).unwrap(),
        Refspecs::parse(Direction::Fetch, ["refs/heads/*:refs/remotes/origin/*"]).unwrap(),
        BTreeSet::new(),
        Reflog::Preserve,
        FetchPrepareLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap()
    .receive_bundle(
        &Bundle::parse(bytes).unwrap(),
        &KnownHistory::default(),
        FetchLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap()
}

fn pack_names(repo: &Repository) -> BTreeSet<std::ffi::OsString> {
    match fs::read_dir(repo.object_dir().join("pack")) {
        Ok(entries) => entries.map(|entry| entry.unwrap().file_name()).collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => panic!("pack inventory: {error}"),
    }
}

fn assert_no_effects(repo: &Repository, before: &BTreeSet<std::ffi::OsString>) {
    assert_eq!(&pack_names(repo), before);
    assert!(
        repo.references()
            .unwrap()
            .read(&RefName::new("refs/remotes/origin/main").unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn admission_budgets_refuse_before_install_or_reference_publication() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (_source, bytes, _) = bundle(format);
        for open in [
            OpenLimits::default().with_metadata_bytes(0),
            OpenLimits::default()
                .with_configuration(girt::config::ResolveLimits::default().with_bytes(0)),
        ] {
            let (_root, repo) = destination(format);
            let received = ready(&repo, &bytes);
            let before = pack_names(&repo);
            let failure = FetchFinishOptions::default()
                .with_admission_limits(FetchFinishLimits::default().with_open(open))
                .finish(received, &AtomicBool::new(false))
                .unwrap_err();
            assert!(matches!(
                failure.source.as_ref(),
                FetchFinishFailure::Reopen(_)
            ));
            assert!(failure.report.installed.is_none());
            assert!(failure.report.references.is_empty());
            assert_no_effects(&repo, &before);
        }
    }
}

#[test]
fn newly_registered_worktree_is_a_typed_refusal_before_install() {
    let (_source, bytes, _) = bundle(ObjectFormat::Sha1);
    let (_root, repo) = destination(ObjectFormat::Sha1);
    let received = ready(&repo, &bytes);
    fs::create_dir_all(repo.common_dir().join("worktrees/registered")).unwrap();
    let before = pack_names(&repo);
    let failure = FetchFinishOptions::default()
        .finish(received, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        failure.source.as_ref(),
        FetchFinishFailure::Safety(FetchPlanError::LinkedWorktrees)
    ));
    assert!(failure.report.installed.is_none());
    assert_no_effects(&repo, &before);
}

#[test]
fn pre_cancelled_finish_leaves_objects_and_refs_unchanged() {
    let (_source, bytes, _) = bundle(ObjectFormat::Sha1);
    let (_root, repo) = destination(ObjectFormat::Sha1);
    let received = ready(&repo, &bytes);
    let before = pack_names(&repo);
    let failure = FetchFinishOptions::default()
        .finish(received, &AtomicBool::new(true))
        .unwrap_err();
    assert!(matches!(
        failure.source.as_ref(),
        FetchFinishFailure::BeforePublication(girt::fetch::FetchError::Cancelled)
    ));
    assert!(failure.report.installed.is_none());
    assert!(failure.report.references.is_empty());
    assert_no_effects(&repo, &before);
}

#[test]
fn default_controlled_finish_installs_and_publishes_in_both_formats() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (_source, bytes, commit) = bundle(format);
        let (_root, repo) = destination(format);
        let report = FetchFinishOptions::default()
            .finish(ready(&repo, &bytes), &AtomicBool::new(false))
            .unwrap();
        assert!(report.installed.is_some());
        assert_eq!(
            repo.references()
                .unwrap()
                .read(&RefName::new("refs/remotes/origin/main").unwrap())
                .unwrap(),
            Some(Target::Direct(commit))
        );
        assert!(repo.objects().unwrap().read(commit).unwrap().is_some());
        git(repo.git_dir(), &["fsck", "--strict"], b"");
    }
}

#[test]
fn current_reference_inventory_is_bounded_before_installation() {
    let (_source, bytes, _) = bundle(ObjectFormat::Sha1);
    let (_root, repo) = destination(ObjectFormat::Sha1);
    let received = ready(&repo, &bytes);
    let blob = oid(&git(
        repo.git_dir(),
        &["hash-object", "-w", "--stdin"],
        b"existing\n",
    ));
    git(
        repo.git_dir(),
        &["update-ref", "refs/tags/existing", &blob.to_string()],
        b"",
    );
    let before = pack_names(&repo);
    let mut preparation = FetchPrepareLimits::default();
    preparation.max_ref_entries = 0;
    let failure = FetchFinishOptions::default()
        .with_admission_limits(FetchFinishLimits::default().with_preparation(preparation))
        .finish(received, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        failure.source.as_ref(),
        FetchFinishFailure::Safety(FetchPlanError::Reference(_))
    ));
    assert!(failure.report.installed.is_none());
    assert_no_effects(&repo, &before);
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&RefName::new("refs/tags/existing").unwrap())
            .unwrap(),
        Some(Target::Direct(blob))
    );
}
