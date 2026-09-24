use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use flate2::write::ZlibEncoder;
use flate2::{Compression, Decompress, FlushDecompress, Status};
use tempfile::NamedTempFile;

use crate::object::object_header;
use crate::{Commit, CommitError, ObjectFormat, ObjectId, Tag, TagError, Tree, TreeError};

/// Reads and writes loose blobs, trees, commits, and tags beneath an explicitly selected SHA-1
/// object directory.
///
/// This owns the path to an object directory, usually `.git/objects`. Callers select the directory
/// and supply its known object format; no repository configuration is inspected. Operations are
/// synchronous and block the calling thread.
///
/// # Example
///
/// ```
/// use girt::{LooseObjects, ObjectFormat, ObjectId};
///
/// // A disposable object directory; no repository discovery or Git executable is needed.
/// let directory = tempfile::tempdir()?;
/// let objects = LooseObjects::new(directory.path(), ObjectFormat::Sha1)?;
/// let bytes = b"hello\0Git\xff";
/// let id = objects.write_blob(bytes)?;
/// assert_eq!(id, ObjectId::for_blob(bytes));
/// assert_eq!(objects.read_blob(id, 1024)?, bytes);
/// # Ok::<(), girt::Error>(())
/// ```
///
/// # Supported storage
///
/// Only SHA-1 loose blobs, trees, commits, and tags are supported. SHA-256 is rejected.
/// Packs and alternates are not searched, so a packed-only object appears missing. Reads validate
/// canonical headers, lengths, zlib completion and checksum, and object identity. The caller
/// supplies a maximum payload size to bound decompressed allocation. Writes compare existing
/// content before accepting a duplicate and publish complete files without replacing existing
/// objects.
///
/// # Filesystem assumptions
///
/// The object directory and its ancestors must be trusted. This API does not protect against
/// symlink replacement or hostile concurrent filesystem mutation. Writes require hard-link support
/// and do not promise power-loss durability or honor Git's shared-repository permission settings.
#[derive(Clone, Debug)]
pub struct LooseObjects {
    directory: PathBuf,
}

impl LooseObjects {
    /// Selects an object directory without accessing or creating it.
    ///
    /// The caller supplies the known object format. A directory alone carries no object-format
    /// metadata; passing [`ObjectFormat::Sha1`] does not validate repository configuration.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedFormat`] for [`ObjectFormat::Sha256`].
    pub fn new(directory: impl Into<PathBuf>, object_format: ObjectFormat) -> Result<Self, Error> {
        if object_format != ObjectFormat::Sha1 {
            return Err(Error::UnsupportedFormat(object_format));
        }
        Ok(Self {
            directory: directory.into(),
        })
    }

    /// Reads a loose blob, validating its encoding and identity before returning owned content.
    ///
    /// `max_size` limits blob bytes, excluding the header. Decompression retains at most that limit
    /// plus a small header and fixed-size scratch buffer. Compressed input is read incrementally.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] for filesystem failures (including missing objects),
    /// [`Error::TooLarge`] for oversized content, [`Error::UnsupportedObjectType`] for other
    /// types, or [`Error::Corrupt`] for malformed headers, incomplete or trailing compressed
    /// data, or identity mismatches.
    pub fn read_blob(&self, id: ObjectId, max_size: usize) -> Result<Vec<u8>, Error> {
        self.read_object(id, max_size, "blob")
    }

    /// Reads and parses a loose tree after verifying its framing and SHA-1 identity.
    ///
    /// Preserves all payloads supported by [`Tree::parse`], including invalid names, duplicates,
    /// and unsorted entries. Call [`Tree::validate`] separately when those rules are required.
    /// References are not resolved or checked against storage.
    ///
    /// `max_size` bounds decompressed payload bytes, excluding the header. Parsing additionally
    /// allocates owned entries and names proportional to that payload; this is not a total heap
    /// limit. Compressed input is read incrementally.
    ///
    /// # Errors
    ///
    /// Returns the storage errors documented by [`Self::read_blob`], including
    /// [`Error::UnsupportedObjectType`] for a non-tree object, or [`Error::Tree`] when the
    /// verified payload cannot be parsed.
    pub fn read_tree(&self, id: ObjectId, max_size: usize) -> Result<Tree, Error> {
        let payload = self.read_object(id, max_size, "tree")?;
        Ok(Tree::parse(&payload)?)
    }

