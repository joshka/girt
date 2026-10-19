//! Bounded opening preserves layout/configuration without publishing a partial handle.
use std::sync::atomic::AtomicBool;
use girt::refs::Backend;
use girt::{InitKind, ObjectFormat, OpenError, OpenLimits, OpenOptions, Repository};
use rstest::rstest;

#[rstest]
#[case(ObjectFormat::Sha1, Backend::Files, InitKind::Bare)]
#[case(ObjectFormat::Sha256, Backend::Files, InitKind::Bare)]
#[case(ObjectFormat::Sha1, Backend::Reftable, InitKind::Bare)]
#[case(ObjectFormat::Sha256, Backend::Reftable, InitKind::Bare)]
#[case(ObjectFormat::Sha1, Backend::Files, InitKind::Worktree)]
#[case(ObjectFormat::Sha256, Backend::Files, InitKind::Worktree)]
#[case(ObjectFormat::Sha1, Backend::Reftable, InitKind::Worktree)]
#[case(ObjectFormat::Sha256, Backend::Reftable, InitKind::Worktree)]
fn metadata_matches_legacy(#[case] format: ObjectFormat, #[case] backend: Backend, #[case] kind: InitKind) {
    let root = tempfile::tempdir().unwrap();
    let original = Repository::init_with_backend(format, root.path().join("repo"), kind, backend).unwrap();
    let opened = OpenOptions::default().open(root.path().join("repo"), &AtomicBool::new(false)).unwrap();
    assert_eq!(opened.git_dir(), original.git_dir());
    assert_eq!(opened.common_dir(), original.common_dir());
    assert_eq!(opened.worktree(), original.worktree());
    assert_eq!(opened.object_dir(), original.object_dir());
    assert_eq!(opened.object_format(), original.object_format());
    assert_eq!(opened.reference_backend(), original.reference_backend());
    assert_eq!(opened.config().entries(), original.config().entries());
}

#[test]
fn pre_cancelled_open_does_not_touch_the_path() {
    assert!(matches!(OpenOptions::default().open("does-not-exist", &AtomicBool::new(true)), Err(OpenError::Cancelled)));
}

#[test]
fn repeated_head_observations_share_one_budget() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, root.path().join("r.git"), InitKind::Bare).unwrap();
    let head = std::fs::read(repo.git_dir().join("HEAD")).unwrap();
    let exact = head.len() * 3;
    let limits = OpenLimits::default().with_metadata_bytes(exact);
    assert!(OpenOptions::default().with_limits(limits).open(repo.git_dir(), &AtomicBool::new(false)).is_ok());
    let limits = limits.with_metadata_bytes(exact - 1);
    assert!(matches!(OpenOptions::default().with_limits(limits).open(repo.git_dir(), &AtomicBool::new(false)), Err(OpenError::Limit(_))));
    assert_eq!(std::fs::read(repo.git_dir().join("HEAD")).unwrap(), head);
}

#[test]
fn oversized_head_is_refused_before_parsing() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, root.path().join("r.git"), InitKind::Bare).unwrap();
    std::fs::write(repo.git_dir().join("HEAD"), vec![b'x'; 128]).unwrap();
    let options = OpenOptions::default().with_limits(OpenLimits::default().with_metadata_bytes(16));
    assert!(matches!(options.open(repo.git_dir(), &AtomicBool::new(false)), Err(OpenError::Limit(_))));
}

#[test]
fn shallow_bytes_have_an_independent_limit() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, root.path().join("r.git"), InitKind::Bare).unwrap();
    std::fs::write(repo.git_dir().join("shallow"), b"1111111111111111111111111111111111111111\n").unwrap();
    let options = OpenOptions::default().with_limits(OpenLimits::default().with_shallow_bytes(1));
    assert!(matches!(options.open(repo.git_dir(), &AtomicBool::new(false)), Err(OpenError::Shallow(girt::ShallowError::Limit))));
}
