//! Original fixtures for explicit replacement, optional-extension policy and selected-path guards.
use rstest::rstest;

use super::*;
use crate::index::{Extension, Mode, Version};
use crate::{InitKind, ObjectFormat, ObjectId};

struct Fixture {
    _root: tempfile::TempDir,
    repo: Repository,
    path: PathBuf,
}
impl Fixture {
    fn new(format: ObjectFormat) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("repo"), InitKind::Worktree).unwrap();
        let path = root.path().join("selected.index");
        Self {
            _root: root,
            repo,
            path,
        }
    }
    fn write(&self, index: &Index) -> Vec<u8> {
        let bytes = index.encode(Limits::default()).unwrap();
        fs::write(&self.path, &bytes).unwrap();
        bytes
    }
    fn edit(&self) -> IndexEdit {
        self.repo
            .edit_index_at_with_options(
                &self.path,
                Limits::default(),
                EditOptions {
                    resolve_split: true,
                    follow_symlink: true,
                },
            )
            .unwrap()
    }
    fn split(&self, extensions: &[[u8; 4]]) -> PathBuf {
        let format = self.repo.object_format();
        let shared = populated(format);
        let bytes = shared.encode(Limits::default()).unwrap();
        let hash = &bytes[bytes.len() - format.digest_len()..];
        let id = ObjectId::from_bytes(format, hash).unwrap();
        let shared_path = self.path.with_file_name(format!("sharedindex.{id}"));
        fs::write(&shared_path, &bytes).unwrap();
        let mut link = hash.to_vec();
        // Two independently authored empty EWAH bitmaps (bit count, run count, word, pointer).
        let bitmap = b"\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\0";
        link.extend_from_slice(bitmap);
        link.extend_from_slice(bitmap);
        let mut index = Index::empty(format);
        index.set_version(Version::V4, Limits::default()).unwrap();
        index.extensions.push(Extension {
            signature: *b"link",
            data: link,
        });
        add_extensions(&mut index, extensions);
        self.write(&index);
        shared_path
    }
}
fn populated(format: ObjectFormat) -> Index {
    Index::new(
        format,
        vec![Entry::new(
            b"entry".to_vec(),
            Mode::Regular,
            ObjectId::null(format),
        )],
        Limits::default(),
    )
    .unwrap()
}
fn add_extensions(index: &mut Index, signatures: &[[u8; 4]]) {
    index
        .extensions
        .extend(signatures.iter().map(|&signature| Extension {
            signature,
            data: b"opaque".to_vec(),
        }));
}
fn sparse(format: ObjectFormat) -> Index {
    let mut entry = Entry::new(
        b"directory/".to_vec(),
        Mode::SparseDirectory,
        ObjectId::null(format),
    );
    entry.skip_worktree = true;
    Index::new(format, vec![entry], Limits::default()).unwrap()
}

#[rstest]
fn fresh_replacement_clears_optional_and_sparse_state(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let fixture = Fixture::new(format);
    let mut original = sparse(format);
    add_extensions(&mut original, &[*b"TEST", *b"REUC"]);
    fixture.write(&original);
    let mut edit = fixture.edit();
    let replacement = populated(format);
    edit.replace_index(replacement.clone()).unwrap();
    assert_eq!(edit.index(), &replacement);
    edit.commit().unwrap();
    let actual =
        Index::parse(format, &fs::read(&fixture.path).unwrap(), Limits::default()).unwrap();
    assert_eq!(actual, replacement);
}

#[rstest]
#[case::foreign_format(false)]
#[case::entry_limit(true)]
fn replacement_failure_is_atomic(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] limit: bool,
) {
    let fixture = Fixture::new(format);
    let original = fixture.write(&Index::empty(format));
    let limits = Limits {
        max_entries: 0,
        ..Limits::default()
    };
    let mut edit = fixture.repo.edit_index_at(&fixture.path, limits).unwrap();
    let replacement = if limit {
        populated(format)
    } else {
        Index::empty(if format == ObjectFormat::Sha1 {
            ObjectFormat::Sha256
        } else {
            ObjectFormat::Sha1
        })
    };
    assert!(edit.replace_index(replacement).is_err());
    assert_eq!(edit.index().encode(Limits::default()).unwrap(), original);
    assert_eq!(fs::read(&fixture.path).unwrap(), original);
}