    /// Reads a loose commit after verifying framing, size, zlib completion, and SHA-1 identity.
    ///
    /// Preserves the exact supported payload; does not call [`Commit::validate`] or resolve IDs.
    /// `max_size` bounds decompressed payload bytes, not total heap use. Parsing additionally owns
    /// the payload and decoded fields. Compressed input is read incrementally.
    ///
    /// # Errors
    ///
    /// Returns the storage errors of [`Self::read_blob`], including wrong object type, or
    /// [`Error::Commit`] if the verified payload cannot be parsed.
    pub fn read_commit(&self, id: ObjectId, max_size: usize) -> Result<Commit, Error> {
        let payload = self.read_object(id, max_size, "commit")?;
        Ok(Commit::parse(&payload)?)
    }

    /// Reads a loose tag after verifying framing, size, zlib completion, and SHA-1 identity.
    ///
    /// Preserves supported payloads without calling [`Tag::validate`] or resolving targets.
    /// `max_size` bounds payload bytes, not total heap use; parsing owns the payload and fields.
    /// Compressed input is read incrementally.
    ///
    /// # Errors
    ///
    /// Returns the storage errors of [`Self::read_blob`], including wrong object type, or
    /// [`Error::Tag`] if the verified payload cannot be parsed.
    pub fn read_tag(&self, id: ObjectId, max_size: usize) -> Result<Tag, Error> {
        let payload = self.read_object(id, max_size, "tag")?;
        Ok(Tag::parse(&payload)?)
    }

    fn read_object(
        &self,
        id: ObjectId,
        max_size: usize,
        expected_kind: &str,
    ) -> Result<Vec<u8>, Error> {
        Ok(self.decode_raw(id, max_size, Some(expected_kind))?.data)
    }

    pub(crate) fn read_raw(&self, id: ObjectId, max_size: usize) -> Result<crate::Object, Error> {
        self.decode_raw(id, max_size, None)
    }

    fn decode_raw(
        &self,
        id: ObjectId,
        max_size: usize,
        expected_kind: Option<&str>,
    ) -> Result<crate::Object, Error> {
        let file = File::open(self.object_path(id))?;
        let mut encoded = decompress(file, max_size.saturating_add(32))?;
        let separator = encoded
            .iter()
            .position(|&byte| byte == 0)
            .ok_or(Error::Corrupt("missing header terminator"))?;
        let header = &encoded[..separator];
        let space = header
            .iter()
            .position(|&byte| byte == b' ')
            .ok_or(Error::Corrupt("invalid header"))?;
        let kind = &header[..space];
        let length = &header[space + 1..];
        if expected_kind.is_some_and(|expected| kind != expected.as_bytes()) {
            return Err(Error::UnsupportedObjectType);
        }
        let kind = match kind {
            b"blob" => crate::ObjectKind::Blob,
            b"tree" => crate::ObjectKind::Tree,
            b"commit" => crate::ObjectKind::Commit,
            b"tag" => crate::ObjectKind::Tag,
            _ => return Err(Error::UnsupportedObjectType),
        };
        let content = &encoded[separator + 1..];
        if length != content.len().to_string().as_bytes() {
            return Err(Error::Corrupt("noncanonical or mismatched length"));
        }
        if content.len() > max_size {
            return Err(Error::TooLarge);
        }
        if ObjectId::for_object(kind.as_str(), content) != id {
            return Err(Error::Corrupt("object identity mismatch"));
        }
        encoded.drain(..separator + 1);
        Ok(crate::Object {
            kind,
            data: encoded,
        })
    }

    /// Writes exact blob bytes and returns their SHA-1 identity.
    ///
    /// Creates missing directories, compresses to a private temporary file in the destination
    /// directory, then publishes with a hard link that cannot overwrite an existing path.
    /// Concurrent writers of identical content can succeed. Existing objects are read and
    /// compared; corrupt or conflicting objects are left untouched. Temporary files are removed
    /// on normal error paths.
    ///
    /// # Errors
    ///
    /// Returns read errors when an existing object cannot be validated,
    /// [`Error::ConflictingObject`] when existing content differs, or [`Error::Io`] on
    /// filesystem/compression failures (including filesystems without hard-link support).
    /// Failed operations may leave created directories. No directory synchronization is
    /// performed, so success does not guarantee power-loss durability.
    pub fn write_blob(&self, bytes: &[u8]) -> Result<ObjectId, Error> {
        self.write_object("blob", bytes)
    }

