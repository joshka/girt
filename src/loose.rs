//! Read and publish individual compressed Git objects.
//!
//! Use [`LooseObjects`] when writing individual blobs, trees, commits, and annotated tags, or when
//! inspecting loose storage specifically. Each file contains a zlib-compressed Git header and
//! payload. Its path is derived from its identity. For general reads use [`crate::Objects`], which
//! also searches packs and configured alternates. A loose-only read can report absence after a
//! repository is packed.
//!
//! # Store bytes without a complete repository
//!
//! ```
//! use girt::{LooseObjects, ObjectFormat};
//! let directory = tempfile::tempdir()?;
//! let objects = LooseObjects::new(directory.path(), ObjectFormat::Sha256);
//! let id = objects.write_blob(b"binary\0payload\xff")?;
//! assert_eq!(objects.read_blob(id, 1024)?, b"binary\0payload\xff");
//! # Ok::<(), girt::loose::Error>(())
//! ```
//!
//! Supply the correct format: the object directory itself carries no format metadata. Repository
//! callers should obtain this handle from [`crate::Repository::loose_objects`]. Writes publish
//! objects without changing refs or the working tree. Duplicate writes compare existing content;
//! they do not silently overwrite a conflicting object.
//!
//! # Errors and filesystem assumptions
//!
//! [`Error`] distinguishes I/O, malformed framing, wrong kind, hash mismatch, size limits, and
//! payload parsing. A payload-size refusal calls for an explicit budget decision; a hash mismatch
//! calls for investigation of storage, not a larger limit. For a missing packed-only object, use
//! the combined reader. Typed reads parse payloads but retain each codec's separate validation
//! contract.
//!
//! Operations block the calling thread. Reads allocate up to the selected payload limit; writes
//! encode and compress caller bytes. The directory and ancestors must be trusted against hostile
//! replacement. Publishing requires hard links, does not promise power-loss durability, and does
//! not implement Git's shared-repository permission policy. See [`LooseObjects`] for the full
//! storage contract.

#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;

use flate2::write::ZlibEncoder;
use flate2::{Compression, Decompress, FlushDecompress, Status};
use tempfile::NamedTempFile;

use crate::object::object_header;
use crate::{
    Commit, CommitError, ObjectFormat, ObjectId, ObjectKind, Tag, TagError, Tree, TreeError,
};

/// Reads and writes loose blobs, trees, commits, and tags beneath an explicitly selected
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
/// let objects = LooseObjects::new(directory.path(), ObjectFormat::Sha1);
/// let bytes = b"hello\0Git\xff";
/// let id = objects.write_blob(bytes)?;
/// assert_eq!(id, ObjectId::for_blob(girt::ObjectFormat::Sha1, bytes));
/// assert_eq!(objects.read_blob(id, 1024)?, bytes);
/// # Ok::<(), girt::loose::Error>(())
/// ```
///
/// # Supported storage
///
/// SHA-1 and SHA-256 loose blobs, trees, commits, and tags are supported.
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
    format: ObjectFormat,
}

impl LooseObjects {
    /// Selects an object directory without accessing or creating it.
    ///
    /// The caller supplies the known object format. A directory alone carries no object-format
    /// metadata; passing [`ObjectFormat::Sha1`] does not validate repository configuration.
    ///
    /// Both closed object formats are supported; construction performs no I/O.
    ///
    /// ```
    /// let directory = tempfile::tempdir()?;
    /// let objects = girt::LooseObjects::new(directory.path(), girt::ObjectFormat::Sha1);
    /// let id = objects.write_blob(b"standalone storage")?;
    /// assert_eq!(objects.read_blob(id, 1024)?, b"standalone storage");
    /// # Ok::<(), girt::loose::Error>(())
    /// ```
    pub fn new(directory: impl Into<PathBuf>, object_format: ObjectFormat) -> Self {
        Self {
            directory: directory.into(),
            format: object_format,
        }
    }