#[rstest]
fn replacement_cannot_introduce_split_dependency(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let fixture = Fixture::new(format);
    fixture.split(&[]);
    let mut edit = fixture.edit();
    let replacement = edit.index().clone();
    let before = replacement.clone();
    assert_eq!(
        edit.replace_index(replacement),
        Err(Error::ExtensionPreventsEdit(*b"link"))
    );
    assert_eq!(edit.index(), &before);
}

#[rstest]
#[case::unknown(*b"TEST", true)]
#[case::absent(*b"NOPE", false)]
fn explicit_optional_drop_and_noop(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] signature: [u8; 4],
    #[case] changed: bool,
) {
    let fixture = Fixture::new(format);
    let mut original = populated(format);
    add_extensions(&mut original, &[*b"TEST"]);
    let before = fixture.write(&original);
    let mut edit = fixture.edit();
    edit.discard_optional_extensions(&[signature]).unwrap();
    assert_eq!(edit.index().extensions().is_empty(), changed);
    edit.commit().unwrap();
    assert_eq!(fs::read(&fixture.path).unwrap() == before, !changed);
}

#[rstest]
fn mandatory_drop_refuses_before_optional_mutation(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let fixture = Fixture::new(format);
    let mut original = populated(format);
    add_extensions(&mut original, &[*b"TEST"]);
    fixture.write(&original);
    let mut edit = fixture.edit();
    assert_eq!(
        edit.discard_optional_extensions(&[*b"TEST", *b"link"]),
        Err(Error::MandatoryExtension(*b"link"))
    );
    assert_eq!(edit.index(), &original);
}

#[rstest]
fn actual_removal_invalidates_both_offset_caches(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let fixture = Fixture::new(format);
    let mut original = populated(format);
    add_extensions(&mut original, &[*b"TEST", *b"EOIE", *b"IEOT", *b"REUC"]);
    fixture.write(&original);
    let mut edit = fixture.edit();
    edit.discard_optional_extensions(&[*b"TEST"]).unwrap();
    assert_eq!(
        edit.index()
            .extensions()
            .iter()
            .map(Extension::signature)
            .collect::<Vec<_>>(),
        [*b"REUC"]
    );
    assert_eq!(edit.index().entries(), original.entries());
}

#[rstest]
fn split_drop_normalizes_entries_version_and_retains_shared_guard(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let fixture = Fixture::new(format);
    let shared = fixture.split(&[*b"TEST", *b"TREE", *b"REUC"]);
    let before = fs::read(&shared).unwrap();
    let mut edit = fixture.edit();
    let entries = edit.index().entries().to_vec();
    edit.discard_optional_extensions(&[*b"TEST"]).unwrap();
    assert_eq!(edit.index().version(), Version::V4);
    assert_eq!(edit.index().entries(), entries);
    assert_eq!(
        edit.index()
            .extensions()
            .iter()
            .map(Extension::signature)
            .collect::<Vec<_>>(),
        [*b"REUC"]
    );
    edit.commit().unwrap();
    assert_eq!(fs::read(shared).unwrap(), before);
    assert!(Index::parse(format, &fs::read(fixture.path).unwrap(), Limits::default()).is_ok());
}

#[rstest]
fn optional_drop_preserves_sparse_marker(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let fixture = Fixture::new(format);
    let mut original = sparse(format);
    add_extensions(&mut original, &[*b"TEST"]);
    fixture.write(&original);
    let mut edit = fixture.edit();
    edit.discard_optional_extensions(&[*b"TEST"]).unwrap();
    assert_eq!(edit.index().entries(), original.entries());
    assert_eq!(edit.index().extensions()[0].signature(), *b"sdir");
    edit.commit().unwrap();
}

#[rstest]
#[case::colocated(true)]
#[case::other_directory(false)]
fn alternate_split_uses_selected_parent(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] colocated: bool,
) {
    let mut fixture = Fixture::new(format);
    if colocated {
        fixture.path = fixture.repo.git_dir().join("alternate.index");
    }
    fixture.split(&[]);
    let edit = fixture.edit();
    assert_eq!(edit.index().entries(), populated(format).entries());
    assert!(matches!(
        fixture.repo.edit_index_at(&fixture.path, Limits::default()),
        Err(StorageError::Locked(_))
    ));
    edit.abort().unwrap();
    assert!(matches!(
        fixture.repo.edit_index_at(&fixture.path, Limits::default()),
        Err(StorageError::Format {
            source: Error::MandatoryExtension(_),
            ..
        })
    ));
}