    /// Writes a tree's exact encoded payload and returns its SHA-1 identity.
    ///
    /// Preserves parsed entries even when [`Tree::validate`] would reject them. Does not validate
    /// names, reorder entries, or resolve references. Use [`Tree::new`] to construct a validated
    /// tree. Allocates a payload buffer before compression.
    ///
    /// Publication, duplicate comparison, concurrency, cleanup, and filesystem assumptions are
    /// identical to [`Self::write_blob`].
    ///
    /// # Errors
    ///
    /// Returns the storage errors documented by [`Self::write_blob`]. Existing corrupt objects
    /// remain untouched; failed writes may leave created directories.
    pub fn write_tree(&self, tree: &Tree) -> Result<ObjectId, Error> {
        self.write_object("tree", &tree.encode())
    }

    /// Writes a commit's exact payload and returns its SHA-1 identity without normalizing it.
    ///
    /// Does not validate fields, resolve references, or verify signatures. Use [`Commit::new`] for
    /// construction validation. Publication, duplicate checking, concurrent writers, cleanup, and
    /// filesystem assumptions are identical to [`Self::write_blob`].
    ///
    /// # Errors
    ///
    /// Returns the storage errors of [`Self::write_blob`]. Existing corrupt files remain untouched;
    /// failed writes may leave created directories. Success does not promise power-loss durability.
    pub fn write_commit(&self, commit: &Commit) -> Result<ObjectId, Error> {
        self.write_object("commit", commit.as_bytes())
    }

    /// Writes a tag's exact payload and returns its SHA-1 identity without normalization.
    ///
    /// Does not validate fields, resolve targets, verify signatures, or create a tag reference.
    /// Uses the publication, concurrency, and cleanup behavior of [`Self::write_blob`].
    ///
    /// # Errors
    ///
    /// Returns the same storage failures as [`Self::write_blob`]. Failed writes can leave created
    /// directories; existing corrupt files are preserved and temporary files are cleaned up.
    pub fn write_tag(&self, tag: &Tag) -> Result<ObjectId, Error> {
        self.write_object("tag", tag.as_bytes())
    }

