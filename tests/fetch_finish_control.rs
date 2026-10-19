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

#[test]
fn dependency_snapshot_is_bounded_before_install_and_default_thin_finish_succeeds() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (source, bytes, first) = bundle(format);
        let (_root, repo) = destination(format);
        // Exercise the unchanged ordinary wrapper for the initial complete installation.
        ready(&repo, &bytes)
            .finish(
                girt::fetch::FetchUpdateLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        let tree = oid(&git(
            source.path(),
            &["rev-parse", &format!("{first}^{{tree}}")],
            b"",
        ));
        let second = oid(&git(
            source.path(),
            &[
                "commit-tree",
                "--no-gpg-sign",
                &tree.to_string(),
                "-p",
                &first.to_string(),
            ],
            b"second\n",
        ));
        git(
            source.path(),
            &["update-ref", "refs/heads/main", &second.to_string()],
            b"",
        );
        git(
            source.path(),
            &[
                "bundle",
                "create",
                "incremental.bundle",
                "refs/heads/main",
                &format!("^{first}"),
            ],
            b"",
        );
        let bytes = fs::read(source.path().join("incremental.bundle")).unwrap();
        let bundle = Bundle::parse(&bytes).unwrap();
        let objects = repo.objects().unwrap();
        let known = KnownHistory::new(
            &objects,
            &[first],
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let receive = || {
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
                &bundle,
                &known,
                FetchLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap()
        };
        let before = pack_names(&repo);
        let failure = FetchFinishOptions::default()
            .with_limits(
                girt::fetch::FetchUpdateLimits::default()
                    .with_snapshot(girt::PackLimits::default().with_max_index_bytes(0)),
            )
            .finish(receive(), &AtomicBool::new(false))
            .unwrap_err();
        assert!(matches!(
            failure.source.as_ref(),
            FetchFinishFailure::Installation(girt::fetch::FetchError::Destination(_))
        ));
        assert!(failure.report.installed.is_none());
        assert_eq!(pack_names(&repo), before);
        assert_eq!(
            repo.references()
                .unwrap()
                .read(&RefName::new("refs/remotes/origin/main").unwrap())
                .unwrap(),
            Some(Target::Direct(first))
        );
        let report = FetchFinishOptions::default()
            .finish(receive(), &AtomicBool::new(false))
            .unwrap();
        assert!(report.installed.is_some());
        assert_eq!(
            repo.references()
                .unwrap()
                .read(&RefName::new("refs/remotes/origin/main").unwrap())
                .unwrap(),
            Some(Target::Direct(second))
        );
        git(repo.git_dir(), &["fsck", "--strict"], b"");
    }
}

#[test]
fn linked_caller_is_refused_even_when_legacy_preparation_admitted_it() {
    let (_source, bytes, first) = bundle(ObjectFormat::Sha1);
    let (root, repo) = destination(ObjectFormat::Sha1);
    ready(&repo, &bytes)
        .finish(
            girt::fetch::FetchUpdateLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let linked = root.path().join("linked");
    git(
        repo.git_dir(),
        &[
            "worktree",
            "add",
            "--detach",
            "--no-checkout",
            linked.to_str().unwrap(),
            "refs/remotes/origin/main",
        ],
        b"",
    );
    let received = FetchRequest::prepare(
        Repository::open(&linked).unwrap(),
        Refspecs::parse(Direction::Fetch, ["refs/heads/*:refs/remotes/origin/*"]).unwrap(),
        BTreeSet::new(),
        Reflog::Preserve,
    )
    .unwrap()
    .receive_bundle(
        &Bundle::parse(&bytes).unwrap(),
        &KnownHistory::default(),
        FetchLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let before = pack_names(&repo);
    let failure = FetchFinishOptions::default()
        .finish(received, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        failure.source.as_ref(),
        FetchFinishFailure::Safety(FetchPlanError::LinkedWorktrees)
    ));
    assert!(failure.report.installed.is_none());
    assert_eq!(pack_names(&repo), before);
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&RefName::new("refs/remotes/origin/main").unwrap())
            .unwrap(),
        Some(Target::Direct(first))
    );
}