#[rstest]
#[case::missing(false)]
#[case::malformed(true)]
fn split_dependency_failure_never_publishes_empty(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] malformed: bool,
) {
    let fixture = Fixture::new(format);
    let shared = fixture.split(&[]);
    let original = fs::read(&fixture.path).unwrap();
    if malformed {
        fs::write(&shared, b"invalid").unwrap();
    } else {
        fs::remove_file(&shared).unwrap();
    }
    let error = fixture
        .repo
        .edit_index_at_with_options(
            &fixture.path,
            Limits::default(),
            EditOptions {
                resolve_split: true,
                follow_symlink: false,
            },
        )
        .unwrap_err();
    if malformed {
        assert!(matches!(error, StorageError::Format { .. }));
    } else {
        assert!(matches!(error, StorageError::MissingShared(_)));
    }
    assert_eq!(fs::read(&fixture.path).unwrap(), original);
    assert!(!fixture.path.with_extension("index.lock").exists());
}

#[cfg(unix)]
#[rstest]
#[case::existing(false)]
#[case::dangling(true)]
fn symlink_publication_replaces_leaf_not_target(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] dangling: bool,
) {
    let fixture = Fixture::new(format);
    let target = fixture.path.with_file_name("referent");
    let original = populated(format).encode(Limits::default()).unwrap();
    if !dangling {
        fs::write(&target, &original).unwrap();
    }
    std::os::unix::fs::symlink("referent", &fixture.path).unwrap();
    assert!(matches!(
        fixture.repo.edit_index_at(&fixture.path, Limits::default()),
        Err(StorageError::NotRegular(_))
    ));
    let mut edit = fixture.edit();
    assert_eq!(edit.index().entries().is_empty(), dangling);
    edit.replace_index(Index::empty(format)).unwrap();
    edit.commit().unwrap();
    assert!(fs::symlink_metadata(&fixture.path).unwrap().is_file());
    if dangling {
        assert!(!target.exists());
    } else {
        assert_eq!(fs::read(target).unwrap(), original);
    }
}

#[cfg(unix)]
#[rstest]
#[case::retarget(0)]
#[case::same_target_new_inode(1)]
#[case::referent_bytes(2)]
#[case::regular_same_bytes(3)]
fn changed_symlink_snapshot_refuses_publication(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] change: u8,
) {
    let fixture = Fixture::new(format);
    let target = fixture.path.with_file_name("referent");
    let bytes = populated(format).encode(Limits::default()).unwrap();
    fs::write(&target, &bytes).unwrap();
    std::os::unix::fs::symlink("referent", &fixture.path).unwrap();
    let mut edit = fixture.edit();
    edit.replace_index(Index::empty(format)).unwrap();
    match change {
        0 => {
            fs::remove_file(&fixture.path).unwrap();
            fs::copy(&target, fixture.path.with_file_name("other")).unwrap();
            std::os::unix::fs::symlink("other", &fixture.path).unwrap();
        }
        1 => {
            fs::rename(&fixture.path, fixture.path.with_file_name("old-link")).unwrap();
            std::os::unix::fs::symlink("referent", &fixture.path).unwrap();
        }
        2 => {
            fs::write(
                &target,
                Index::empty(format).encode(Limits::default()).unwrap(),
            )
            .unwrap();
        }
        3 => {
            fs::remove_file(&fixture.path).unwrap();
            fs::write(&fixture.path, &bytes).unwrap();
        }
        _ => unreachable!(),
    }
    let before = fs::read(&fixture.path).unwrap();
    assert!(matches!(edit.commit(), Err(StorageError::Changed(_))));
    assert_eq!(fs::read(&fixture.path).unwrap(), before);
}