    fn write_object(&self, kind: &str, bytes: &[u8]) -> Result<ObjectId, Error> {
        let id = ObjectId::for_object(kind, bytes);
        let path = self.object_path(id);
        let parent = path.parent().expect("object path always has a parent");
        fs::create_dir_all(parent)?;
        let mut temporary = NamedTempFile::new_in(parent)?;
        let mut encoder = ZlibEncoder::new(temporary.as_file_mut(), Compression::default());
        encoder.write_all(object_header(kind, bytes.len()).as_bytes())?;
        encoder.write_all(bytes)?;
        encoder.finish()?;
        match fs::hard_link(temporary.path(), &path) {
            Ok(()) => Ok(id),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if self.read_object(id, bytes.len(), kind)? != bytes {
                    return Err(Error::ConflictingObject);
                }
                Ok(id)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn object_path(&self, id: ObjectId) -> PathBuf {
        let hex = id.to_string();
        self.directory.join(&hex[..2]).join(&hex[2..])
    }
}

/// A failure to read or publish a loose object.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem operation failed. Missing loose objects use [`std::io::ErrorKind::NotFound`].
    #[error("loose object I/O: {0}")]
    Io(#[from] std::io::Error),
    /// The caller selected a recognized object format that loose storage does not support.
    #[error("unsupported object format: {0}")]
    UnsupportedFormat(ObjectFormat),
    /// A loose object has a different type from the requested operation.
    #[error("object type does not match the requested operation")]
    UnsupportedObjectType,
    /// The zlib stream, header, length, or requested identity is invalid.
    #[error("corrupt loose object: {0}")]
    Corrupt(&'static str),
    /// Decompressed content exceeds the caller's maximum payload size.
    #[error("object exceeds the size limit")]
    TooLarge,
    /// The verified tree payload cannot be parsed; preserves the underlying parsing failure.
    #[error("invalid tree payload: {0}")]
    Tree(#[from] TreeError),
    /// The verified commit payload cannot be parsed; preserves the underlying cause.
    #[error("invalid commit payload: {0}")]
    Commit(#[from] CommitError),
    /// The verified tag payload cannot be parsed; preserves the underlying cause.
    #[error("invalid tag payload: {0}")]
    Tag(#[from] TagError),
    /// An existing valid object has the same identity but different content.
    #[error("existing object has different content")]
    ConflictingObject,
}

// Use the low-level inflater to require StreamEnd explicitly. A Read wrapper alone can accept EOF
// before the zlib trailer; validating content length would not detect that truncation.
fn decompress(file: File, limit: usize) -> Result<Vec<u8>, Error> {
    let mut reader = BufReader::new(file);
    let mut inflater = Decompress::new(true);
    let mut encoded = Vec::new();
    let mut output = [0; 8192];
    loop {
        let input = reader.fill_buf()?;
        let before_in = inflater.total_in();
        let before_out = inflater.total_out();
        let status = inflater
            .decompress(input, &mut output, FlushDecompress::None)
            .map_err(|_| Error::Corrupt("invalid zlib stream"))?;
        let consumed = (inflater.total_in() - before_in) as usize;
        let produced = (inflater.total_out() - before_out) as usize;
        reader.consume(consumed);
        if produced > limit.saturating_sub(encoded.len()) {
            return Err(Error::TooLarge);
        }
        encoded.extend_from_slice(&output[..produced]);
        if status == Status::StreamEnd {
            if !reader.fill_buf()?.is_empty() {
                return Err(Error::Corrupt("trailing compressed data"));
            }
            return Ok(encoded);
        }
        if consumed == 0 && produced == 0 {
            return Err(Error::Corrupt("incomplete zlib stream"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construction selects a path lazily: reading a missing object fails without creating storage.
    #[test]
    fn selects_sha1_without_creating_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("objects");
        let objects = LooseObjects::new(&directory, ObjectFormat::Sha1).unwrap();

        let id = ObjectId::for_blob(b"");
        assert!(matches!(objects.read_blob(id, 0), Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound));
        assert!(!directory.exists());
    }

    /// Unsupported SHA-256 selection reports the format and leaves the filesystem unchanged.
    #[test]
    fn rejects_sha256_without_creating_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("objects");
        let error = LooseObjects::new(&directory, ObjectFormat::Sha256).unwrap_err();

        assert!(matches!(
            error,
            Error::UnsupportedFormat(ObjectFormat::Sha256)
        ));
        assert_eq!(error.to_string(), "unsupported object format: sha256");
        assert!(!directory.exists());
    }

    fn tree_fixture() -> Tree {
        let tree = Tree::new(vec![crate::TreeEntry {
            mode: crate::EntryMode::Blob,
            name: b"file".to_vec(),
            id: ObjectId::for_blob(b"contents"),
        }]);
        tree.unwrap()
    }

    /// Installs independently framed bytes to test failures before tree parsing.
    fn install(objects: &LooseObjects, id: ObjectId, encoded: &[u8]) {
        let path = objects.object_path(id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(encoded).unwrap();
        fs::write(path, encoder.finish().unwrap()).unwrap();
    }

    #[rstest::rstest]
    #[case::empty(Tree::new(vec![]).unwrap())]
    #[case::one_entry(tree_fixture())]
    fn reads_tree_at_exact_payload_limit(#[case] tree: Tree) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let id = objects.write_tree(&tree).unwrap();
        assert_eq!(id, tree.id());
        assert_eq!(objects.read_tree(id, tree.encode().len()).unwrap(), tree);
    }

    #[test]
    fn rejects_tree_one_byte_over_limit() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let tree = tree_fixture();
        let id = objects.write_tree(&tree).unwrap();
        assert!(matches!(
            objects.read_tree(id, tree.encode().len() - 1),
            Err(Error::TooLarge)
        ));
    }

    #[test]
    fn bounds_tree_decompression_before_parsing() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let payload = vec![0; 20000];
        let id = ObjectId::for_object("tree", &payload);
        install(
            &objects,
            id,
            &[b"tree 20000\0".as_slice(), &payload].concat(),
        );
        assert!(matches!(objects.read_tree(id, 10), Err(Error::TooLarge)));
    }

    #[test]
    fn rejects_blob_through_tree_reader() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let id = objects.write_blob(b"").unwrap();
        assert!(matches!(
            objects.read_tree(id, 0),
            Err(Error::UnsupportedObjectType)
        ));
    }

    #[rstest::rstest]
    #[case::missing_terminator(b"tree 0")]
    #[case::missing_length(b"tree\0")]
    #[case::leading_zero(b"tree 00\0")]
    #[case::mismatched_length(b"tree 1\0")]
    #[case::wrong_identity(b"tree 1\0x")]
    fn rejects_corrupt_tree_framing(#[case] encoded: &[u8]) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let id = Tree::new(vec![]).unwrap().id();
        install(&objects, id, encoded);
        assert!(matches!(objects.read_tree(id, 100), Err(Error::Corrupt(_))));
    }

    #[rstest::rstest]
    #[case::missing_mode(b"100644", TreeError::MissingModeDelimiter)]
    #[case::missing_name(b"100644 a", TreeError::MissingNameDelimiter)]
    #[case::truncated_id(b"100644 a\0", TreeError::TruncatedObjectId)]
    fn reports_tree_parse_error_after_identity_verification(
        #[case] payload: &[u8],
        #[case] expected: TreeError,
    ) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let id = ObjectId::for_object("tree", payload);
        let encoded = [object_header("tree", payload.len()).as_bytes(), payload].concat();
        install(&objects, id, &encoded);
        let Error::Tree(error) = objects.read_tree(id, payload.len()).unwrap_err() else {
            panic!("expected tree parsing error");
        };
        assert_eq!(error, expected);
    }

    #[test]
    fn reports_missing_tree_without_creating_storage() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("objects");
        let objects = LooseObjects::new(&directory, ObjectFormat::Sha1).unwrap();
        assert!(
            matches!(objects.read_tree(tree_fixture().id(), 1024), Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(!directory.exists());
    }

    #[test]
    fn duplicate_tree_preserves_file_and_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let tree = tree_fixture();
        let id = objects.write_tree(&tree).unwrap();
        let path = objects.object_path(id);
        let original = fs::read(&path).unwrap();
        assert_eq!(objects.write_tree(&tree).unwrap(), id);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn tree_write_preserves_corrupt_file_and_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let tree = tree_fixture();
        let path = objects.object_path(tree.id());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"invalid zlib").unwrap();
        assert!(matches!(objects.write_tree(&tree), Err(Error::Corrupt(_))));
        assert_eq!(fs::read(&path).unwrap(), b"invalid zlib");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn concurrent_tree_writes_publish_one_object() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let tree = tree_fixture();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let objects = &objects;
                let tree = &tree;
                scope.spawn(move || assert_eq!(objects.write_tree(tree).unwrap(), tree.id()));
            }
        });
        let path = objects.object_path(tree.id());
        assert_eq!(objects.read_tree(tree.id(), 1024).unwrap(), tree);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn failed_tree_publication_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let tree = tree_fixture();
        let path = objects.object_path(tree.id());
        fs::create_dir_all(&path).unwrap();
        assert!(objects.write_tree(&tree).is_err());
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }
}

#[cfg(test)]
mod commit_storage_tests {
    use super::*;