    /// The explicitly selected storage format.
    pub fn object_format(&self) -> ObjectFormat {
        self.format
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
    /// recognized types, [`Error::UnknownObjectType`] for unknown kinds, or [`Error::Corrupt`]
    /// for malformed headers, incomplete or trailing compressed
    /// data, or identity mismatches.
    /// Foreign-format IDs return [`Error::ObjectFormat`] before any filesystem access.
    ///
    /// ```
    /// let directory = tempfile::tempdir()?;
    /// let objects = girt::LooseObjects::new(directory.path(), girt::ObjectFormat::Sha1);
    /// let id = objects.write_blob(b"hello\0Git")?;
    /// assert_eq!(objects.read_blob(id, 1024)?, b"hello\0Git");
    /// // A size refusal leaves the stored object intact.
    /// assert!(objects.read_blob(id, 1).is_err());
    /// # Ok::<(), girt::loose::Error>(())
    /// ```
    pub fn read_blob(&self, id: ObjectId, max_size: usize) -> Result<Vec<u8>, Error> {
        self.read_object(id, max_size, "blob")
    }

    /// Reads and parses a loose tree after verifying its framing and selected-format identity.
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
    ///
    /// ```
    /// let directory = tempfile::tempdir()?;
    /// let objects = girt::LooseObjects::new(directory.path(), girt::ObjectFormat::Sha1);
    /// let tree = girt::Tree::new(girt::ObjectFormat::Sha1, vec![])?;
    /// let id = objects.write_tree(&tree)?;
    /// assert_eq!(objects.read_tree(id, 1024)?.id(), tree.id());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read_tree(&self, id: ObjectId, max_size: usize) -> Result<Tree, Error> {
        let payload = self.read_object(id, max_size, "tree")?;
        Ok(Tree::parse(self.format, &payload)?)
    }

    /// Reads a loose commit after verifying framing, size, zlib completion, and selected-format
    /// identity.
    ///
    /// Preserves the exact supported payload; does not call [`Commit::validate`] or resolve IDs.
    /// `max_size` bounds decompressed payload bytes, not total heap use. Parsing additionally owns
    /// the payload and decoded fields. Compressed input is read incrementally.
    ///
    /// # Errors
    ///
    /// Returns the storage errors of [`Self::read_blob`], including wrong object type, or
    /// [`Error::Commit`] if the verified payload cannot be parsed.
    ///
    /// ```
    /// use girt::{Commit, CommitFields, ObjectFormat, Signature, Tree};
    /// let person = Signature {
    ///     name: b"A. Writer".to_vec(),
    ///     email: b"a@example.com".to_vec(),
    ///     seconds: 1_700_000_000,
    ///     offset_minutes: 0,
    /// };
    /// let commit = Commit::new(CommitFields {
    ///     tree: Tree::new(ObjectFormat::Sha1, vec![])?.id(),
    ///     parents: vec![],
    ///     author: person.clone(),
    ///     committer: person,
    ///     extra_headers: vec![],
    ///     message: b"Initial snapshot\n".to_vec(),
    /// })?;
    /// let directory = tempfile::tempdir()?;
    /// let objects = girt::LooseObjects::new(directory.path(), girt::ObjectFormat::Sha1);
    /// let id = objects.write_commit(&commit)?;
    /// assert_eq!(
    ///     objects.read_commit(id, 4096)?.message(),
    ///     b"Initial snapshot\n"
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read_commit(&self, id: ObjectId, max_size: usize) -> Result<Commit, Error> {
        let payload = self.read_object(id, max_size, "commit")?;
        Ok(Commit::parse(self.format, &payload)?)
    }

