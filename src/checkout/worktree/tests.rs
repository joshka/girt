use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use rstest::rstest;

use super::*;
use crate::{InitKind, Tree, TreeEntry};

struct Fixture {
    _temp: tempfile::TempDir,
    repo: Repository,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::init(temp.path().join("repo"), InitKind::Worktree).unwrap();
        Self { _temp: temp, repo }
    }
    fn root(&self) -> &Path {
        self.repo.worktree().unwrap()
    }
    fn tree(&self, name: &[u8], mode: EntryMode, bytes: &[u8]) -> ObjectId {
        let store = self.repo.loose_objects().unwrap();
        let id = store.write_blob(bytes).unwrap();
        self.entry_tree(name, mode, id)
    }
    fn entry_tree(&self, path: &[u8], mode: EntryMode, id: ObjectId) -> ObjectId {
        let store = self.repo.loose_objects().unwrap();
        let (first, rest) = path
            .iter()
            .position(|b| *b == b'/')
            .map_or((path, None), |n| (&path[..n], Some(&path[n + 1..])));
        let (mode, id) = match rest {
            Some(rest) => (EntryMode::Tree, self.entry_tree(rest, mode, id)),
            None => (mode, id),
        };
        store
            .write_tree(
                &Tree::new(vec![TreeEntry {
                    name: first.to_vec(),
                    mode,
                    id,
                }])
                .unwrap(),
            )
            .unwrap()
    }
    fn checkout(&self, old: Option<ObjectId>, new: Option<ObjectId>) -> Result<Report, Failure> {
        self.repo
            .checkout_tree(old, new, Limits::default(), &AtomicBool::new(false))
    }
    fn initial(&self, name: &[u8], mode: EntryMode, bytes: &[u8]) -> ObjectId {
        let tree = self.tree(name, mode, bytes);
        self.checkout(None, Some(tree)).unwrap();
        tree
    }
    fn index_bytes(&self) -> Vec<u8> {
        fs::read(self.repo.git_dir().join("index")).unwrap()
    }
}

