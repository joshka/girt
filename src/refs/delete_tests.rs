use rstest::rstest;

use super::*;
use crate::InitKind;

const OLD: &str = "1111111111111111111111111111111111111111";
const NEW: &str = "2222222222222222222222222222222222222222";
const KEEP: &str = "# pack-refs with: peeled fully-peeled sorted \n3333333333333333333333333333333333333333 refs/tags/keep\n^4444444444444444444444444444444444444444\n";

fn name(bytes: &[u8]) -> RefName {
    RefName::new(bytes).unwrap()
}
fn current() -> Expected {
    Expected::Value(Target::Direct(NEW.parse().unwrap()))
}
fn packed_bytes() -> String {
    format!(
        "# pack-refs with: peeled fully-peeled sorted \n{OLD} refs/heads/main\n3333333333333333333333333333333333333333 refs/tags/keep\n^4444444444444444444444444444444444444444\n"
    )
}
fn fixture() -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        crate::ObjectFormat::Sha1,
        root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    fs::create_dir_all(repo.git_dir().join("refs/heads")).unwrap();
    fs::write(repo.git_dir().join("refs/heads/main"), format!("{NEW}\n")).unwrap();
    fs::write(repo.git_dir().join("packed-refs"), packed_bytes()).unwrap();
    (root, repo)
}

#[test]
fn deletes_both_copies_and_preserves_unrelated_metadata() {
    let (_root, repo) = fixture();
    repo.references()
        .unwrap()
        .delete_without_reflog(&name(b"refs/heads/main"), current())
        .unwrap();
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name(b"refs/heads/main"))
            .unwrap(),
        None
    );
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        KEEP.as_bytes()
    );
    assert!(!repo.git_dir().join("refs/heads/main.lock").exists());
    assert!(!repo.git_dir().join("packed-refs.lock").exists());
}

#[rstest]
#[case::old_packed_value(Expected::Value(Target::Direct(OLD.parse().unwrap())))]
#[case::requires_absence(Expected::Absent)]
#[case::wrong_kind(Expected::Value(Target::Symbolic(name(b"refs/heads/main"))))]
fn mismatch_preserves_both_files(#[case] expected: Expected) {
    let (_root, repo) = fixture();
    assert!(matches!(
        repo.references()
            .unwrap()
            .delete_without_reflog(&name(b"refs/heads/main"), expected),
        Err(ReferenceError::Mismatch {
            actual: Some(Target::Direct(_))
        })
    ));
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        packed_bytes().as_bytes()
    );
    assert_eq!(
        fs::read(repo.git_dir().join("refs/heads/main")).unwrap(),
        format!("{NEW}\n").as_bytes()
    );
    assert!(!repo.git_dir().join("refs/heads/main.lock").exists());
    assert!(!repo.git_dir().join("packed-refs.lock").exists());
}

#[test]
fn deleting_symbolic_name_preserves_terminal() {
    let (_root, repo) = fixture();
    repo.references()
        .unwrap()
        .delete_without_reflog(
            &name(b"HEAD"),
            Expected::Value(Target::Symbolic(name(b"refs/heads/main"))),
        )
        .unwrap();
    assert!(!repo.git_dir().join("HEAD").exists());
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name(b"refs/heads/main"))
            .unwrap(),
        Some(Target::Direct(NEW.parse().unwrap()))
    );
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        packed_bytes().as_bytes()
    );
}

#[test]
fn deleting_resolved_branch_preserves_symbolic_head_and_reflogs() {
    let (_root, repo) = fixture();
    fs::create_dir_all(repo.git_dir().join("logs/refs/heads")).unwrap();
    fs::write(repo.git_dir().join("logs/refs/heads/main"), b"retained log").unwrap();
    assert_eq!(
        repo.references()
            .unwrap()
            .delete_resolved_without_reflog(&name(b"HEAD"), current())
            .unwrap(),
        name(b"refs/heads/main")
    );
    assert_eq!(
        fs::read(repo.git_dir().join("HEAD")).unwrap(),
        b"ref: refs/heads/main\n"
    );
    assert_eq!(
        fs::read(repo.git_dir().join("logs/refs/heads/main")).unwrap(),
        b"retained log"
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .resolve(&name(b"HEAD"), 1)
            .unwrap()
            .id,
        None
    );
}