    /// Reads a loose tag after verifying framing, size, zlib completion, and selected-format
    /// identity.
    ///
    /// Preserves supported payloads without calling [`Tag::validate`] or resolving targets.
    /// `max_size` bounds payload bytes, not total heap use; parsing owns the payload and fields.
    /// Compressed input is read incrementally.
    ///
    /// # Errors
    ///
    /// Returns the storage errors of [`Self::read_blob`], including wrong object type, or
    /// [`Error::Tag`] if the verified payload cannot be parsed.
    ///
    /// ```
    /// use girt::{ObjectFormat, ObjectId, ObjectKind, Tag, TagFields};
    /// let tag = Tag::new(TagFields {
    ///     target: ObjectId::for_blob(ObjectFormat::Sha1, b"manifest"),
    ///     target_kind: ObjectKind::Blob,
    ///     name: b"v1".to_vec(),
    ///     tagger: None,
    ///     extra_headers: vec![],
    ///     message: b"Release manifest\n".to_vec(),
    /// })?;
    /// let directory = tempfile::tempdir()?;
    /// let objects = girt::LooseObjects::new(directory.path(), girt::ObjectFormat::Sha1);
    /// let id = objects.write_tag(&tag)?;
    /// assert_eq!(objects.read_tag(id, 4096)?.target(), tag.target());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read_tag(&self, id: ObjectId, max_size: usize) -> Result<Tag, Error> {
        let payload = self.read_object(id, max_size, "tag")?;
        Ok(Tag::parse(self.format, &payload)?)
    }

    fn read_object(
        &self,
        id: ObjectId,
        max_size: usize,
        expected_kind: &str,
    ) -> Result<Vec<u8>, Error> {
        Ok(self
            .decode_raw(id, max_size, Some(expected_kind))?
            .into_data())
    }

    pub(crate) fn read_raw(&self, id: ObjectId, max_size: usize) -> Result<crate::Object, Error> {
        self.decode_raw(id, max_size, None)
    }

    /// Locates a retention candidate by its canonical header without decoding its payload.
    pub(crate) fn locate(&self, id: ObjectId) -> Result<(ObjectKind, u64), Error> {
        id.require_format(self.format)?;
        let file = File::open(self.object_path(id))?;
        let mut decoder = flate2::read::ZlibDecoder::new(file);
        let mut header = Vec::new();
        for _ in 0..32 {
            let mut byte = [0];
            decoder.read_exact(&mut byte)?;
            if byte[0] == 0 {
                let text = std::str::from_utf8(&header)
                    .map_err(|_| Error::Corrupt("invalid loose header"))?;
                let (kind, length) = text
                    .split_once(' ')
                    .ok_or(Error::Corrupt("invalid loose header"))?;
                let kind = match kind {
                    "blob" => ObjectKind::Blob,
                    "tree" => ObjectKind::Tree,
                    "commit" => ObjectKind::Commit,
                    "tag" => ObjectKind::Tag,
                    _ => return Err(Error::UnknownObjectType),
                };
                let size: u64 = length
                    .parse()
                    .map_err(|_| Error::Corrupt("invalid loose size"))?;
                if size.to_string() != length {
                    return Err(Error::Corrupt("noncanonical loose size"));
                }
                return Ok((kind, size));
            }
            header.push(byte[0]);
        }
        Err(Error::Corrupt("loose header limit"))
    }

    fn decode_raw(
        &self,
        id: ObjectId,
        max_size: usize,
        expected_kind: Option<&str>,
    ) -> Result<crate::Object, Error> {
        #[cfg(feature = "tracing")]
        let span = tracing::trace_span!(
            target: "girt",
            "loose.read",
            object_format = %self.format,
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
            max_bytes = max_size,
        );

        let operation = || {
            id.require_format(self.format)?;
            let file = File::open(self.object_path(id))?;
            let mut encoded = decompress(file, max_size.saturating_add(32))?;
            let (kind, content) = split_header(&encoded, expected_kind)?;
            if content.len() > max_size {
                return Err(Error::TooLarge);
            }
            if self.format.hash_object(kind, content) != id {
                return Err(Error::Corrupt("object identity mismatch"));
            }
            let header_len = encoded.len() - content.len();
            encoded.drain(..header_len);
            Ok(crate::Object {
                kind,
                format: self.format,
                data: encoded.into(),
            })
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::loose);

        result
    }

    /// Writes exact blob bytes and returns their selected-format identity.
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
    ///
    /// ```
    /// use girt::{LooseObjects, ObjectFormat, ObjectId};
    /// let directory = tempfile::tempdir()?;
    /// let objects = LooseObjects::new(directory.path(), ObjectFormat::Sha1);
    /// let first = objects.write_blob(b"hello\0Git")?;
    /// let second = objects.write_blob(b"hello\0Git")?;
    /// assert_eq!(first, second); // Publishing identical content is idempotent.
    /// assert_eq!(first, ObjectId::for_blob(ObjectFormat::Sha1, b"hello\0Git"));
    /// assert_ne!(first, objects.write_blob(b"hello Git")?);
    /// # Ok::<(), girt::loose::Error>(())
    /// ```
    pub fn write_blob(&self, bytes: &[u8]) -> Result<ObjectId, Error> {
        self.write_object(self.format, ObjectKind::Blob, bytes)
    }

    /// Writes a tree's exact encoded payload and returns its selected-format identity.
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
    /// Returns [`Error::ObjectFormat`] for a foreign-format tree before filesystem access.
    /// Returns the storage errors documented by [`Self::write_blob`]. Existing corrupt objects
    /// remain untouched; failed writes may leave created directories.
    ///
    /// ```
    /// use girt::{EntryMode, LooseObjects, ObjectFormat, Tree, TreeEntry};
    /// let directory = tempfile::tempdir()?;
    /// let objects = LooseObjects::new(directory.path(), ObjectFormat::Sha1);
    /// let blob = objects.write_blob(b"notes\n")?;
    /// let tree = Tree::new(
    ///     ObjectFormat::Sha1,
    ///     vec![TreeEntry {
    ///         mode: EntryMode::Blob,
    ///         name: b"notes.txt".to_vec(),
    ///         id: blob,
    ///     }],
    /// )?;
    /// let id = objects.write_tree(&tree)?;
    /// assert_eq!(id, tree.id()); // Child storage precedes publishing a complete snapshot.
    /// assert_eq!(objects.read_tree(id, 4096)?.entries()[0].id, blob);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn write_tree(&self, tree: &Tree) -> Result<ObjectId, Error> {
        self.write_object(tree.object_format(), ObjectKind::Tree, &tree.encode())
    }

    /// Writes a commit's exact payload and returns its selected-format identity without normalizing
    /// it.
    ///
    /// Does not validate fields, resolve references, or verify signatures. Use [`Commit::new`] for
    /// construction validation. Publication, duplicate checking, concurrent writers, cleanup, and
    /// filesystem assumptions are identical to [`Self::write_blob`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectFormat`] for a foreign-format commit before filesystem access.
    /// Returns the storage errors of [`Self::write_blob`]. Existing corrupt files remain untouched;
    /// failed writes may leave created directories. Success does not promise power-loss durability.
    ///
    /// ```
    /// # use girt::{Commit, CommitFields, ObjectFormat, Signature, Tree};
    /// # let person = Signature { name: b"A. Writer".to_vec(), email: b"a@example.com".to_vec(),
    /// #                          seconds: 1_700_000_000, offset_minutes: 0 };
    /// # let commit = Commit::new(CommitFields {
    /// #     tree: Tree::new(ObjectFormat::Sha1, vec![])?.id(), parents: vec![],
    /// #     author: person.clone(), committer: person,
    /// #     extra_headers: vec![], message: b"Initial snapshot\n".to_vec(),
    /// # })?;
    /// let directory = tempfile::tempdir()?;
    /// let objects = girt::LooseObjects::new(directory.path(), ObjectFormat::Sha1);
    /// let id = objects.write_commit(&commit)?;
    /// assert_eq!(id, commit.id());
    /// // Storing a commit neither creates its referenced tree nor publishes a branch.
    /// assert!(objects.read_tree(commit.tree(), 4096).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn write_commit(&self, commit: &Commit) -> Result<ObjectId, Error> {
        self.write_object(
            commit.object_format(),
            ObjectKind::Commit,
            commit.as_bytes(),
        )
    }

    /// Writes a tag's exact payload and returns its selected-format identity without normalization.
    ///
    /// Does not validate fields, resolve targets, verify signatures, or create a tag reference.
    /// Uses the publication, concurrency, and cleanup behavior of [`Self::write_blob`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectFormat`] for a foreign-format tag before filesystem access.
    /// Returns the same storage failures as [`Self::write_blob`]. Failed writes can leave created
    /// directories; existing corrupt files are preserved and temporary files are cleaned up.
    ///
    /// ```
    /// # use girt::{ObjectFormat, ObjectId, ObjectKind, Tag, TagFields};
    /// # let tag = Tag::new(TagFields {
    /// #     target: ObjectId::for_blob(ObjectFormat::Sha1, b"manifest"),
    /// #     target_kind: ObjectKind::Blob, name: b"v1".to_vec(), tagger: None,
    /// #     extra_headers: vec![], message: b"Release manifest\n".to_vec(),
    /// # })?;
    /// let directory = tempfile::tempdir()?;
    /// let objects = girt::LooseObjects::new(directory.path(), ObjectFormat::Sha1);
    /// let id = objects.write_tag(&tag)?;
    /// assert_eq!(id, tag.id());
    /// // No reference is created, and the target is not fetched or written implicitly.
    /// assert!(objects.read_blob(tag.target(), 4096).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn write_tag(&self, tag: &Tag) -> Result<ObjectId, Error> {
        self.write_object(tag.object_format(), ObjectKind::Tag, tag.as_bytes())
    }

    pub(crate) fn write_object(
        &self,
        format: ObjectFormat,
        kind: ObjectKind,
        bytes: &[u8],
    ) -> Result<ObjectId, Error> {
        #[cfg(feature = "tracing")]
        let span = tracing::trace_span!(
            target: "girt",
            "loose.write",
            object_format = %self.format,
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
            bytes = bytes.len(),
        );

        let operation = || {
            ObjectId::null(format).require_format(self.format)?;
            let id = self.format.hash_object(kind, bytes);
            let path = self.object_path(id);
            let parent = path
                .parent()
                .ok_or(Error::Corrupt("object path has no parent directory"))?;
            fs::create_dir_all(parent)?;
            let mut temporary = NamedTempFile::new_in(parent)?;
            let mut encoder = ZlibEncoder::new(temporary.as_file_mut(), Compression::default());
            encoder.write_all(object_header(kind.as_str(), bytes.len()).as_bytes())?;
            encoder.write_all(bytes)?;
            encoder.finish()?;
            match fs::hard_link(temporary.path(), &path) {
                Ok(()) => Ok(id),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if self.read_object(id, bytes.len(), kind.as_str())? != bytes {
                        return Err(Error::ConflictingObject);
                    }
                    Ok(id)
                }
                Err(error) => Err(error.into()),
            }
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::loose);

        result
    }

    /// Reports whether a loose file exists for `id`, without reading it.
    pub(crate) fn contains(&self, id: ObjectId) -> Result<bool, Error> {
        id.require_format(self.format)?;
        match fs::metadata(self.object_path(id)) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn object_path(&self, id: ObjectId) -> PathBuf {
        let hex = id.to_string();
        self.directory.join(&hex[..2]).join(&hex[2..])
    }
}

