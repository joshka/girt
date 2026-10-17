//! Reads that check an object's kind and parse it: [`Objects::read_commit`],
//! [`Objects::read_tree`], [`Objects::read_tag`] and [`Objects::read_blob`].
//!
//! Each uses [`ReadLimits::default`] and has a `_with` form, such as [`Objects::read_tree_with`],
//! that takes explicit [`ReadLimits`]. Unlike [`Objects::read`], absence is an error: an identity
//! that came from a reference, a tree entry or a history walk names an object that should exist.

use super::{ObjectReadError, Objects};
use crate::{
    Commit, CommitError, Object, ObjectId, ObjectKind, ReadLimits, Tag, TagError, Tree, TreeError,
};

impl Objects {
    /// Reads `id` and checks its kind: `Ok(Err(None))` when absent, `Ok(Err(Some(kind)))` when it
    /// has another kind.
    fn read_kind(
        &self,
        id: ObjectId,
        expected: ObjectKind,
        limits: ReadLimits,
    ) -> Result<Result<Object, Option<ObjectKind>>, ObjectReadError> {
        Ok(match self.read_with(id, limits)? {
            None => Err(None),
            Some(object) if object.kind() != expected => Err(Some(object.kind())),
            Some(object) => Ok(object),
        })
    }

    /// Reads and parses the commit `id`.
    ///
    /// Use this when `id` came from a reference or a history walk and must be a commit. Unlike
    /// [`Self::read`], a missing object is an error, because a commit you were pointed at should
    /// exist. Use [`Self::read`] to test whether an object exists or to accept any kind.
    ///
    /// ```
    /// use girt::{
    ///     Commit, CommitFields, InitKind, ObjectFormat, PackLimits, ReadLimits, Repository,
    ///     Signature, Tree,
    /// };
    ///
    /// let directory = tempfile::tempdir()?;
    /// let path = directory.path().join("repo.git");
    /// let repository = Repository::init(ObjectFormat::Sha1, path, InitKind::Bare)?;
    /// let loose = repository.loose_objects();
    /// let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
    /// let person = Signature {
    ///     name: b"A. Writer".to_vec(),
    ///     email: b"writer@example.com".to_vec(),
    ///     seconds: 1_700_000_000,
    ///     offset_minutes: 0,
    /// };
    /// let id = loose.write_commit(&Commit::new(CommitFields {
    ///     tree,
    ///     parents: vec![],
    ///     author: person.clone(),
    ///     committer: person,
    ///     extra_headers: vec![],
    ///     message: b"first\n".to_vec(),
    /// })?)?;
    ///
    /// let objects = repository.objects_with(PackLimits::default())?;
    /// let commit = objects.read_commit_with(id, ReadLimits::default())?;
    /// assert_eq!(commit.message(), b"first\n");
    /// assert!(
    ///     objects
    ///         .read_commit_with(tree, ReadLimits::default())
    ///         .is_err()
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`ReadCommitError::Missing`] if no store has `id`, [`ReadCommitError::Kind`] if
    /// `id` names another kind of object, [`ReadCommitError::Parse`] for a malformed commit, and
    /// [`ReadCommitError::Read`] for the failures of [`Self::read`].
    pub fn read_commit_with(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> Result<Commit, ReadCommitError> {
        let object = match self.read_kind(id, ObjectKind::Commit, limits)? {
            Ok(object) => object,
            Err(None) => return Err(ReadCommitError::Missing(id)),
            Err(Some(kind)) => return Err(ReadCommitError::Kind { id, kind }),
        };
        Ok(Commit::parse(self.format, object.data())?)
    }

    /// Reads and parses the commit `id` with [`ReadLimits::default`].
    ///
    /// Equivalent to `self.read_commit_with(id, ReadLimits::default())`; see
    /// [`Self::read_commit_with`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::read_commit_with`].
    pub fn read_commit(&self, id: ObjectId) -> Result<Commit, ReadCommitError> {
        self.read_commit_with(id, ReadLimits::default())
    }

    /// Reads and parses the tree `id`.
    ///
    /// Use this for a commit's [`Commit::tree`] or a tree entry with [`crate::EntryMode::Tree`].
    /// A missing object is an error, as with [`Self::read_commit`]. The canonical empty tree is
    /// present in every repository, as [`Self::read`] reports it.
    ///
    /// ```
    /// use girt::{EntryMode, InitKind, ObjectFormat, ReadLimits, Repository, Tree, TreeEntry};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let path = directory.path().join("repo.git");
    /// let repository = Repository::init(ObjectFormat::Sha1, path, InitKind::Bare)?;
    /// let loose = repository.loose_objects();
    /// let blob = loose.write_blob(b"hello\n")?;
    /// let entry = TreeEntry {
    ///     name: b"hello.txt".to_vec(),
    ///     mode: EntryMode::Blob,
    ///     id: blob,
    /// };
    /// let id = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![entry])?)?;
    ///
    /// let objects = repository.objects()?;
    /// let tree = objects.read_tree_with(id, ReadLimits::default())?;
    /// assert_eq!(tree.entries()[0].name, b"hello.txt");
    /// assert_eq!(objects.read_blob(tree.entries()[0].id)?, b"hello\n");
    /// assert!(objects.read_tree(blob).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`ReadTreeError::Missing`] if no store has `id`, [`ReadTreeError::Kind`] if `id`
    /// names another kind of object, [`ReadTreeError::Parse`] for a malformed tree, and
    /// [`ReadTreeError::Read`] for the failures of [`Self::read`].
    pub fn read_tree_with(&self, id: ObjectId, limits: ReadLimits) -> Result<Tree, ReadTreeError> {
        let object = match self.read_kind(id, ObjectKind::Tree, limits)? {
            Ok(object) => object,
            Err(None) => return Err(ReadTreeError::Missing(id)),
            Err(Some(kind)) => return Err(ReadTreeError::Kind { id, kind }),
        };
        Ok(Tree::parse(self.format, object.data())?)
    }

    /// Reads and parses the tree `id` with [`ReadLimits::default`].
    ///
    /// Equivalent to `self.read_tree_with(id, ReadLimits::default())`; see
    /// [`Self::read_tree_with`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::read_tree_with`].
    pub fn read_tree(&self, id: ObjectId) -> Result<Tree, ReadTreeError> {
        self.read_tree_with(id, ReadLimits::default())
    }

    /// Reads and parses the annotated tag `id`.
    ///
    /// A missing object is an error, as with [`Self::read_commit`]. Lightweight tags are
    /// references, not tag objects; resolve them through [`crate::refs`].
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, ObjectKind, ReadLimits, Repository, Tag, TagFields};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let path = directory.path().join("repo.git");
    /// let repository = Repository::init(ObjectFormat::Sha1, path, InitKind::Bare)?;
    /// let loose = repository.loose_objects();
    /// let blob = loose.write_blob(b"release notes\n")?;
    /// let id = loose.write_tag(&Tag::new(TagFields {
    ///     target: blob,
    ///     target_kind: ObjectKind::Blob,
    ///     name: b"v1.0".to_vec(),
    ///     tagger: None,
    ///     extra_headers: vec![],
    ///     message: b"first release\n".to_vec(),
    /// })?)?;
    ///
    /// let objects = repository.objects()?;
    /// let tag = objects.read_tag_with(id, ReadLimits::default())?;
    /// assert_eq!(tag.name(), b"v1.0");
    /// assert_eq!(objects.read_tag(id)?.target(), blob);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`ReadTagError::Missing`] if no store has `id`, [`ReadTagError::Kind`] if `id`
    /// names another kind of object, [`ReadTagError::Parse`] for a malformed tag, and
    /// [`ReadTagError::Read`] for the failures of [`Self::read`].
    pub fn read_tag_with(&self, id: ObjectId, limits: ReadLimits) -> Result<Tag, ReadTagError> {
        let object = match self.read_kind(id, ObjectKind::Tag, limits)? {
            Ok(object) => object,
            Err(None) => return Err(ReadTagError::Missing(id)),
            Err(Some(kind)) => return Err(ReadTagError::Kind { id, kind }),
        };
        Ok(Tag::parse(self.format, object.data())?)
    }

    /// Reads and parses the annotated tag `id` with [`ReadLimits::default`].
    ///
    /// Equivalent to `self.read_tag_with(id, ReadLimits::default())`; see [`Self::read_tag_with`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::read_tag_with`].
    pub fn read_tag(&self, id: ObjectId) -> Result<Tag, ReadTagError> {
        self.read_tag_with(id, ReadLimits::default())
    }

    /// Reads the blob `id` and returns its bytes.
    ///
    /// Blob bytes need no parsing. A missing object is an error, as with [`Self::read_commit`].
    /// The bytes are copied out of the reader's delta base cache when it shares them; borrow
    /// [`Object::data`] from [`Self::read`] to avoid that copy. See [`Self::read_tree_with`] for an
    /// example.
    ///
    /// # Errors
    ///
    /// Returns [`ReadBlobError::Missing`] if no store has `id`, [`ReadBlobError::Kind`] if `id`
    /// names another kind of object, and [`ReadBlobError::Read`] for the failures of
    /// [`Self::read`].
    pub fn read_blob_with(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> Result<Vec<u8>, ReadBlobError> {
        match self.read_kind(id, ObjectKind::Blob, limits)? {
            Ok(object) => Ok(object.into_data()),
            Err(None) => Err(ReadBlobError::Missing(id)),
            Err(Some(kind)) => Err(ReadBlobError::Kind { id, kind }),
        }
    }

    /// Reads the blob `id` with [`ReadLimits::default`] and returns its bytes.
    ///
    /// Equivalent to `self.read_blob_with(id, ReadLimits::default())`; see
    /// [`Self::read_blob_with`]. The default limits refuse blobs over 64 MiB; pass larger
    /// [`ReadLimits`] to [`Self::read_blob_with`] when the repository is trusted to hold them.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::read_blob_with`].
    pub fn read_blob(&self, id: ObjectId) -> Result<Vec<u8>, ReadBlobError> {
        self.read_blob_with(id, ReadLimits::default())
    }
}