    fn commit_fixture() -> Commit {
        Commit::parse(b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a> 1 +0000\ncommitter C <c> 2 -0700\n\nmessage\n").unwrap()
    }

    /// Installs independently framed bytes to test failures before commit parsing.
    fn install(objects: &LooseObjects, id: ObjectId, encoded: &[u8]) {
        let path = objects.object_path(id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(encoded).unwrap();
        fs::write(path, encoder.finish().unwrap()).unwrap();
    }

    #[rstest::rstest]
    #[case::root(commit_fixture())]
    fn reads_commit_at_exact_payload_limit(#[case] commit: Commit) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let id = objects.write_commit(&commit).unwrap();
        assert_eq!(id, commit.id());
        assert_eq!(
            objects.read_commit(id, commit.encode().len()).unwrap(),
            commit
        );
    }

    #[test]
    fn rejects_commit_one_byte_over_limit() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let commit = commit_fixture();
        let id = objects.write_commit(&commit).unwrap();
        assert!(matches!(
            objects.read_commit(id, commit.encode().len() - 1),
            Err(Error::TooLarge)
        ));
    }

    #[test]
    fn bounds_commit_decompression_before_parsing() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let payload = vec![0; 20000];
        let id = ObjectId::for_object("commit", &payload);
        install(
            &objects,
            id,
            &[b"commit 20000\0".as_slice(), &payload].concat(),
        );
        assert!(matches!(objects.read_commit(id, 10), Err(Error::TooLarge)));
    }