/// A failure to read or publish a loose object.
///
/// A wrong-kind refusal means the object exists but the chosen typed operation is unsuitable.
/// Inspect its kind through [`crate::Objects::read`] before selecting a parser. A loose
/// [`Self::Io`] with [`std::io::ErrorKind::NotFound`] can mean the object is packed;
/// [`crate::Objects`] searches both representations. Preserve corruption and other I/O causes
/// rather than converting them to absence.
///
/// ```
/// use girt::loose::Error;
/// use girt::{InitKind, ObjectFormat, ObjectKind, PackLimits, ReadLimits, Repository};
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r.git"),
///     InitKind::Bare,
/// )?;
/// let loose = repo.loose_objects();
/// let id = loose.write_blob(b"release notes\n")?;
/// assert!(matches!(
///     loose.read_commit(id, 4096),
///     Err(Error::UnsupportedObjectType)
/// ));
/// let object = repo
///     .objects(PackLimits::default())?
///     .read(id, ReadLimits::default())?
///     .ok_or("object absent")?;
/// assert_eq!(object.kind(), ObjectKind::Blob);
/// assert_eq!(object.data(), b"release notes\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An identity or structured object differs from this store's format.
    /// Use the store format for both requested IDs and structured payloads.
    #[error(transparent)]
    ObjectFormat(#[from] crate::ObjectFormatError),
    /// A filesystem operation failed. Missing loose objects use [`std::io::ErrorKind::NotFound`].
    /// Inspect the operating-system source. For NotFound, try the combined Objects reader if the
    /// object may be packed.
    #[error("loose object I/O")]
    Io(#[from] std::io::Error),
    /// A loose object has a different type from the requested operation.
    /// Choose the typed read matching the stored kind; do not parse a blob as another object.
    #[error("object type does not match the requested operation")]
    UnsupportedObjectType,
    /// The stored header names an unrecognized Git object kind.
    /// Report unsupported stored framing; ordinary Git object kinds are blob, tree, commit, and
    /// tag.
    #[error("unrecognized loose object type")]
    UnknownObjectType,
    /// The zlib stream, header, length, or requested identity is invalid.
    /// Retain the damaged object for diagnosis and recover verified bytes from a trusted source.
    #[error("corrupt loose object: {0}")]
    Corrupt(&'static str),
    /// Decompressed content exceeds the caller's maximum payload size.
    /// Choose a larger payload budget only after assessing trust and resource cost.
    #[error("object exceeds the size limit")]
    TooLarge,
    /// The verified tree payload cannot be parsed; preserves the underlying parsing failure.
    /// Inspect the nested tree parser error; storage identity verification did not validate its
    /// payload.
    #[error("invalid tree payload")]
    Tree(#[from] TreeError),
    /// The verified commit payload cannot be parsed; preserves the underlying cause.
    /// Inspect the nested commit parser error; recover graph-readable data without inventing
    /// fields.
    #[error("invalid commit payload")]
    Commit(#[from] CommitError),
    /// The verified tag payload cannot be parsed; preserves the underlying cause.
    /// Inspect the nested tag parser error and preserve the original bytes.
    #[error("invalid tag payload")]
    Tag(#[from] TagError),
    /// An existing valid object has the same identity but different content.
    /// Stop publication and inspect both copies. The writer refuses to overwrite an existing
    /// conflicting object.
    #[error("existing object has different content")]
    ConflictingObject,
}

// Use the low-level inflater to require StreamEnd explicitly. A Read wrapper alone can accept EOF
// before the zlib trailer; validating content length would not detect that truncation.
/// Splits an inflated loose object into its kind and exactly sized, canonically declared payload.
///
/// A kind other than `expected_kind` is reported before any length error.
pub(crate) fn split_header<'a>(
    encoded: &'a [u8],
    expected_kind: Option<&str>,
) -> Result<(ObjectKind, &'a [u8]), Error> {
    let (header, content) =
        crate::bytes::split_once(encoded, 0).ok_or(Error::Corrupt("missing header terminator"))?;
    let (kind, length) =
        crate::bytes::split_once(header, b' ').ok_or(Error::Corrupt("invalid header"))?;
    let kind = match kind {
        b"blob" => ObjectKind::Blob,
        b"tree" => ObjectKind::Tree,
        b"commit" => ObjectKind::Commit,
        b"tag" => ObjectKind::Tag,
        _ => return Err(Error::UnknownObjectType),
    };
    if expected_kind.is_some_and(|expected| kind.as_str() != expected) {
        return Err(Error::UnsupportedObjectType);
    }
    if length != content.len().to_string().as_bytes() {
        return Err(Error::Corrupt("noncanonical or mismatched length"));
    }
    Ok((kind, content))
}

pub(crate) fn decompress(file: impl Read, limit: usize) -> Result<Vec<u8>, Error> {
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
        encoded.extend_from_slice(
            output
                .get(..produced)
                .ok_or(Error::Corrupt("invalid zlib stream"))?,
        );
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

    #[test]
    fn distinguishes_unknown_header_kind_from_wrong_requested_kind() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let id = ObjectId::for_blob(ObjectFormat::Sha1, b"");
        install(&objects, id, b"future 0\0");
        assert!(matches!(
            objects.read_blob(id, 100),
            Err(Error::UnknownObjectType)
        ));
        assert!(matches!(
            objects.read_raw(id, 100),
            Err(Error::UnknownObjectType)
        ));
    }

    #[rstest::rstest]
    #[case::no_terminator(b"blob 0".as_slice(), "missing header terminator")]
    #[case::no_space(b"blob\0", "invalid header")]
    #[case::empty_header(b"\0", "invalid header")]
    fn rejects_header_without_terminator_or_space(#[case] encoded: &[u8], #[case] expected: &str) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let id = ObjectId::for_blob(ObjectFormat::Sha1, b"");
        install(&objects, id, encoded);
        assert!(matches!(
            objects.read_raw(id, 100),
            Err(Error::Corrupt(message)) if message == expected
        ));
    }

    /// Construction selects a path lazily: reading a missing object fails without creating storage.
    #[test]
    fn selects_sha1_without_creating_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("objects");
        let objects = LooseObjects::new(&directory, ObjectFormat::Sha1);

        let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"");
        assert!(matches!(objects.read_blob(id, 0), Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound));
        assert!(!directory.exists());
    }

    #[test]
    fn selects_sha256_without_creating_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("objects");
        let objects = LooseObjects::new(&directory, ObjectFormat::Sha256);
        assert_eq!(objects.object_format(), ObjectFormat::Sha256);
        assert!(!directory.exists());
    }

    fn tree_fixture() -> Tree {
        let tree = Tree::new(
            crate::ObjectFormat::Sha1,
            vec![crate::TreeEntry {
                mode: crate::EntryMode::Blob,
                name: b"file".to_vec(),
                id: ObjectId::for_blob(crate::ObjectFormat::Sha1, b"contents"),
            }],
        );
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
    #[case::empty(Tree::new(crate::ObjectFormat::Sha1, vec![]).unwrap())]
    #[case::one_entry(tree_fixture())]
    fn reads_tree_at_exact_payload_limit(#[case] tree: Tree) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let id = objects.write_tree(&tree).unwrap();
        assert_eq!(id, tree.id());
        assert_eq!(objects.read_tree(id, tree.encode().len()).unwrap(), tree);
    }

    #[test]
    fn rejects_tree_one_byte_over_limit() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let id = objects.write_blob(b"").unwrap();
        assert!(matches!(
            objects.read_tree(id, 0),
            Err(Error::UnsupportedObjectType)
        ));
    }

    #[test]
    fn reports_unexpected_kind_before_malformed_length() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let id = ObjectId::for_object("blob", b"x");
        install(&objects, id, b"blob 9\0x");
        assert!(matches!(
            objects.read_tree(id, 10),
            Err(Error::UnsupportedObjectType)
        ));
        assert!(matches!(objects.read_blob(id, 10), Err(Error::Corrupt(_))));
    }

    #[rstest::rstest]
    #[case::missing_terminator(b"tree 0")]
    #[case::missing_length(b"tree\0")]
    #[case::leading_zero(b"tree 00\0")]
    #[case::mismatched_length(b"tree 1\0")]
    #[case::wrong_identity(b"tree 1\0x")]
    fn rejects_corrupt_tree_framing(#[case] encoded: &[u8]) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let id = Tree::new(crate::ObjectFormat::Sha1, vec![]).unwrap().id();
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(&directory, ObjectFormat::Sha1);
        assert!(
            matches!(objects.read_tree(tree_fixture().id(), 1024), Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(!directory.exists());
    }

    #[test]
    fn duplicate_tree_preserves_file_and_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        Commit::parse(crate::ObjectFormat::Sha1, b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a> 1 +0000\ncommitter C <c> 2 -0700\n\nmessage\n").unwrap()
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let id = commit_fixture().id();
        install(&objects, id, encoded);
        assert!(matches!(
            objects.read_commit(id, 100),
            Err(Error::Corrupt(_))
        ));
    }

    #[rstest::rstest]
    #[case::separator(b"tree abc", CommitError::RequiredHeader)]
    #[case::identity(b"tree abc\n\n", CommitError::InvalidObjectId)]
    fn reports_commit_parse_error_after_identity_verification(
        #[case] payload: &[u8],
        #[case] expected: CommitError,
    ) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(&directory, ObjectFormat::Sha1);
        assert!(
            matches!(objects.read_commit(commit_fixture().id(), 1024), Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(!directory.exists());
    }

    #[test]
    fn duplicate_commit_preserves_file_and_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        Tag::parse(crate::ObjectFormat::Sha1, b"object e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\ntype blob\ntag v1\ntagger A <a> 1 +0000\n\nmessage\n").unwrap()
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let id = objects.write_tag(&tag).unwrap();
        assert_eq!(id, tag.id());
        assert_eq!(objects.read_tag(id, tag.encode().len()).unwrap(), tag);
    }

    #[test]
    fn rejects_tag_one_byte_over_limit() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(&directory, ObjectFormat::Sha1);
        assert!(
            matches!(objects.read_tag(tag_fixture().id(), 1024), Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(!directory.exists());
    }

    #[test]
    fn duplicate_tag_preserves_file_and_cleans_temporary() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
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
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let tag = tag_fixture();
        let path = objects.object_path(tag.id());
        fs::create_dir_all(&path).unwrap();
        assert!(objects.write_tag(&tag).is_err());
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }
}

#[cfg(test)]
mod format_boundary_tests {
    use super::*;

    #[test]
    fn rejects_sha256_before_accessing_missing_storage() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("missing");
        let objects = LooseObjects::new(&path, ObjectFormat::Sha1);
        assert!(matches!(
            objects.read_blob(ObjectId::Sha256([1; 32]), 100),
            Err(Error::ObjectFormat(_))
        ));
        assert!(!path.exists());
    }
}

