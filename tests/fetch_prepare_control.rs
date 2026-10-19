//! Controlled preparation refuses incomplete admission without changing repository files.
use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

use girt::fetch::{FetchPlanError, FetchPrepareLimits, FetchRequest};
use girt::refs::{Backend, RefName, Reflog};
use girt::remote::{Direction, Refspecs};
use girt::{InitKind, ObjectFormat, Repository};
use rstest::rstest;

fn prepare(
    repo: Repository,
    limits: FetchPrepareLimits,
    cancelled: bool,
) -> Result<FetchRequest, FetchPlanError> {
    FetchRequest::prepare_controlled(
        repo,
        Refspecs::parse(Direction::Fetch, ["refs/heads/*:refs/remotes/origin/*"]).unwrap(),
        BTreeSet::new(),
        Reflog::Preserve,
        limits,
        &AtomicBool::new(cancelled),
    )
}

#[rstest]
#[case(ObjectFormat::Sha1, Backend::Files)]
#[case(ObjectFormat::Sha256, Backend::Files)]
#[case(ObjectFormat::Sha1, Backend::Reftable)]
#[case(ObjectFormat::Sha256, Backend::Reftable)]
fn empty_repository_is_admitted(#[case] format: ObjectFormat, #[case] backend: Backend) {
    let root = tempfile::tempdir().unwrap();
    let repo =
        Repository::init_with_backend(format, root.path().join("r.git"), InitKind::Bare, backend)
            .unwrap();
    assert!(prepare(repo, FetchPrepareLimits::default(), false).is_ok());
}

#[test]
fn pre_cancelled_request_does_not_read_head() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    std::fs::write(repo.git_dir().join("HEAD"), b"malformed").unwrap();
    assert!(matches!(
        prepare(repo, FetchPrepareLimits::default(), true),
        Err(FetchPlanError::Cancelled)
    ));
}

#[test]
fn registration_is_a_typed_skip_before_opening_its_contents() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    std::fs::create_dir_all(repo.common_dir().join("worktrees/broken")).unwrap();
    assert!(matches!(
        prepare(repo, FetchPrepareLimits::default(), false),
        Err(FetchPlanError::LinkedWorktrees)
    ));
}

#[test]
fn head_bytes_are_bounded() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    let mut limits = FetchPrepareLimits::default();
    limits.max_head_bytes = 1;
    assert!(matches!(
        prepare(repo, limits, false),
        Err(FetchPlanError::Reference(
            girt::refs::ReferenceError::Limit(_)
        ))
    ));
}

#[test]
fn head_may_not_escape_branch_namespace() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    std::fs::write(
        repo.git_dir().join("HEAD"),
        b"ref: refs/remotes/origin/main\n",
    )
    .unwrap();
    assert!(
        matches!(prepare(repo, FetchPrepareLimits::default(), false), Err(FetchPlanError::Head(name)) if name == RefName::new("refs/remotes/origin/main").unwrap())
    );
}

#[rstest]
#[case::entries(true)]
#[case::bytes(false)]
fn inventory_limits_refuse_without_a_partial_request(#[case] entries: bool) {
    use girt::refs::{Expected, Target};
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    let id = repo.loose_objects().write_blob(b"tip").unwrap();
    repo.references()
        .unwrap()
        .update_without_reflog(
            &RefName::new("refs/remotes/origin/main").unwrap(),
            Target::Direct(id),
            Expected::Absent,
        )
        .unwrap();
    let mut limits = FetchPrepareLimits::default();
    if entries {
        limits.max_ref_entries = 0;
    } else {
        limits.max_ref_bytes = 0;
    }
    assert!(matches!(
        prepare(repo, limits, false),
        Err(FetchPlanError::Reference(
            girt::refs::ReferenceError::Limit(_)
        ))
    ));
}

#[test]
fn shallow_root_copy_has_a_separate_budget() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    std::fs::write(
        repo.git_dir().join("shallow"),
        b"1111111111111111111111111111111111111111\n",
    )
    .unwrap();
    let repo = Repository::open(repo.git_dir()).unwrap();
    let mut limits = FetchPrepareLimits::default();
    limits.max_shallow_roots = 0;
    assert!(matches!(
        prepare(repo, limits, false),
        Err(FetchPlanError::PrepareLimit("shallow roots"))
    ));
}

#[test]
fn cyclic_head_chain_is_not_admitted() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    std::fs::write(repo.git_dir().join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    std::fs::create_dir_all(repo.git_dir().join("refs/heads")).unwrap();
    std::fs::write(
        repo.git_dir().join("refs/heads/main"),
        b"ref: refs/heads/main\n",
    )
    .unwrap();
    assert!(matches!(
        prepare(repo, FetchPrepareLimits::default(), false),
        Err(FetchPlanError::Reference(
            girt::refs::ReferenceError::Depth(32)
        ))
    ));
}

#[path = "support/maintenance_git.rs"]
mod oracle;

#[test]
fn linked_caller_is_a_typed_skip() {
    let root = tempfile::tempdir().unwrap();
    oracle::git(root.path(), &["init", "-q", "main"], b"");
    let main = root.path().join("main");
    oracle::git(
        &main,
        &[
            "-c",
            "maintenance.auto=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "initial",
        ],
        b"",
    );
    oracle::git(&main, &["worktree", "add", "-q", "../linked"], b"");
    let linked = Repository::open(root.path().join("linked")).unwrap();
    assert!(matches!(
        prepare(linked, FetchPrepareLimits::default(), false),
        Err(FetchPlanError::LinkedWorktrees)
    ));
}

#[rstest]
#[case(ObjectFormat::Sha1, Backend::Files)]
#[case(ObjectFormat::Sha256, Backend::Files)]
#[case(ObjectFormat::Sha1, Backend::Reftable)]
#[case(ObjectFormat::Sha256, Backend::Reftable)]
fn preview_matches_legacy_and_keeps_refs_unchanged(
    #[case] format: ObjectFormat,
    #[case] backend: Backend,
) {
    use girt::fetch::{AdvertisedRef, Advertisement, AdvertisementKind};
    use girt::refs::{Expected, Target};
    let root = tempfile::tempdir().unwrap();
    let repo =
        Repository::init_with_backend(format, root.path().join("r.git"), InitKind::Bare, backend)
            .unwrap();
    let id = repo.loose_objects().write_blob(b"tip").unwrap();
    let name = RefName::new("refs/remotes/origin/main").unwrap();
    repo.references()
        .unwrap()
        .update_without_reflog(&name, Target::Direct(id), Expected::Absent)
        .unwrap();
    let before = repo.references().unwrap().list().unwrap();
    let controlled = prepare(repo.clone(), FetchPrepareLimits::default(), false).unwrap();
    let legacy = FetchRequest::prepare(
        repo.clone(),
        Refspecs::parse(Direction::Fetch, ["refs/heads/*:refs/remotes/origin/*"]).unwrap(),
        BTreeSet::new(),
        Reflog::Preserve,
    )
    .unwrap();
    let advertisement = Advertisement::new(
        vec![AdvertisedRef::new(
            RefName::new("refs/heads/main").unwrap(),
            id,
            AdvertisementKind::Tip,
        )],
        Vec::new(),
    );
    assert_eq!(
        format!("{:?}", controlled.plan(&advertisement).unwrap()),
        format!("{:?}", legacy.plan(&advertisement).unwrap())
    );
    assert_eq!(repo.references().unwrap().list().unwrap(), before);
    oracle::git(
        root.path(),
        &[
            "--git-dir",
            repo.git_dir().to_str().unwrap(),
            "fsck",
            "--strict",
        ],
        b"",
    );
}