    #[test]
    fn rejects_blob_through_commit_reader() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let id = objects.write_blob(b"").unwrap();
        assert!(matches!(
            objects.read_commit(id, 0),
            Err(Error::UnsupportedObjectType)
        ));
    }

    #[rstest::rstest]
    #[case::missing_terminator(b"commit 0")]
    #[case::missing_length(b"commit\0")]
    #[case::leading_zero(b"commit 00\0")]
    #[case::mismatched_length(b"commit 1\0")]
    #[case::wrong_identity(b"commit 1\0x")]
    fn rejects_corrupt_commit_framing(#[case] encoded: &[u8]) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let id = commit_fixture().id();
        install(&objects, id, encoded);
        assert!(matches!(
            objects.read_commit(id, 100),
            Err(Error::Corrupt(_))
        ));
    }

    #[rstest::rstest]
    #[case::separator(b"tree abc", CommitError::MissingSeparator)]
    #[case::identity(b"tree abc\n\n", CommitError::InvalidObjectId)]
    fn reports_commit_parse_error_after_identity_verification(
        #[case] payload: &[u8],
        #[case] expected: CommitError,
    ) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let id = ObjectId::for_object("commit", payload);
        let encoded = [object_header("commit", payload.len()).as_bytes(), payload].concat();
        install(&objects, id, &encoded);
        let Error::Commit(error) = objects.read_commit(id, payload.len()).unwrap_err() else {
            panic!("expected commit parsing error");
        };
        assert_eq!(error, expected);
    }

    #[test]
    fn reports_missing_commit_without_creating_storage() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("objects");
        let objects = LooseObjects::new(&directory, ObjectFormat::Sha1).unwrap();
        assert!(
            matches!(objects.read_commit(commit_fixture().id(), 1024), Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(!directory.exists());
    }

    #[test]
    fn duplicate_commit_preserves_file_and_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let commit = commit_fixture();
        let id = objects.write_commit(&commit).unwrap();
        let path = objects.object_path(id);
        let original = fs::read(&path).unwrap();
        assert_eq!(objects.write_commit(&commit).unwrap(), id);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn commit_write_preserves_corrupt_file_and_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let commit = commit_fixture();
        let path = objects.object_path(commit.id());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"invalid zlib").unwrap();
        assert!(matches!(
            objects.write_commit(&commit),
            Err(Error::Corrupt(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), b"invalid zlib");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn concurrent_commit_writes_publish_one_object() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let commit = commit_fixture();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let objects = &objects;
                let commit = &commit;
                scope.spawn(move || assert_eq!(objects.write_commit(commit).unwrap(), commit.id()));
            }
        });
        let path = objects.object_path(commit.id());
        assert_eq!(objects.read_commit(commit.id(), 1024).unwrap(), commit);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn failed_commit_publication_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let commit = commit_fixture();
        let path = objects.object_path(commit.id());
        fs::create_dir_all(&path).unwrap();
        assert!(objects.write_commit(&commit).is_err());
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }
}

#[cfg(test)]
mod tag_storage_tests {
    use super::*;

    fn tag_fixture() -> Tag {
        Tag::parse(b"object e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\ntype blob\ntag v1\ntagger A <a> 1 +0000\n\nmessage\n").unwrap()
    }