#[rstest]
#[case::regular(EntryMode::Blob, EntryMode::Blob, b"new")]
#[case::executable(EntryMode::Blob, EntryMode::Executable, b"new")]
#[case::link(EntryMode::Blob, EntryMode::Symlink, b"destination")]
#[case::link_to_file(EntryMode::Symlink, EntryMode::Blob, b"new")]
#[case::changed_link(EntryMode::Symlink, EntryMode::Symlink, b"new")]
fn updates_raw_content_and_modes(
    #[case] old_mode: EntryMode,
    #[case] mode: EntryMode,
    #[case] bytes: &[u8],
) {
    let f = Fixture::new();
    let old = f.initial(b"file", old_mode, b"old");
    let target = f.tree(b"file", mode, bytes);
    let report = f.checkout(Some(old), Some(target)).unwrap();
    assert!(report.index_published);
    assert_eq!(
        report.applied,
        vec![op(b"file", Action::Remove), op(b"file", Action::Install)]
    );
    let status = f
        .repo
        .raw_status(
            crate::status::Baseline::Tree(Some(target)),
            crate::status::Untracked::Omit,
            crate::status::Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(status.unstaged.is_empty());
    assert!(status.staged.is_empty());
}

#[rstest]
#[case::file_to_directory(b"a", b"a/b")]
#[case::directory_to_file(b"a/b", b"a")]
#[case::nested_directory_to_file(b"a/b/c", b"a")]
fn tracked_file_directory_transitions(#[case] old_path: &[u8], #[case] new_path: &[u8]) {
    let f = Fixture::new();
    let old = f.initial(old_path, EntryMode::Blob, b"old");
    let new = f.tree(new_path, EntryMode::Blob, b"new");
    f.checkout(Some(old), Some(new)).unwrap();
    assert_eq!(
        fs::read(f.root().join(OsStr::from_bytes(new_path))).unwrap(),
        b"new"
    );
}

#[test]
fn deletes_only_tracked_content_and_retains_unrelated_files() {
    let f = Fixture::new();
    let old = f.initial(b"dir/file", EntryMode::Blob, b"old");
    fs::write(f.root().join("dir/keep"), b"user").unwrap();
    f.checkout(Some(old), None).unwrap();
    assert_eq!(fs::read(f.root().join("dir/keep")).unwrap(), b"user");
    assert!(!f.root().join("dir/file").exists());
    assert!(
        f.repo
            .read_index(Default::default())
            .unwrap()
            .unwrap()
            .entries()
            .is_empty()
    );
}

#[rstest]
#[case::content(false)]
#[case::assume_valid(true)]
fn refuses_dirty_files_before_any_mutation(#[case] assume: bool) {
    let f = Fixture::new();
    let old = f.initial(b"file", EntryMode::Blob, b"old");
    let mut edit = f.repo.edit_index(Default::default()).unwrap();
    let mut entries = edit.index().entries().to_vec();
    entries[0].assume_valid = assume;
    edit.replace_entries(entries).unwrap();
    edit.commit().unwrap();
    let index = f.index_bytes();
    fs::write(f.root().join("file"), b"user").unwrap();
    let error = f.checkout(Some(old), None).unwrap_err();
    assert_eq!(error.report.stage, Stage::Preparation);
    assert!(error.report.applied.is_empty());
    assert_eq!(f.index_bytes(), index);
    assert_eq!(fs::read(f.root().join("file")).unwrap(), b"user");
}

#[rstest]
#[case::staged(index::Stage::Normal)]
#[case::conflict(index::Stage::Ours)]
fn preserves_staged_changes_and_conflicts(#[case] stage: index::Stage) {
    let f = Fixture::new();
    let old = f.initial(b"file", EntryMode::Blob, b"old");
    let mut edit = f.repo.edit_index(Default::default()).unwrap();
    let mut entries = edit.index().entries().to_vec();
    entries[0].id = f
        .repo
        .loose_objects()
        .unwrap()
        .write_blob(b"staged")
        .unwrap();
    entries[0].stage = stage;
    edit.replace_entries(entries).unwrap();
    edit.commit().unwrap();
    let index = f.index_bytes();
    let error = f.checkout(Some(old), None).unwrap_err();
    assert!(error.report.applied.is_empty());
    assert_eq!(f.index_bytes(), index);
    assert_eq!(fs::read(f.root().join("file")).unwrap(), b"old");
}

#[rstest]
#[case::leaf(b"file", "file")]
#[case::ancestor(b"file/child", "file")]
#[case::case_alias(b"FILE", "file")]
fn refuses_untracked_obstructions(#[case] path: &[u8], #[case] obstruction: &str) {
    let f = Fixture::new();
    let target = f.tree(path, EntryMode::Blob, b"new");
    fs::write(f.root().join(obstruction), b"user").unwrap();
    let failure = f.checkout(None, Some(target)).unwrap_err();
    assert!(failure.report.applied.is_empty());
    assert_eq!(fs::read(f.root().join(obstruction)).unwrap(), b"user");
    assert!(!f.repo.git_dir().join("index").exists());
}

#[rstest]
#[case::file(false)]
#[case::empty_directory(true)]
fn directory_transition_refuses_unknown_contents(#[case] directory: bool) {
    let f = Fixture::new();
    let old = f.initial(b"dir/file", EntryMode::Blob, b"old");
    let target = f.tree(b"dir", EntryMode::Blob, b"new");
    create_obstruction(&f.root().join("dir/user"), directory);
    let failure = f.checkout(Some(old), Some(target)).unwrap_err();
    assert!(failure.report.applied.is_empty());
    assert_eq!(fs::read(f.root().join("dir/file")).unwrap(), b"old");
    assert!(f.root().join("dir/user").exists());
}
fn create_obstruction(path: &Path, directory: bool) {
    if directory {
        fs::create_dir(path).unwrap();
    } else {
        fs::write(path, b"user").unwrap();
    }
}

#[rstest]
#[case::gitlink(b"sub", EntryMode::Gitlink)]
#[case::metadata_alias(b".GIT/config", EntryMode::Blob)]
#[case::backslash(b"a\\b", EntryMode::Blob)]
#[case::colon(b"a:b", EntryMode::Blob)]
fn rejects_unsupported_tree_paths_before_mutation(#[case] path: &[u8], #[case] mode: EntryMode) {
    let f = Fixture::new();
    let head = fs::read(f.repo.git_dir().join("HEAD")).unwrap();
    let tree = f.tree(path, mode, b"bad");
    let error = f.checkout(None, Some(tree)).unwrap_err();
    assert!(error.report.applied.is_empty());
    assert_eq!(fs::read(f.repo.git_dir().join("HEAD")).unwrap(), head);
    assert!(!f.repo.git_dir().join("index.lock").exists());
}

#[test]
fn rejects_nested_repository() {
    let f = Fixture::new();
    let nested = Repository::init(f.root().join("nested"), InitKind::Worktree).unwrap();
    let target = f.tree(b"nested/file", EntryMode::Blob, b"new");
    let head = fs::read(nested.git_dir().join("HEAD")).unwrap();
    assert!(f.checkout(None, Some(target)).is_err());
    assert_eq!(fs::read(nested.git_dir().join("HEAD")).unwrap(), head);
    assert!(!f.root().join("nested/file").exists());
}

#[rstest]
#[case::after_plan("prepared")]
#[case::at_mutation("before delete")]
fn changed_file_is_preserved_at_mutation_boundaries(#[case] checkpoint: &str) {
    let f = Fixture::new();
    let old = f.initial(b"file", EntryMode::Blob, b"old");
    let index = f.index_bytes();
    let result = run(
        &f.repo,
        Some(old),
        None,
        Limits::default(),
        &AtomicBool::new(false),
        &mut |event, _| {
            if event == checkpoint {
                fs::write(f.root().join("file"), b"user").unwrap();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(result.report.applied.is_empty());
    assert_eq!(fs::read(f.root().join("file")).unwrap(), b"user");
    assert_eq!(f.index_bytes(), index);
}

#[rstest]
#[case::symlink(true)]
#[case::directory(false)]
fn substituted_ancestor_never_receives_writes(#[case] link: bool) {
    let f = Fixture::new();
    let old = f.initial(b"dir/file", EntryMode::Blob, b"old");
    let target = f.tree(b"dir/file", EntryMode::Blob, b"new");
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("file"), b"outside").unwrap();
    let failure = run(
        &f.repo,
        Some(old),
        Some(target),
        Limits::default(),
        &AtomicBool::new(false),
        &mut |event, _| {
            if event == "before delete" {
                fs::rename(f.root().join("dir"), f.root().join("saved")).unwrap();
                substitute(&f.root().join("dir"), outside.path(), link);
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(failure.report.applied.is_empty());
    assert_eq!(fs::read(f.root().join("saved/file")).unwrap(), b"old");
    assert_eq!(fs::read(outside.path().join("file")).unwrap(), b"outside");
}
fn substitute(path: &Path, outside: &Path, link: bool) {
    if link {
        symlink(outside, path).unwrap();
    } else {
        fs::create_dir(path).unwrap();
    }
}

#[test]
fn destination_symlink_substitution_does_not_follow_or_overwrite() {
    let f = Fixture::new();
    let tree = f.tree(b"file", EntryMode::Blob, b"new");
    let outside = tempfile::NamedTempFile::new().unwrap();
    fs::write(outside.path(), b"user").unwrap();
    let error = run(
        &f.repo,
        None,
        Some(tree),
        Limits::default(),
        &AtomicBool::new(false),
        &mut |event, _| {
            if event == "before rename" {
                symlink(outside.path(), f.root().join("file")).unwrap();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(error.report.applied.is_empty());
    assert_eq!(fs::read(outside.path()).unwrap(), b"user");
    assert!(
        fs::symlink_metadata(f.root().join("file"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(fs::read_dir(f.root()).unwrap().count(), 2);
}

#[rstest]
#[case::before("prepared", 0)]
#[case::during("after operation", 1)]
#[case::publication("before publication", 2)]
fn cancellation_reports_prior_namespace_changes(#[case] checkpoint: &str, #[case] count: usize) {
    let f = Fixture::new();
    let old = f.initial(b"file", EntryMode::Blob, b"old");
    let new = f.tree(b"file", EntryMode::Blob, b"new");
    let index = f.index_bytes();
    let cancel = AtomicBool::new(false);
    let error = run(
        &f.repo,
        Some(old),
        Some(new),
        Limits::default(),
        &cancel,
        &mut |event, _| {
            if event == checkpoint {
                cancel.store(true, Ordering::Relaxed);
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(matches!(*error.cause, Error::Cancelled));
    assert_eq!(error.report.applied.len(), count);
    assert_eq!(f.index_bytes(), index);
    assert!(!f.repo.git_dir().join("index.lock").exists());
}

#[rstest]
#[case::write("before write", 1)]
#[case::install("before rename", 1)]
#[case::delete("before delete", 0)]
#[case::index("before publication", 2)]
fn injected_io_failures_report_residual_changes(#[case] checkpoint: &str, #[case] count: usize) {
    let f = Fixture::new();
    let old = f.initial(b"file", EntryMode::Blob, b"old");
    let new = f.tree(b"file", EntryMode::Blob, b"new");
    let index = f.index_bytes();
    let error = run(
        &f.repo,
        Some(old),
        Some(new),
        Limits::default(),
        &AtomicBool::new(false),
        &mut |event, path| {
            if event == checkpoint {
                return Err(io(path, std::io::Error::other("injected failure")));
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.report.applied.len(), count);
    assert!(!error.report.index_published);
    assert_eq!(f.index_bytes(), index);
    assert!(error.cleanup.is_empty());
    assert!(!f.repo.git_dir().join("index.lock").exists());
}

#[test]
fn foreign_index_lock_is_preserved() {
    let f = Fixture::new();
    let target = f.tree(b"file", EntryMode::Blob, b"new");
    fs::write(f.repo.git_dir().join("index.lock"), b"foreign").unwrap();
    let error = f.checkout(None, Some(target)).unwrap_err();
    assert!(matches!(
        *error.cause,
        Error::Index(index::StorageError::Locked(_))
    ));
    assert_eq!(
        fs::read(f.repo.git_dir().join("index.lock")).unwrap(),
        b"foreign"
    );
    assert!(!f.root().join("file").exists());
}

#[test]
fn final_verification_refuses_changed_target_without_publishing() {
    let f = Fixture::new();
    let new = f.tree(b"file", EntryMode::Blob, b"new");
    let error = run(
        &f.repo,
        None,
        Some(new),
        Limits::default(),
        &AtomicBool::new(false),
        &mut |event, _| {
            if event == "before publication" {
                fs::write(f.root().join("file"), b"user").unwrap();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.report.applied.len(), 1);
    assert_eq!(error.report.stage, Stage::Publication);
    assert!(!f.repo.git_dir().join("index").exists());
    assert_eq!(fs::read(f.root().join("file")).unwrap(), b"user");
}

#[test]
fn mode_only_dirty_change_is_not_overwritten() {
    let f = Fixture::new();
    let old = f.initial(b"file", EntryMode::Blob, b"old");
    fs::set_permissions(f.root().join("file"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(f.checkout(Some(old), None).is_err());
    assert_eq!(
        fs::metadata(f.root().join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[test]
fn successful_install_with_failed_cleanup_is_reported() {
    let f = Fixture::new();
    let new = f.tree(b"file", EntryMode::Blob, b"new");
    let error = run(
        &f.repo,
        None,
        Some(new),
        Limits::default(),
        &AtomicBool::new(false),
        &mut |event, path| {
            if event == "before cleanup" {
                return Err(io(path, std::io::Error::other("cleanup failure")));
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.report.applied, vec![op(b"file", Action::Install)]);
    assert_eq!(error.cleanup.len(), 1);
    assert_eq!(fs::read(f.root().join("file")).unwrap(), b"new");
    assert_eq!(fs::read_dir(f.root()).unwrap().count(), 3);
    assert!(!f.repo.git_dir().join("index").exists());
}

#[rstest]
#[case::leaf(b"file", Action::Install)]
#[case::directory(b"dir/file", Action::CreateDirectory)]
fn failures_after_namespace_success_still_report_operation(
    #[case] path: &[u8],
    #[case] action: Action,
) {
    let f = Fixture::new();
    let target = f.tree(path, EntryMode::Blob, b"new");
    let error = run(
        &f.repo,
        None,
        Some(target),
        Limits::default(),
        &AtomicBool::new(false),
        &mut |event, path| {
            if event == "after namespace" {
                return Err(io(path, std::io::Error::other("post-mutation failure")));
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.report.applied.len(), 1);
    assert_eq!(error.report.applied[0].action, action);
    assert!(!f.repo.git_dir().join("index").exists());
}

#[test]
fn noncooperating_index_change_prevents_publication_after_worktree_update() {
    let f = Fixture::new();
    let old = f.initial(b"file", EntryMode::Blob, b"old");
    let new = f.tree(b"file", EntryMode::Blob, b"new");
    let foreign = index::Index::default().encode(Default::default()).unwrap();
    let error = run(
        &f.repo,
        Some(old),
        Some(new),
        Limits::default(),
        &AtomicBool::new(false),
        &mut |event, _| {
            if event == "before publication" {
                fs::write(f.repo.git_dir().join("index"), &foreign).unwrap();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(matches!(
        *error.cause,
        Error::Index(index::StorageError::Changed(_))
    ));
    assert_eq!(error.report.stage, Stage::Publication);
    assert_eq!(error.report.applied.len(), 2);
    assert_eq!(f.index_bytes(), foreign);
    assert_eq!(fs::read(f.root().join("file")).unwrap(), b"new");
}

#[test]
fn index_lock_cleanup_failure_is_visible_without_recursive_cleanup() {
    let f = Fixture::new();
    let new = f.tree(b"file", EntryMode::Blob, b"new");
    let error = run(
        &f.repo,
        None,
        Some(new),
        Limits::default(),
        &AtomicBool::new(false),
        &mut |event, _| {
            if event == "prepared" {
                fs::remove_file(f.repo.git_dir().join("index.lock")).unwrap();
                fs::create_dir(f.repo.git_dir().join("index.lock")).unwrap();
                fs::write(f.repo.git_dir().join("index.lock/foreign"), b"keep").unwrap();
                return Err(Error::Cancelled);
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.cleanup.len(), 1);
    assert_eq!(
        fs::read(f.repo.git_dir().join("index.lock/foreign")).unwrap(),
        b"keep"
    );
    assert!(!f.root().join("file").exists());
}

#[rstest]
#[case::objects(Limits { max_object_bytes: 2, ..Limits::default() })]
#[case::file(Limits { max_file_bytes: 2, ..Limits::default() })]
#[case::paths(Limits { max_path_bytes: 3, ..Limits::default() })]
#[case::depth(Limits { max_depth: 0, ..Limits::default() })]
#[case::directory(Limits { max_directory_entries: 0, ..Limits::default() })]
#[case::tree(Limits { trees: crate::TreeCompareLimits { max_entries: 0, ..crate::TreeCompareLimits::default() }, ..Limits::default() })]
#[case::index(Limits { index: index::Limits { max_entries: 0, ..index::Limits::default() }, ..Limits::default() })]
fn exhausted_preparation_limits_preserve_worktree(#[case] limits: Limits) {
    let f = Fixture::new();
    let new = f.tree(b"file", EntryMode::Blob, b"new");
    let failure = f
        .repo
        .checkout_tree(None, Some(new), limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(failure.report.applied.is_empty());
    assert!(!f.root().join("file").exists());
    assert!(!f.repo.git_dir().join("index.lock").exists());
}

#[test]
fn exhausted_worktree_budget_does_not_remove_files() {
    let f = Fixture::new();
    let old = f.initial(b"file", EntryMode::Blob, b"old");
    let limits = Limits {
        max_worktree_bytes: 2,
        ..Limits::default()
    };
    let failure = f
        .repo
        .checkout_tree(Some(old), None, limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(failure.report.applied.is_empty());
    assert_eq!(fs::read(f.root().join("file")).unwrap(), b"old");
}

#[rstest]
#[case::empty(b"")]
#[case::nul(b"foo\0bar")]
fn unsupported_symlink_payload_is_rejected_before_mutation(#[case] bytes: &[u8]) {
    let f = Fixture::new();
    let new = f.tree(b"file", EntryMode::Symlink, bytes);
    let failure = f.checkout(None, Some(new)).unwrap_err();
    assert!(failure.report.applied.is_empty());
    assert!(!f.root().join("file").exists());
}

#[test]
fn missing_blob_is_rejected_before_mutation() {
    let f = Fixture::new();
    let tree = f.entry_tree(b"file", EntryMode::Blob, ObjectId::for_blob(b"missing"));
    let failure = f.checkout(None, Some(tree)).unwrap_err();
    assert!(matches!(*failure.cause, Error::InvalidBlob(_)));
    assert!(failure.report.applied.is_empty());
}

#[test]
fn wrong_kind_blob_is_rejected_before_mutation() {
    let f = Fixture::new();
    let child = f.tree(b"child", EntryMode::Blob, b"hello");
    let tree = f.entry_tree(b"file", EntryMode::Blob, child);
    let failure = f.checkout(None, Some(tree)).unwrap_err();
    assert!(matches!(*failure.cause, Error::InvalidBlob(_)));
    assert!(failure.report.applied.is_empty());
}

#[test]
fn corrupted_blob_is_rejected_before_mutation() {
    let f = Fixture::new();
    let tree = f.tree(b"file", EntryMode::Blob, b"hello");
    let id = ObjectId::for_blob(b"hello").to_string();
    fs::write(
        f.repo.object_dir().join(&id[..2]).join(&id[2..]),
        b"corrupt",
    )
    .unwrap();
    let failure = f.checkout(None, Some(tree)).unwrap_err();
    assert!(matches!(*failure.cause, Error::Object(_)));
    assert!(failure.report.applied.is_empty());
}

#[test]
fn colliding_tree_paths_are_rejected_even_on_case_sensitive_filesystems() {
    let f = Fixture::new();
    let store = f.repo.loose_objects().unwrap();
    let id = store.write_blob(b"new").unwrap();
    let tree = store
        .write_tree(
            &Tree::new(vec![
                TreeEntry {
                    name: b"file".to_vec(),
                    mode: EntryMode::Blob,
                    id,
                },
                TreeEntry {
                    name: b"FILE".to_vec(),
                    mode: EntryMode::Blob,
                    id,
                },
            ])
            .unwrap(),
        )
        .unwrap();
    let failure = f.checkout(None, Some(tree)).unwrap_err();
    assert!(failure.report.applied.is_empty());
    assert!(!f.root().join("file").exists());
}

#[test]
fn existing_symlink_ancestor_does_not_escape_worktree() {
    let f = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), f.root().join("dir")).unwrap();
    let tree = f.tree(b"dir/file", EntryMode::Blob, b"new");
    let failure = f.checkout(None, Some(tree)).unwrap_err();
    assert!(failure.report.applied.is_empty());
    assert!(!outside.path().join("file").exists());
}

#[test]
fn bare_nested_repository_is_not_entered() {
    let f = Fixture::new();
    Repository::init(f.root().join("nested"), InitKind::Bare).unwrap();
    let tree = f.tree(b"nested/file", EntryMode::Blob, b"new");
    let failure = f.checkout(None, Some(tree)).unwrap_err();
    assert!(failure.report.applied.is_empty());
    assert!(!f.root().join("nested/file").exists());
}

#[test]
fn root_substitution_before_mutation_is_detected() {
    let f = Fixture::new();
    let tree = f.tree(b"file", EntryMode::Blob, b"new");
    let saved = f.root().with_file_name("saved");
    let failure = run(
        &f.repo,
        None,
        Some(tree),
        Limits::default(),
        &AtomicBool::new(false),
        &mut |event, _| {
            if event == "prepared" {
                fs::rename(f.root(), &saved).unwrap();
                fs::create_dir(f.root()).unwrap();
                fs::write(f.root().join("keep"), b"user").unwrap();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(failure.report.applied.is_empty());
    assert!(!saved.join("file").exists());
    assert_eq!(fs::read(f.root().join("keep")).unwrap(), b"user");
    assert_eq!(failure.cleanup.len(), 1); // The moved metadata lock is deliberately not chased.
}

#[cfg(target_os = "linux")]
#[test]
fn linux_preserves_non_utf8_filename_bytes() {
    let f = Fixture::new();
    let tree = f.tree(b"\xff", EntryMode::Blob, b"raw");
    f.checkout(None, Some(tree)).unwrap();
    assert_eq!(
        fs::read(f.root().join(OsStr::from_bytes(b"\xff"))).unwrap(),
        b"raw"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_rejects_non_ascii_paths() {
    let f = Fixture::new();
    let tree = f.tree("é".as_bytes(), EntryMode::Blob, b"raw");
    let failure = f.checkout(None, Some(tree)).unwrap_err();
    assert!(failure.report.applied.is_empty());
}

#[test]
fn replaced_temporary_is_not_deleted_by_cleanup() {
    let f = Fixture::new();
    let new = f.tree(b"file", EntryMode::Blob, b"new");
    let mut replaced = Vec::new();
    let error = run(
        &f.repo,
        None,
        Some(new),
        Limits::default(),
        &AtomicBool::new(false),
        &mut |event, path| {
            if event == "before cleanup" {
                let temporary = f.root().join(OsStr::from_bytes(path));
                fs::remove_file(&temporary).unwrap();
                fs::write(&temporary, b"foreign").unwrap();
                replaced = path.to_vec();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.cleanup.len(), 1);
    assert_eq!(
        fs::read(f.root().join(OsStr::from_bytes(&replaced))).unwrap(),
        b"foreign"
    );
    assert_eq!(fs::read(f.root().join("file")).unwrap(), b"new");
    assert!(!error.report.index_published);
}

#[test]
fn target_verification_budget_is_checked_before_installation() {
    let f = Fixture::new();
    let new = f.tree(b"file", EntryMode::Blob, b"new");
    let limits = Limits {
        max_worktree_bytes: 2,
        ..Limits::default()
    };
    let failure = f
        .repo
        .checkout_tree(None, Some(new), limits, &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(failure.report.stage, Stage::Preparation);
    assert!(!f.root().join("file").exists());
}

fn append_extension(repo: &Repository, signature: &[u8; 4]) -> Vec<u8> {
    use sha1::{Digest, Sha1};
    let mut bytes = fs::read(repo.git_dir().join("index")).unwrap();
    bytes.truncate(bytes.len() - 20);
    bytes.extend_from_slice(signature);
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&Sha1::digest(&bytes));
    fs::write(repo.git_dir().join("index"), &bytes).unwrap();
    bytes
}

#[test]
fn unsupported_optional_extension_is_preserved_and_refused() {
    let f = Fixture::new();
    let tree = f.initial(b"file", EntryMode::Blob, b"old");
    let before = append_extension(&f.repo, b"REUC");
    let failure = f.checkout(Some(tree), Some(tree)).unwrap_err();
    assert!(failure.report.applied.is_empty());
    assert_eq!(f.index_bytes(), before);
}

#[test]
fn unchanged_checkout_discards_unverified_tree_cache() {
    let f = Fixture::new();
    let tree = f.initial(b"file", EntryMode::Blob, b"old");
    append_extension(&f.repo, b"TREE");
    f.checkout(Some(tree), Some(tree)).unwrap();
    assert!(
        f.repo
            .read_index(Default::default())
            .unwrap()
            .unwrap()
            .extensions()
            .is_empty()
    );
}