#[cfg(test)]
mod dual_format_tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1, ObjectFormat::Sha256)]
    #[case::sha256(ObjectFormat::Sha256, ObjectFormat::Sha1)]
    fn foreign_objects_fail_before_creating_storage(
        #[case] format: ObjectFormat,
        #[case] foreign: ObjectFormat,
    ) {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("objects");
        let objects = LooseObjects::new(&directory, format);
        let tree = Tree::new(foreign, vec![]).unwrap();
        let payload = format!(
            "tree {}\nauthor A <a> -1 +0000\ncommitter A <a> 1 +0000\n\n",
            tree.id()
        );
        let commit = Commit::parse(foreign, payload.as_bytes()).unwrap();
        let payload = format!("object {}\ntype commit\ntag v1\n\n", commit.id());
        let tag = Tag::parse(foreign, payload.as_bytes()).unwrap();
        assert!(matches!(
            objects.write_tree(&tree),
            Err(Error::ObjectFormat(_))
        ));
        assert!(matches!(
            objects.write_commit(&commit),
            Err(Error::ObjectFormat(_))
        ));
        assert!(matches!(
            objects.write_tag(&tag),
            Err(Error::ObjectFormat(_))
        ));
        assert!(matches!(
            objects.read_blob(ObjectId::null(foreign), 0),
            Err(Error::ObjectFormat(_))
        ));
        assert!(!directory.exists());
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn limits_duplicates_corruption_and_cleanup(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), format);
        let id = objects.write_blob(b"content").unwrap();
        let path = objects.object_path(id);
        let encoded = fs::read(&path).unwrap();
        assert_eq!(objects.read_blob(id, 7).unwrap(), b"content");
        assert!(matches!(objects.read_blob(id, 6), Err(Error::TooLarge)));
        assert_eq!(objects.write_blob(b"content").unwrap(), id);
        assert_eq!(fs::read(&path).unwrap(), encoded);
        fs::write(&path, b"corrupt existing object").unwrap();
        assert!(objects.write_blob(b"content").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"corrupt existing object");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn failed_publication_preserves_destination(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), format);
        let id = ObjectId::for_blob(format, b"content");
        let path = objects.object_path(id);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("sentinel"), b"keep").unwrap();
        assert!(objects.write_blob(b"content").is_err());
        assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"keep");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn concurrent_publication_returns_same_identity(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), format);
        let other = objects.clone();
        let worker = std::thread::spawn(move || other.write_blob(b"concurrent").unwrap());
        let id = objects.write_blob(b"concurrent").unwrap();
        assert_eq!(worker.join().unwrap(), id);
        assert_eq!(objects.read_blob(id, 10).unwrap(), b"concurrent");
        assert_eq!(
            fs::read_dir(objects.object_path(id).parent().unwrap())
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn foreign_commit_payload_is_not_reinterpreted() {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha256);
        let payload = format!(
            "tree {}\nauthor A <a> 1 +0000\ncommitter A <a> 1 +0000\n\n",
            ObjectId::Sha1([1; 20])
        );
        let id = objects
            .write_object(ObjectFormat::Sha256, ObjectKind::Commit, payload.as_bytes())
            .unwrap();
        assert!(matches!(
            objects.read_commit(id, 1000),
            Err(Error::Commit(CommitError::InvalidObjectId))
        ));
    }
    #[rstest::rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn retention_location_does_not_validate_blob_payload(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), format);
        let id = objects.write_blob(b"original").unwrap();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"blob 8\0different").unwrap();
        fs::write(objects.object_path(id), encoder.finish().unwrap()).unwrap();
        assert_eq!(objects.locate(id).unwrap(), (ObjectKind::Blob, 8));
        assert!(matches!(objects.read_blob(id, 100), Err(Error::Corrupt(_))));
    }

    #[rstest::rstest]
    #[case::leading_zero(b"blob 08\0original")]
    #[case::negative(b"blob -8\0original")]
    #[case::missing_kind(b" 8\0original")]
    #[case::unterminated(b"blob 8")]
    fn retention_location_rejects_malformed_headers(#[case] encoded: &[u8]) {
        let root = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let id = objects.write_blob(b"original").unwrap();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(encoded).unwrap();
        fs::write(objects.object_path(id), encoder.finish().unwrap()).unwrap();
        assert!(objects.locate(id).is_err());
    }
}