    /// Installs independently framed bytes to test failures before tag parsing.
    fn install(objects: &LooseObjects, id: ObjectId, encoded: &[u8]) {
        let path = objects.object_path(id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(encoded).unwrap();
        fs::write(path, encoder.finish().unwrap()).unwrap();
    }

    #[rstest::rstest]
    #[case::annotated(tag_fixture())]
    fn reads_tag_at_exact_payload_limit(#[case] tag: Tag) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let id = objects.write_tag(&tag).unwrap();
        assert_eq!(id, tag.id());
        assert_eq!(objects.read_tag(id, tag.encode().len()).unwrap(), tag);
    }

    #[test]
    fn rejects_tag_one_byte_over_limit() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let tag = tag_fixture();
        let id = objects.write_tag(&tag).unwrap();
        assert!(matches!(
            objects.read_tag(id, tag.encode().len() - 1),
            Err(Error::TooLarge)
        ));
    }

    #[test]
    fn bounds_tag_decompression_before_parsing() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let payload = vec![0; 20000];
        let id = ObjectId::for_object("tag", &payload);
        install(
            &objects,
            id,
            &[b"tag 20000\0".as_slice(), &payload].concat(),
        );
        assert!(matches!(objects.read_tag(id, 10), Err(Error::TooLarge)));
    }

    #[test]
    fn rejects_blob_through_tag_reader() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let id = objects.write_blob(b"").unwrap();
        assert!(matches!(
            objects.read_tag(id, 0),
            Err(Error::UnsupportedObjectType)
        ));
    }

    #[rstest::rstest]
    #[case::missing_terminator(b"tag 0")]
    #[case::missing_length(b"tag\0")]
    #[case::leading_zero(b"tag 00\0")]
    #[case::mismatched_length(b"tag 1\0")]
    #[case::wrong_identity(b"tag 1\0x")]
    fn rejects_corrupt_tag_framing(#[case] encoded: &[u8]) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let id = tag_fixture().id();
        install(&objects, id, encoded);
        assert!(matches!(objects.read_tag(id, 100), Err(Error::Corrupt(_))));
    }

    #[rstest::rstest]
    #[case::separator(b"object abc", TagError::UnterminatedHeader)]
    #[case::identity(b"object abc\n\n", TagError::InvalidObjectId)]
    fn reports_tag_parse_error_after_identity_verification(
        #[case] payload: &[u8],
        #[case] expected: TagError,
    ) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let id = ObjectId::for_object("tag", payload);
        let encoded = [object_header("tag", payload.len()).as_bytes(), payload].concat();
        install(&objects, id, &encoded);
        let Error::Tag(error) = objects.read_tag(id, payload.len()).unwrap_err() else {
            panic!("expected tag parsing error");
        };
        assert_eq!(error, expected);
    }

    #[test]
    fn reports_missing_tag_without_creating_storage() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("objects");
        let objects = LooseObjects::new(&directory, ObjectFormat::Sha1).unwrap();
        assert!(
            matches!(objects.read_tag(tag_fixture().id(), 1024), Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(!directory.exists());
    }

    #[test]
    fn duplicate_tag_preserves_file_and_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let tag = tag_fixture();
        let id = objects.write_tag(&tag).unwrap();
        let path = objects.object_path(id);
        let original = fs::read(&path).unwrap();
        assert_eq!(objects.write_tag(&tag).unwrap(), id);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn tag_write_preserves_corrupt_file_and_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let tag = tag_fixture();
        let path = objects.object_path(tag.id());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"invalid zlib").unwrap();
        assert!(matches!(objects.write_tag(&tag), Err(Error::Corrupt(_))));
        assert_eq!(fs::read(&path).unwrap(), b"invalid zlib");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn concurrent_tag_writes_publish_one_object() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let tag = tag_fixture();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let objects = &objects;
                let tag = &tag;
                scope.spawn(move || assert_eq!(objects.write_tag(tag).unwrap(), tag.id()));
            }
        });
        let path = objects.object_path(tag.id());
        assert_eq!(objects.read_tag(tag.id(), 1024).unwrap(), tag);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn failed_tag_publication_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1).unwrap();
        let tag = tag_fixture();
        let path = objects.object_path(tag.id());
        fs::create_dir_all(&path).unwrap();
        assert!(objects.write_tag(&tag).is_err());
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }
}