#[rstest]
#[case::any(Expected::Any)]
#[case::absent(Expected::Absent)]
fn missing_delete_is_idempotent(#[case] expected: Expected) {
    let (_root, repo) = fixture();
    repo.references()
        .unwrap()
        .delete_without_reflog(&name(b"refs/heads/missing"), expected)
        .unwrap();
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        packed_bytes().as_bytes()
    );
}

#[rstest]
#[case::packed("packed-refs.lock")]
#[case::loose("refs/heads/main.lock")]
fn lock_contention_preserves_both_values_and_existing_lock(#[case] lock: &str) {
    let (_root, repo) = fixture();
    fs::write(repo.git_dir().join(lock), b"owner").unwrap();
    assert!(matches!(
        repo.references()
            .unwrap()
            .delete_without_reflog(&name(b"refs/heads/main"), current()),
        Err(ReferenceError::Locked(_))
    ));
    assert_eq!(fs::read(repo.git_dir().join(lock)).unwrap(), b"owner");
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        packed_bytes().as_bytes()
    );
    assert_eq!(
        fs::read(repo.git_dir().join("refs/heads/main")).unwrap(),
        format!("{NEW}\n").as_bytes()
    );
}

#[rstest]
#[case::loose("refs/heads/main")]
#[case::packed("packed-refs")]
fn malformed_data_is_unchanged(#[case] path: &str) {
    let (_root, repo) = fixture();
    fs::write(repo.git_dir().join(path), b"broken").unwrap();
    let packed_before = fs::read(repo.git_dir().join("packed-refs")).unwrap();
    let loose_before = fs::read(repo.git_dir().join("refs/heads/main")).unwrap();
    assert!(matches!(
        repo.references()
            .unwrap()
            .delete_without_reflog(&name(b"refs/heads/main"), Expected::Any),
        Err(ReferenceError::Malformed { .. })
    ));
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        packed_before
    );
    assert_eq!(
        fs::read(repo.git_dir().join("refs/heads/main")).unwrap(),
        loose_before
    );
}

#[test]
fn packed_only_deletion_removes_its_peel_line() {
    let (_root, repo) = fixture();
    repo.references()
        .unwrap()
        .delete_without_reflog(
            &name(b"refs/tags/keep"),
            Expected::Value(Target::Direct(ObjectId::Sha1([0x33; 20]))),
        )
        .unwrap();
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        format!("# pack-refs with: peeled fully-peeled sorted \n{OLD} refs/heads/main\n")
            .as_bytes()
    );
}

#[test]
fn packed_publication_retains_lock_and_current_loose_value() {
    let (_root, repo) = fixture();
    let lock = Lock::acquire(repo.git_dir().join("packed-refs")).unwrap();
    lock.publish_retaining_lock(KEEP.as_bytes()).unwrap();
    assert!(matches!(
        Lock::acquire(repo.git_dir().join("packed-refs")),
        Err(ReferenceError::Locked(_))
    ));
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name(b"refs/heads/main"))
            .unwrap(),
        Some(Target::Direct(NEW.parse().unwrap()))
    );
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        KEEP.as_bytes()
    );
}

#[test]
fn failed_packed_publication_preserves_loose_and_cleans_temporary_file() {
    let (_root, repo) = fixture();
    let mut lock = Lock::acquire(repo.git_dir().join("packed-refs")).unwrap();
    let loose_lock = Lock::acquire(repo.git_dir().join("refs/heads/main")).unwrap();
    let bytes = packed_bytes();
    let packed = packed::parse(bytes.as_bytes(), &lock.destination).unwrap();
    // A directory destination injects a rename failure without permission/root assumptions.
    lock.destination = repo.git_dir().join("refs");
    assert!(
        delete_locked(
            &lock,
            &loose_lock,
            &name(b"refs/heads/main"),
            bytes.as_bytes(),
            &packed
        )
        .is_err()
    );
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        packed_bytes().as_bytes()
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name(b"refs/heads/main"))
            .unwrap(),
        Some(Target::Direct(NEW.parse().unwrap()))
    );
    assert!(!fs::read_dir(repo.git_dir()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".girt-packed-")
    }));
}