#[test]
fn received_shallow_roots_are_admitted_before_install_and_then_publish_with_defaults() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (source, _bytes, first) = bundle(format);
        let tree = oid(&git(
            source.path(),
            &["rev-parse", &format!("{first}^{{tree}}")],
            b"",
        ));
        let commit = oid(&git(
            source.path(),
            &[
                "commit-tree",
                "--no-gpg-sign",
                &tree.to_string(),
                "-p",
                &first.to_string(),
            ],
            b"shallow tip\n",
        ));
        git(
            source.path(),
            &["update-ref", "refs/heads/main", &commit.to_string()],
            b"",
        );
        let (_root, repo) = destination(format);
        let cancel = AtomicBool::new(false);
        let receive = || {
            let request = FetchRequest::prepare_controlled(
                Repository::open(repo.git_dir()).unwrap(),
                Refspecs::parse(Direction::Fetch, ["refs/heads/*:refs/remotes/origin/*"]).unwrap(),
                BTreeSet::new(),
                Reflog::Preserve,
                FetchPrepareLimits::default(),
                &cancel,
            )
            .unwrap();
            request
                .with_depth(std::num::NonZeroU32::MIN)
                .receive_local(
                    source.path(),
                    &KnownHistory::default(),
                    FetchLimits::default(),
                    girt::transport::TransportControl::new(&cancel),
                    girt::fetch::LocalFetchObserver::default(),
                )
                .unwrap()
        };
        let before = pack_names(&repo);
        let mut preparation = FetchPrepareLimits::default();
        preparation.max_shallow_roots = 0;
        let failure = FetchFinishOptions::default()
            .with_admission_limits(FetchFinishLimits::default().with_preparation(preparation))
            .finish(receive(), &cancel)
            .unwrap_err();
        assert!(matches!(
            failure.source.as_ref(),
            FetchFinishFailure::Safety(FetchPlanError::PrepareLimit("received shallow roots"))
        ));
        assert!(failure.report.installed.is_none());
        assert!(!repo.common_dir().join("shallow").exists());
        assert!(!repo.common_dir().join("shallow.lock").exists());
        assert_no_effects(&repo, &before);
        let report = FetchFinishOptions::default()
            .finish(receive(), &cancel)
            .unwrap();
        assert!(report.shallow_published);
        let reopened = Repository::open(repo.git_dir()).unwrap();
        assert!(reopened.shallow_roots().contains(commit));
        git(repo.git_dir(), &["fsck", "--strict"], b"");
    }
}

