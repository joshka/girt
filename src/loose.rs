use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use flate2::write::ZlibEncoder;
use flate2::{Compression, Decompress, FlushDecompress, Status};
use tempfile::NamedTempFile;

use crate::ObjectId;
use crate::object::blob_header;

/// Reads and writes loose blobs beneath an explicitly selected SHA-1 object directory.
///
/// This owns the path to an object directory, usually `.git/objects`. Callers select the directory
/// and supply its known object format; no repository configuration is inspected.
///
/// # Example
///
/// ```
/// use girt::{LooseObjects, ObjectId};
///
/// // A disposable object directory; no repository discovery or Git executable is needed.
/// let directory = tempfile::tempdir()?;
/// let objects = LooseObjects::new(directory.path(), "sha1")?;
/// let bytes = b"hello\0Git\xff";
/// let id = objects.write_blob(bytes)?;
/// assert_eq!(id, ObjectId::for_blob(bytes));
/// assert_eq!(objects.read_blob(id, 1024)?, bytes);
/// # Ok::<(), girt::Error>(())
/// ```
///
///
/// # Supported storage
///
/// Only SHA-1 loose blobs are supported. SHA-256 and other object types are rejected. Packs and
/// alternates are not searched, so a packed-only blob appears missing. Reads validate canonical
/// headers, lengths, zlib completion and checksum, and object identity. The caller supplies a
/// maximum blob size to bound decompressed allocation. Writes compare existing content before
/// accepting a duplicate and publish complete files without replacing existing objects.
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
    /// `object_format` must be `sha1`, as determined by the caller. A directory alone carries no
    /// object-format metadata; passing `sha1` does not validate repository configuration.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedFormat`] for any other format, including `sha256`.
    pub fn new(directory: impl Into<PathBuf>, object_format: &str) -> Result<Self, Error> {
        if object_format != "sha1" {
            return Err(Error::UnsupportedFormat(object_format.to_owned()));
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
        if kind != b"blob" {
            return Err(Error::UnsupportedObjectType);
        }
        let content = &encoded[separator + 1..];
        if length != content.len().to_string().as_bytes() {
            return Err(Error::Corrupt("noncanonical or mismatched length"));
        }
        if content.len() > max_size {
            return Err(Error::TooLarge);
        }
        if ObjectId::for_blob(content) != id {
            return Err(Error::Corrupt("object identity mismatch"));
        }
        encoded.drain(..separator + 1);
        Ok(encoded)
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
        let id = ObjectId::for_blob(bytes);
        let path = self.object_path(id);
        let parent = path.parent().expect("object path always has a parent");
        fs::create_dir_all(parent)?;
        let mut temporary = NamedTempFile::new_in(parent)?;
        let mut encoder = ZlibEncoder::new(temporary.as_file_mut(), Compression::default());
        encoder.write_all(blob_header(bytes.len()).as_bytes())?;
        encoder.write_all(bytes)?;
        encoder.finish()?;
        match fs::hard_link(temporary.path(), &path) {
            Ok(()) => Ok(id),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if self.read_blob(id, bytes.len())? != bytes {
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

/// A failure to read or publish a loose blob.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem operation failed. Missing loose objects use [`std::io::ErrorKind::NotFound`].
    Io(std::io::Error),
    /// The caller selected an object format other than `sha1`.
    UnsupportedFormat(String),
    /// A loose object names a type other than `blob`.
    UnsupportedObjectType,
    /// The zlib stream, header, length, or requested identity is invalid.
    Corrupt(&'static str),
    /// Decompressed content exceeds the caller's maximum blob size.
    TooLarge,
    /// An existing valid object has the same identity but different content.
    ConflictingObject,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "loose object I/O: {error}"),
            Self::UnsupportedFormat(format) => write!(f, "unsupported object format: {format}"),
            Self::UnsupportedObjectType => f.write_str("only blob objects are supported"),
            Self::Corrupt(reason) => write!(f, "corrupt loose object: {reason}"),
            Self::TooLarge => f.write_str("blob exceeds the size limit"),
            Self::ConflictingObject => f.write_str("existing object has different content"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
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