#[test]
fn failed_unlink_after_packed_publication_reports_partial_completion() {
    let (_root, repo) = fixture();
    let lock = Lock::acquire(repo.git_dir().join("packed-refs")).unwrap();
    let mut loose_lock = Lock::acquire(repo.git_dir().join("refs/heads/main")).unwrap();
    let bytes = packed_bytes();
    let packed = packed::parse(bytes.as_bytes(), &lock.destination).unwrap();
    // Substitute a directory at the unlink boundary without changing the actual loose value.
    // The full deletion path must publish packed removal before returning this failure.
    loose_lock.destination = repo.git_dir().join("refs/heads");
    assert!(matches!(
        delete_locked(
            &lock,
            &loose_lock,
            &name(b"refs/heads/main"),
            bytes.as_bytes(),
            &packed
        ),
        Err(ReferenceError::PackedDeleted { .. })
    ));
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name(b"refs/heads/main"))
            .unwrap(),
        Some(Target::Direct(NEW.parse().unwrap()))
    );
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        KEEP.as_bytes()
    );
}

#[test]
fn resolved_cycle_does_not_mutate_refs() {
    let (_root, repo) = fixture();
    fs::write(repo.git_dir().join("refs/heads/main"), b"ref: HEAD\n").unwrap();
    assert!(matches!(
        repo.references()
            .unwrap()
            .delete_resolved_without_reflog(&name(b"HEAD"), Expected::Any),
        Err(ReferenceError::Cycle(_))
    ));
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        packed_bytes().as_bytes()
    );
    assert_eq!(
        fs::read(repo.git_dir().join("refs/heads/main")).unwrap(),
        b"ref: HEAD\n"
    );
    assert!(!repo.git_dir().join("HEAD.lock").exists());
    assert!(!repo.git_dir().join("refs/heads/main.lock").exists());
}

#[test]
fn loose_only_deletion_does_not_create_packed_storage() {
    let (_root, repo) = fixture();
    fs::remove_file(repo.git_dir().join("packed-refs")).unwrap();
    repo.references()
        .unwrap()
        .delete_without_reflog(&name(b"refs/heads/main"), current())
        .unwrap();
    assert!(!repo.git_dir().join("packed-refs").exists());
    assert!(!repo.git_dir().join("refs/heads/main").exists());
}

#[test]
fn symbolic_loose_shadow_is_removed_with_its_packed_copy() {
    let (_root, repo) = fixture();
    fs::write(
        repo.git_dir().join("refs/heads/main"),
        b"ref: refs/tags/keep\n",
    )
    .unwrap();
    repo.references()
        .unwrap()
        .delete_without_reflog(
            &name(b"refs/heads/main"),
            Expected::Value(Target::Symbolic(name(b"refs/tags/keep"))),
        )
        .unwrap();
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name(b"refs/heads/main"))
            .unwrap(),
        None
    );
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        KEEP.as_bytes()
    );
}

#[test]
fn missing_value_expectation_fails_without_creating_a_ref() {
    let (_root, repo) = fixture();
    assert!(matches!(
        repo.references()
            .unwrap()
            .delete_without_reflog(&name(b"refs/heads/missing"), current()),
        Err(ReferenceError::Mismatch { actual: None })
    ));
    assert!(!repo.git_dir().join("refs/heads/missing").exists());
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        packed_bytes().as_bytes()
    );
}

#[test]
fn resolved_locked_terminal_preserves_head_and_packed_file() {
    let (_root, repo) = fixture();
    fs::write(repo.git_dir().join("refs/heads/main.lock"), b"owner").unwrap();
    assert!(matches!(
        repo.references()
            .unwrap()
            .delete_resolved_without_reflog(&name(b"HEAD"), current()),
        Err(ReferenceError::Locked(_))
    ));
    assert_eq!(
        fs::read(repo.git_dir().join("HEAD")).unwrap(),
        b"ref: refs/heads/main\n"
    );
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        packed_bytes().as_bytes()
    );
    assert!(!repo.git_dir().join("HEAD.lock").exists());
}

#[test]
fn resolved_mismatch_preserves_every_name() {
    let (_root, repo) = fixture();
    assert!(matches!(
        repo.references()
            .unwrap()
            .delete_resolved_without_reflog(&name(b"HEAD"), Expected::Absent),
        Err(ReferenceError::Mismatch { .. })
    ));
    assert_eq!(
        fs::read(repo.git_dir().join("HEAD")).unwrap(),
        b"ref: refs/heads/main\n"
    );
    assert_eq!(
        fs::read(repo.git_dir().join("refs/heads/main")).unwrap(),
        format!("{NEW}\n").as_bytes()
    );
    assert_eq!(
        fs::read(repo.git_dir().join("packed-refs")).unwrap(),
        packed_bytes().as_bytes()
    );
}
