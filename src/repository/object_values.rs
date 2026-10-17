//! Object IDs and trees built in the repository's object format.
use super::Repository;
use crate::{ObjectId, ParseObjectIdError, Tree, TreeEntry, TreeError};

impl Repository {
    /// Builds an object ID from raw digest bytes in this repository's object format.
    ///
    /// Equivalent to `ObjectId::from_bytes(repo.object_format(), bytes)`; see
    /// [`ObjectId::from_bytes`]. Use it when an ID crosses a boundary as bytes, such as a
    /// consumer's own commit ID type.
    ///
    /// # Errors
    ///
    /// Returns [`ParseObjectIdError`] when `bytes` isn't exactly one digest of this format: 20
    /// bytes for SHA-1, 32 for SHA-256.
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha256,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    /// let id = repo.loose_objects().write_blob(b"notes")?;
    /// assert_eq!(repo.object_id_from_bytes(id.as_bytes())?, id);
    /// assert!(repo.object_id_from_bytes(&[0; 20]).is_err()); // A SHA-1 length.
    ///
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn object_id_from_bytes(&self, bytes: &[u8]) -> Result<ObjectId, ParseObjectIdError> {
        ObjectId::from_bytes(self.object_format(), bytes)
    }

    /// Parses a full hexadecimal object ID in this repository's object format.
    ///
    /// Equivalent to `ObjectId::from_hex(repo.object_format(), hex)`; see [`ObjectId::from_hex`].
    /// Abbreviated IDs need [`crate::Objects::resolve_prefix`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`ParseObjectIdError`] for non-hexadecimal text or a length other than this
    /// format's full width.
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    /// let id = repo.loose_objects().write_blob(b"hello")?;
    /// assert_eq!(
    ///     repo.object_id_from_hex("b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0")?,
    ///     id
    /// );
    /// assert!(repo.object_id_from_hex("b6fc4c6").is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn object_id_from_hex(&self, hex: &str) -> Result<ObjectId, ParseObjectIdError> {
        ObjectId::from_hex(self.object_format(), hex)
    }

    /// Builds a validated tree in this repository's object format.
    ///
    /// Equivalent to `Tree::new(repo.object_format(), entries)`; see [`Tree::new`] for sorting,
    /// name validation and the format check on entry IDs. The tree is not written; pass it to
    /// [`crate::LooseObjects::write_tree`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Tree::new`], including an entry ID of the other format.
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha256,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    /// let empty = repo.loose_objects().write_tree(&repo.new_tree(vec![])?)?;
    /// assert_eq!(empty.format(), ObjectFormat::Sha256);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new_tree(&self, entries: Vec<TreeEntry>) -> Result<Tree, TreeError> {
        Tree::new(self.object_format(), entries)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use crate::{EntryMode, InitKind, ObjectFormat, ObjectId, Repository, Tree, TreeEntry};

    fn repository(format: ObjectFormat) -> (tempfile::TempDir, Repository) {
        let directory = tempfile::tempdir().unwrap();
        let repo =
            Repository::init(format, directory.path().join("r.git"), InitKind::Bare).unwrap();
        (directory, repo)
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn ids_take_the_repository_format(#[case] format: ObjectFormat) {
        let (_directory, repo) = repository(format);
        let id = ObjectId::for_blob(format, b"payload");
        assert_eq!(repo.object_id_from_bytes(id.as_bytes()).unwrap(), id);
        assert_eq!(repo.object_id_from_hex(&id.to_string()).unwrap(), id);
    }

    #[rstest]
    #[case::sha1_repository(ObjectFormat::Sha1, ObjectFormat::Sha256)]
    #[case::sha256_repository(ObjectFormat::Sha256, ObjectFormat::Sha1)]
    fn ids_of_the_other_format_are_rejected(
        #[case] format: ObjectFormat,
        #[case] other: ObjectFormat,
    ) {
        let (_directory, repo) = repository(format);
        let id = ObjectId::for_blob(other, b"payload");
        assert!(repo.object_id_from_bytes(id.as_bytes()).is_err());
        assert!(repo.object_id_from_hex(&id.to_string()).is_err());
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn trees_take_the_repository_format(#[case] format: ObjectFormat) {
        let (_directory, repo) = repository(format);
        let entries = vec![TreeEntry {
            mode: EntryMode::Blob,
            name: b"a".to_vec(),
            id: ObjectId::for_blob(format, b"a"),
        }];
        let tree = repo.new_tree(entries.clone()).unwrap();
        assert_eq!(tree, Tree::new(format, entries).unwrap());
    }

    #[test]
    fn tree_rejects_entries_of_the_other_format() {
        let (_directory, repo) = repository(ObjectFormat::Sha256);
        let entries = vec![TreeEntry {
            mode: EntryMode::Blob,
            name: b"a".to_vec(),
            id: ObjectId::for_blob(ObjectFormat::Sha1, b"a"),
        }];
        assert!(repo.new_tree(entries).is_err());
    }
}
