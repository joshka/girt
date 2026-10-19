//! Controlled preparation refuses incomplete admission without changing repository files.
use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

use girt::fetch::{FetchPlanError, FetchPrepareLimits, FetchRequest};
use girt::refs::{Backend, RefName, Reflog};
use girt::remote::{Direction, Refspecs};
use girt::{InitKind, ObjectFormat, Repository};
use rstest::rstest;

fn prepare(repo: Repository, limits: FetchPrepareLimits, cancelled: bool) -> Result<FetchRequest, FetchPlanError> {
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
    let repo = Repository::init_with_backend(format, root.path().join("r.git"), InitKind::Bare, backend).unwrap();
    assert!(prepare(repo, FetchPrepareLimits::default(), false).is_ok());
}

#[test]
fn pre_cancelled_request_does_not_read_head() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, root.path().join("r.git"), InitKind::Bare).unwrap();
    std::fs::write(repo.git_dir().join("HEAD"), b"malformed").unwrap();
    assert!(matches!(prepare(repo, FetchPrepareLimits::default(), true), Err(FetchPlanError::Cancelled)));
}

#[test]
fn registration_is_a_typed_skip_before_opening_its_contents() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, root.path().join("r.git"), InitKind::Bare).unwrap();
    std::fs::create_dir_all(repo.common_dir().join("worktrees/broken")).unwrap();
    assert!(matches!(prepare(repo, FetchPrepareLimits::default(), false), Err(FetchPlanError::LinkedWorktrees)));
}

#[test]
fn head_bytes_are_bounded() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, root.path().join("r.git"), InitKind::Bare).unwrap();
    let mut limits = FetchPrepareLimits::default();
    limits.max_head_bytes = 1;
    assert!(matches!(prepare(repo, limits, false), Err(FetchPlanError::Reference(girt::refs::ReferenceError::Limit(_)))));
}

#[test]
fn head_may_not_escape_branch_namespace() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, root.path().join("r.git"), InitKind::Bare).unwrap();
    std::fs::write(repo.git_dir().join("HEAD"), b"ref: refs/remotes/origin/main\n").unwrap();
    assert!(matches!(prepare(repo, FetchPrepareLimits::default(), false), Err(FetchPlanError::Head(name)) if name == RefName::new("refs/remotes/origin/main").unwrap()));
}
