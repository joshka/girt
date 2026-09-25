use rstest::rstest;

use super::*;
use crate::{InitKind, PackLimits, Repository, Tree};

struct Fixture {
    root: tempfile::TempDir,
    objects: Objects,
    change: TreeChange,
    tree: ObjectId,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(root.path().join("repo"), InitKind::Bare).unwrap();
        let loose = repo.loose_objects().unwrap();
        let old = loose.write_blob(b"old\n").unwrap();
        let new = loose.write_blob(b"new\n").unwrap();
        let tree = loose.write_tree(&Tree::new(vec![]).unwrap()).unwrap();
        let objects = repo.objects(PackLimits::default()).unwrap();
        Self {
            root,
            objects,
            tree,
            change: TreeChange {
                path: b"file".to_vec(),
                old: Some(TreeValue {
                    mode: EntryMode::Blob,
                    id: old,
                }),
                new: Some(TreeValue {
                    mode: EntryMode::Blob,
                    id: new,
                }),
            },
        }
    }

    fn read(&self, limit: usize) -> Result<BlobContent, ContentReadError> {
        BlobContent::read(
            &self.objects,
            &self.change,
            ReadLimits::default(),
            limit,
            &AtomicBool::new(false),
        )
    }

    fn old_path(&self) -> std::path::PathBuf {
        let id = self.change.old.unwrap().id.to_string();
        self.root
            .path()
            .join("repo/objects")
            .join(&id[..2])
            .join(&id[2..])
    }
}

#[rstest]
#[case::regular(EntryMode::Blob)]
#[case::executable(EntryMode::Executable)]
#[case::symlink_bytes(EntryMode::Symlink)]
fn accepts_blob_modes_with_exact_cumulative_limit(#[case] mode: EntryMode) {
    let mut f = Fixture::new();
    f.change.old.as_mut().unwrap().mode = mode;
    f.change.new.as_mut().unwrap().mode = mode;
    let content = f.read(8).unwrap();
    assert_eq!(content.old, b"old\n");
    assert_eq!(content.new, b"new\n");
}

#[rstest]
#[case::old(None, Some(b"new\n".as_slice()), b"", b"new\n")]
#[case::new(Some(b"old\n".as_slice()), None, b"old\n", b"")]
#[case::both(None, None, b"", b"")]
fn absent_sides_are_empty(
    #[case] old: Option<&[u8]>,
    #[case] new: Option<&[u8]>,
    #[case] expected_old: &[u8],
    #[case] expected_new: &[u8],
) {
    let mut f = Fixture::new();
    f.change.old = old.map(|_| f.change.old.unwrap());
    f.change.new = new.map(|_| f.change.new.unwrap());
    let content = f.read(8).unwrap();
    assert_eq!(content.old, expected_old);
    assert_eq!(content.new, expected_new);
}

#[rstest]
#[case::tree(EntryMode::Tree)]
#[case::gitlink(EntryMode::Gitlink)]
fn rejects_non_blob_modes_before_reading_either_side(#[case] mode: EntryMode) {
    let mut f = Fixture::new();
    std::fs::remove_file(f.old_path()).unwrap();
    f.change.new.as_mut().unwrap().mode = mode;
    assert!(matches!(f.read(8), Err(ContentReadError::UnsupportedMode(found)) if found == mode));
}

#[rstest]
#[case::first_payload(3)]
#[case::combined_payload(7)]
fn bounds_decoding_before_retaining_payload(#[case] limit: usize) {
    let f = Fixture::new();
    assert!(matches!(f.read(limit), Err(ContentReadError::Read { .. })));
}

#[test]
fn per_read_limit_remains_independent() {
    let f = Fixture::new();
    let read = ReadLimits {
        max_object_bytes: 3,
        ..ReadLimits::default()
    };
    assert!(matches!(
        BlobContent::read(&f.objects, &f.change, read, 8, &AtomicBool::new(false)),
        Err(ContentReadError::Read { .. })
    ));
}

#[test]
fn missing_equal_ids_are_not_skipped() {
    let mut f = Fixture::new();
    std::fs::remove_file(f.old_path()).unwrap();
    f.change.new = f.change.old;
    assert!(
        matches!(f.read(8), Err(ContentReadError::Missing(id)) if id == f.change.old.unwrap().id)
    );
}

#[test]
fn missing_replacement_fails_after_reading_original() {
    let mut f = Fixture::new();
    let missing = ObjectId::Sha1([42; 20]);
    f.change.new.as_mut().unwrap().id = missing;
    assert!(matches!(f.read(8), Err(ContentReadError::Missing(id)) if id == missing));
}

#[test]
fn wrong_kind_retains_identity_and_actual_kind() {
    let mut f = Fixture::new();
    f.change.new.as_mut().unwrap().id = f.tree;
    assert!(
        matches!(f.read(8), Err(ContentReadError::NotBlob { id, actual: ObjectKind::Tree }) if id == f.tree)
    );
}

#[test]
fn corrupt_payload_retains_storage_source() {
    let f = Fixture::new();
    std::fs::write(f.old_path(), b"not a zlib object").unwrap();
    let error = f.read(8).unwrap_err();
    assert!(std::error::Error::source(&error).is_some());
    assert!(matches!(error, ContentReadError::Read { id, .. } if id == f.change.old.unwrap().id));
}

#[test]
fn cancellation_precedes_mode_errors_and_reads() {
    let mut f = Fixture::new();
    f.change.old.as_mut().unwrap().mode = EntryMode::Gitlink;
    assert!(matches!(
        BlobContent::read(
            &f.objects,
            &f.change,
            ReadLimits::default(),
            8,
            &AtomicBool::new(true)
        ),
        Err(ContentReadError::Cancelled)
    ));
}