#[test]
fn late_admission_failure_preserves_installed_pack_and_published_shallow_report() {
    let (source, _bytes, first) = bundle(ObjectFormat::Sha1);
    let tree = oid(&git(
        source.path(),
        &["rev-parse", &format!("{first}^{{tree}}")],
        b"",
    ));
    let commit = oid(&git(
        source.path(),
        &[
            "commit-tree",
            "--no-gpg-sign",
            &tree.to_string(),
            "-p",
            &first.to_string(),
        ],
        b"late failure tip\n",
    ));
    git(
        source.path(),
        &["update-ref", "refs/heads/main", &commit.to_string()],
        b"",
    );
    let (_root, repo) = destination(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let request = FetchRequest::prepare_controlled(
        Repository::open(repo.git_dir()).unwrap(),
        Refspecs::parse(Direction::Fetch, ["refs/heads/*:refs/remotes/origin/*"]).unwrap(),
        BTreeSet::new(),
        Reflog::Preserve,
        FetchPrepareLimits::default(),
        &cancel,
    )
    .unwrap();
    let received = request
        .with_depth(std::num::NonZeroU32::MIN)
        .receive_local(
            source.path(),
            &KnownHistory::default(),
            FetchLimits::default(),
            girt::transport::TransportControl::new(&cancel),
            girt::fetch::LocalFetchObserver::default(),
        )
        .unwrap();
    let before = pack_names(&repo);
    // No shallow file exists at initial admission. Its later publication exceeds this read budget.
    let failure = FetchFinishOptions::default()
        .with_admission_limits(
            FetchFinishLimits::default().with_open(OpenLimits::default().with_shallow_bytes(0)),
        )
        .finish(received, &cancel)
        .unwrap_err();
    assert!(matches!(
        failure.source.as_ref(),
        FetchFinishFailure::Reopen(_)
    ));
    assert!(failure.report.installed.is_some());
    assert!(failure.report.shallow_published);
    assert!(failure.report.references.is_empty());
    assert_ne!(pack_names(&repo), before);
    let reopened = Repository::open(repo.git_dir()).unwrap();
    assert!(reopened.shallow_roots().contains(commit));
    assert!(reopened.objects().unwrap().read(commit).unwrap().is_some());
    assert!(
        reopened
            .references()
            .unwrap()
            .read(&RefName::new("refs/remotes/origin/main").unwrap())
            .unwrap()
            .is_none()
    );
    git(repo.git_dir(), &["fsck", "--strict"], b"");
}

#[test]
fn controlled_files_transaction_limits_preserve_refs_after_installation() {
    use girt::refs::FilesTransactionLimits;
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (_source, bytes, _) = bundle(format);
        for limits in [
            FilesTransactionLimits::default().with_max_edits(0),
            FilesTransactionLimits::default().with_max_packed_refs(0),
            FilesTransactionLimits::default().with_max_input_bytes(0),
            FilesTransactionLimits::default().with_max_prepared_bytes(0),
        ] {
            let (_root, repo) = destination(format);
            let sentinel = repo
                .loose_objects()
                .write_blob(b"unrelated packed ref")
                .unwrap();
            git(
                repo.git_dir(),
                &["update-ref", "refs/keep/sentinel", &sentinel.to_string()],
                b"",
            );
            git(repo.git_dir(), &["pack-refs", "--all"], b"");
            let packed = fs::read(repo.common_dir().join("packed-refs")).unwrap();
            let failure = FetchFinishOptions::default()
                .with_admission_limits(FetchFinishLimits::default().with_files_transaction(limits))
                .finish(ready(&repo, &bytes), &AtomicBool::new(false))
                .unwrap_err();
            assert!(failure.report.installed.is_some());
            assert!(failure.report.references.is_empty());
            if limits.max_edits == 0 {
                assert!(matches!(
                    failure.source.as_ref(),
                    FetchFinishFailure::Safety(FetchPlanError::PrepareLimit("reference edits"))
                ));
            } else {
                assert!(matches!(
                    failure.source.as_ref(),
                    FetchFinishFailure::Publication(girt::refs::TransactionError::Prepare { .. })
                ));
            }
            assert!(
                repo.references()
                    .unwrap()
                    .read(&RefName::new("refs/remotes/origin/main").unwrap())
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                fs::read(repo.common_dir().join("packed-refs")).unwrap(),
                packed
            );
            assert_eq!(
                repo.references()
                    .unwrap()
                    .read(&RefName::new("refs/keep/sentinel").unwrap())
                    .unwrap(),
                Some(Target::Direct(sentinel))
            );
            assert!(!repo.common_dir().join("packed-refs.lock").exists());
            assert!(
                !repo
                    .common_dir()
                    .join("refs/remotes/origin/main.lock")
                    .exists()
            );
        }
    }
}

#[test]
fn controlled_finish_publishes_through_both_reference_backends() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (_source, bytes, commit) = bundle(format);
        for backend in [girt::refs::Backend::Files, girt::refs::Backend::Reftable] {
            let root = tempfile::tempdir().unwrap();
            let repo = Repository::init_with_backend(
                format,
                root.path().join("r.git"),
                InitKind::Bare,
                backend,
            )
            .unwrap();
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
            git(repo.git_dir(), &["fsck", "--strict"], b"");
        }
    }
}