/// [`Objects::read_commit`] failed to produce a commit.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadCommitError {
    /// No store has the object.
    #[error("commit {0} is missing")]
    Missing(ObjectId),
    /// The object exists but is not a commit.
    #[error("object {id} is a {kind:?}, not a commit")]
    Kind {
        /// The requested object.
        id: ObjectId,
        /// The kind it has.
        kind: ObjectKind,
    },
    /// The commit bytes are malformed.
    #[error(transparent)]
    Parse(#[from] CommitError),
    /// Reading the object failed.
    #[error(transparent)]
    Read(#[from] ObjectReadError),
}

/// [`Objects::read_tree`] failed to produce a tree.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadTreeError {
    /// No store has the object.
    #[error("tree {0} is missing")]
    Missing(ObjectId),
    /// The object exists but is not a tree.
    #[error("object {id} is a {kind:?}, not a tree")]
    Kind {
        /// The requested object.
        id: ObjectId,
        /// The kind it has.
        kind: ObjectKind,
    },
    /// The tree bytes are malformed.
    #[error(transparent)]
    Parse(#[from] TreeError),
    /// Reading the object failed.
    #[error(transparent)]
    Read(#[from] ObjectReadError),
}

/// [`Objects::read_tag`] failed to produce an annotated tag.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadTagError {
    /// No store has the object.
    #[error("tag {0} is missing")]
    Missing(ObjectId),
    /// The object exists but is not an annotated tag.
    #[error("object {id} is a {kind:?}, not a tag")]
    Kind {
        /// The requested object.
        id: ObjectId,
        /// The kind it has.
        kind: ObjectKind,
    },
    /// The tag bytes are malformed.
    #[error(transparent)]
    Parse(#[from] TagError),
    /// Reading the object failed.
    #[error(transparent)]
    Read(#[from] ObjectReadError),
}

/// [`Objects::read_blob`] failed to produce blob bytes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadBlobError {
    /// No store has the object.
    #[error("blob {0} is missing")]
    Missing(ObjectId),
    /// The object exists but is not a blob.
    #[error("object {id} is a {kind:?}, not a blob")]
    Kind {
        /// The requested object.
        id: ObjectId,
        /// The kind it has.
        kind: ObjectKind,
    },
    /// Reading the object failed.
    #[error(transparent)]
    Read(#[from] ObjectReadError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InitKind, ObjectFormat, Repository};

    /// A bare repository holding one blob, so every typed read has a wrong-kind target.
    fn repository_with_blob() -> (tempfile::TempDir, Repository, ObjectId) {
        let directory = tempfile::tempdir().unwrap();
        let repository = Repository::init(
            ObjectFormat::Sha1,
            directory.path().join("r.git"),
            InitKind::Bare,
        )
        .unwrap();
        let blob = repository.loose_objects().write_blob(b"bytes\n").unwrap();
        (directory, repository, blob)
    }

    /// Each typed read reports the stored kind instead of parsing another kind's bytes.
    #[test]
    fn wrong_kind_reports_the_stored_kind() {
        let (_directory, repository, blob) = repository_with_blob();
        let objects = repository.objects().unwrap();
        assert!(matches!(
            objects.read_commit(blob),
            Err(ReadCommitError::Kind {
                kind: ObjectKind::Blob,
                ..
            })
        ));
        assert!(matches!(
            objects.read_tree(blob),
            Err(ReadTreeError::Kind {
                kind: ObjectKind::Blob,
                ..
            })
        ));
        assert!(matches!(
            objects.read_tag(blob),
            Err(ReadTagError::Kind {
                kind: ObjectKind::Blob,
                ..
            })
        ));
    }

    #[test]
    fn blob_read_rejects_a_tree() {
        let (_directory, repository, _) = repository_with_blob();
        let tree = repository
            .loose_objects()
            .write_tree(&Tree::new(ObjectFormat::Sha1, vec![]).unwrap())
            .unwrap();
        let objects = repository.objects().unwrap();
        assert!(matches!(
            objects.read_blob(tree),
            Err(ReadBlobError::Kind {
                kind: ObjectKind::Tree,
                ..
            })
        ));
    }

    /// Unlike `read`, absence is an error naming the requested identity.
    #[test]
    fn missing_object_is_an_error() {
        let (_directory, repository, _) = repository_with_blob();
        let objects = repository.objects().unwrap();
        let absent = ObjectId::for_blob(ObjectFormat::Sha1, b"never written");
        assert!(matches!(
            objects.read_blob(absent),
            Err(ReadBlobError::Missing(id)) if id == absent
        ));
        assert!(matches!(
            objects.read_tree(absent),
            Err(ReadTreeError::Missing(id)) if id == absent
        ));
        assert!(matches!(
            objects.read_tag(absent),
            Err(ReadTagError::Missing(id)) if id == absent
        ));
    }

    /// The canonical empty tree reads as present without being stored, as `read` reports it.
    #[test]
    fn unstored_empty_tree_reads_as_empty() {
        let (_directory, repository, _) = repository_with_blob();
        let objects = repository.objects().unwrap();
        let empty = ObjectFormat::Sha1.hash_object(ObjectKind::Tree, b"");
        assert!(objects.read_tree(empty).unwrap().entries().is_empty());
    }

    #[test]
    fn malformed_tree_is_a_parse_error() {
        let (_directory, repository, _) = repository_with_blob();
        let id = repository
            .loose_objects()
            .write_object(ObjectFormat::Sha1, ObjectKind::Tree, b"not a tree entry")
            .unwrap();
        let objects = repository.objects().unwrap();
        assert!(matches!(
            objects.read_tree(id),
            Err(ReadTreeError::Parse(_))
        ));
    }

    /// The explicit limits reach the read: a bound below the payload size refuses it.
    #[test]
    fn explicit_limits_apply() {
        let (_directory, repository, blob) = repository_with_blob();
        let objects = repository.objects().unwrap();
        let limits = ReadLimits::default().with_max_object_bytes(2);
        let error = objects.read_blob_with(blob, limits).unwrap_err();
        assert!(
            matches!(
                error,
                ReadBlobError::Read(ObjectReadError::Loose(crate::loose::Error::TooLarge))
            ),
            "{error:?}"
        );
        assert_eq!(objects.read_blob(blob).unwrap(), b"bytes\n");
    }
}