#[rstest]
#[case::primary(0)]
#[case::shared_bytes(1)]
#[case::shared_deleted(2)]
fn replacement_retains_storage_guards(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] change: u8,
) {
    let fixture = Fixture::new(format);
    let shared = fixture.split(&[]);
    let before = fs::read(&fixture.path).unwrap();
    let mut edit = fixture.edit();
    edit.replace_index(Index::empty(format)).unwrap();
    match change {
        0 => fs::write(
            &fixture.path,
            Index::empty(format).encode(Limits::default()).unwrap(),
        )
        .unwrap(),
        1 => fs::write(&shared, b"changed").unwrap(),
        2 => fs::remove_file(&shared).unwrap(),
        _ => unreachable!(),
    }
    assert!(matches!(edit.commit(), Err(StorageError::Changed(_))));
    if change != 0 {
        assert_eq!(fs::read(&fixture.path).unwrap(), before);
    }
}

#[rstest]
fn explicit_options_preserve_lock_contention(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let fixture = Fixture::new(format);
    let edit = fixture.edit();
    assert!(matches!(
        fixture.repo.edit_index_at_with_options(
            &fixture.path,
            Limits::default(),
            EditOptions::default()
        ),
        Err(StorageError::Locked(_))
    ));
    edit.abort().unwrap();
}

#[rstest]
fn retained_unknown_prevents_split_conversion_atomically(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let fixture = Fixture::new(format);
    fixture.split(&[*b"TEST", *b"KEEP"]);
    let mut edit = fixture.edit();
    let before = edit.index().clone();
    assert_eq!(
        edit.discard_optional_extensions(&[*b"TEST"]),
        Err(Error::ExtensionPreventsEdit(*b"KEEP"))
    );
    assert_eq!(edit.index(), &before);
}

#[cfg(unix)]
#[rstest]
#[case::selected_shared_present(true)]
#[case::only_referent_shared_present(false)]
fn symlink_split_lookup_never_uses_referent_parent(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] selected_shared: bool,
) {
    let fixture = Fixture::new(format);
    let shared = fixture.split(&[]);
    let target = fixture.repo.git_dir().join("referent.index");
    fs::rename(&fixture.path, &target).unwrap();
    if !selected_shared {
        fs::rename(
            &shared,
            fixture.repo.git_dir().join(shared.file_name().unwrap()),
        )
        .unwrap();
    }
    std::os::unix::fs::symlink(&target, &fixture.path).unwrap();
    let result = fixture.repo.edit_index_at_with_options(
        &fixture.path,
        Limits::default(),
        EditOptions {
            resolve_split: true,
            follow_symlink: true,
        },
    );
    if selected_shared {
        let mut edit = result.unwrap();
        assert_eq!(edit.index().entries(), populated(format).entries());
        edit.replace_index(Index::empty(format)).unwrap();
        let before = fs::read(&target).unwrap();
        edit.commit().unwrap();
        assert_eq!(fs::read(target).unwrap(), before);
    } else {
        assert!(matches!(result, Err(StorageError::MissingShared(path)) if path == shared));
        assert!(fs::symlink_metadata(fixture.path).unwrap().is_symlink());
    }
}

#[rstest]
fn synchronization_failure_precedes_index_publication(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let fixture = Fixture::new(format);
    let original = fixture.write(&populated(format));
    let mut edit = fixture.edit();
    edit.replace_index(Index::empty(format)).unwrap();
    let error = edit
        .publish_with_policy(
            IndexCommitOptions {
                sync: true,
                ..Default::default()
            },
            |_| Err(io::Error::other("injected sync failure")),
            |_, _| panic!("must not publish after failed sync"),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        StorageError::Io {
            operation: "synchronize index lock",
            ..
        }
    ));
    assert_eq!(fs::read(&fixture.path).unwrap(), original);
    edit.abort().unwrap();
}

#[rstest]
fn invalid_commit_mode_preserves_index_and_cleans_lock(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let fixture = Fixture::new(format);
    let original = fixture.write(&populated(format));
    let edit = fixture.edit();
    assert!(
        edit.commit_with_options(IndexCommitOptions {
            shared_permissions: crate::SharedPermissions::Exact(0o444),
            sync: false
        })
        .is_err()
    );
    assert_eq!(fs::read(&fixture.path).unwrap(), original);
    assert!(!fixture.path.with_extension("index.lock").exists());
}