#[test]
fn reference_backend_migration_after_preparation_refuses_before_installation() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (_source, bytes, _) = bundle(format);
        let (_root, repo) = destination(format);
        let received = ready(&repo, &bytes);
        git(
            repo.git_dir(),
            &["refs", "migrate", "--ref-format=reftable"],
            b"",
        );
        assert_eq!(
            Repository::open(repo.git_dir())
                .unwrap()
                .reference_backend(),
            girt::refs::Backend::Reftable
        );
        let before = pack_names(&repo);
        let failure = FetchFinishOptions::default()
            .finish(received, &AtomicBool::new(false))
            .unwrap_err();
        assert!(matches!(
            failure.source.as_ref(),
            FetchFinishFailure::BeforePublication(girt::fetch::FetchError::Unsupported(_))
        ));
        assert!(failure.report.installed.is_none());
        assert!(failure.report.references.is_empty());
        assert_no_effects(&repo, &before);
    }
}

#[test]
fn completed_controlled_prunes_survive_later_update_preparation_failure() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (_source, bytes, commit) = bundle(format);
        let (_root, repo) = destination(format);
        ready(&repo, &bytes)
            .finish(Default::default(), &AtomicBool::new(false))
            .unwrap();
        git(
            repo.git_dir(),
            &["update-ref", "-d", "refs/remotes/origin/main"],
            b"",
        );
        git(
            repo.git_dir(),
            &[
                "update-ref",
                "refs/remotes/origin/stale",
                &commit.to_string(),
            ],
            b"",
        );
        let received = FetchRequest::prepare_controlled(
            Repository::open(repo.git_dir()).unwrap(),
            Refspecs::parse(Direction::Fetch, ["refs/heads/*:refs/remotes/origin/*"]).unwrap(),
            BTreeSet::new(),
            Reflog::Preserve,
            FetchPrepareLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .with_prune()
        .receive_bundle(
            &Bundle::parse(&bytes).unwrap(),
            &KnownHistory::default(),
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let lock = repo.common_dir().join("refs/remotes/origin/main.lock");
        fs::write(&lock, b"foreign update lock").unwrap();
        let failure = FetchFinishOptions::default()
            .finish(received, &AtomicBool::new(false))
            .unwrap_err();
        assert!(failure.report.installed.is_some());
        assert!(
            matches!(failure.source.as_ref(), FetchFinishFailure::Publication(girt::refs::TransactionError::Publish { outcomes, .. }) if outcomes.len() == 1)
        );
        assert!(
            repo.references()
                .unwrap()
                .read(&RefName::new("refs/remotes/origin/stale").unwrap())
                .unwrap()
                .is_none()
        );
        assert!(
            repo.references()
                .unwrap()
                .read(&RefName::new("refs/remotes/origin/main").unwrap())
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read(lock).unwrap(), b"foreign update lock");
    }
}

#[test]
fn controlled_reftable_publication_charges_existing_and_new_records() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (_source, bytes, _) = bundle(format);
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init_with_backend(
            format,
            root.path().join("r.git"),
            InitKind::Bare,
            girt::refs::Backend::Reftable,
        )
        .unwrap();
        // The initial unborn HEAD record fits. Adding the remote ref would exceed the shared
        // existing-plus-new stack record budget, after the pack is already installed.
        let mut preparation = FetchPrepareLimits::default();
        preparation.reftable = preparation
            .reftable
            .with_records(girt::refs::reftable::Limits::default().with_records(1));
        let failure = FetchFinishOptions::default()
            .with_admission_limits(FetchFinishLimits::default().with_preparation(preparation))
            .finish(ready(&repo, &bytes), &AtomicBool::new(false))
            .unwrap_err();
        assert!(failure.report.installed.is_some());
        assert!(failure.report.references.is_empty());
        assert!(matches!(
            failure.source.as_ref(),
            FetchFinishFailure::Publication(girt::refs::TransactionError::Prepare { .. })
        ));
        assert!(
            repo.references()
                .unwrap()
                .read(&RefName::new("refs/remotes/origin/main").unwrap())
                .unwrap()
                .is_none()
        );
        git(repo.git_dir(), &["fsck", "--strict"], b"");
    }
}
