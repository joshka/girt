use crate::refs::{RefName, Target};
use crate::{ObjectFormat, ObjectId};

/// A stored reference, including a deletion that hides older stack entries.
///
/// A tombstone must hide an older value during stack merging; dropping it from one table can
/// resurrect that value. Full-stack compaction can discard tombstones once every older table is
/// covered. A peeled target is meaningful only with a direct target.
///
/// ```
/// use girt::refs::reftable::{RecordName, RefRecord};
/// let deletion = RefRecord {
///     name: RecordName::new("refs/tags/old")?,
///     update_index: 2,
///     target: None,
///     peeled: None,
/// };
/// assert!(deletion.target.is_none());
/// # Ok::<(), girt::refs::reftable::Error>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefRecord {
    /// Full reference name.
    pub name: RecordName,
    /// Transaction that last changed this reference.
    pub update_index: u64,
    /// Stored target, or a deletion tombstone.
    pub target: Option<Target>,
    /// Optional peeled identity associated with a direct target.
    pub peeled: Option<ObjectId>,
}

/// Binary reflog fields without files-reflog lexical normalization.
///
/// ```
/// use girt::refs::reftable::{Limits, LogRecord, LogValue, RecordName, Table};
/// use girt::{ObjectFormat, ObjectId};
/// let format = ObjectFormat::Sha1;
/// let value = LogValue {
///     old: ObjectId::null(format),
///     new: ObjectId::for_blob(format, b"tagged"),
///     name: b"Writer".to_vec(),
///     email: b"writer@example.com".to_vec(),
///     seconds: u64::MAX,
///     offset_minutes: 330,
///     message: b"created\n".to_vec(),
/// };
/// let table = Table {
///     format,
///     min_update_index: 1,
///     max_update_index: 1,
///     references: vec![],
///     logs: vec![LogRecord {
///         name: RecordName::new("refs/tags/one")?,
///         update_index: 1,
///         value: Some(value.clone()),
///     }],
/// };
/// let decoded = Table::decode(&table.encode(Limits::default())?, Limits::default())?;
/// assert_eq!(decoded.logs[0].value, Some(value));
/// // The binary codec preserves unsigned seconds; do not coerce through Signature's signed range.
/// # Ok::<(), girt::refs::reftable::Error>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogValue {
    /// Previous identity, or zero for creation.
    pub old: ObjectId,
    /// Replacement identity, or zero for deletion.
    pub new: ObjectId,
    /// Exact committer name bytes.
    pub name: Vec<u8>,
    /// Exact committer email bytes, excluding angle brackets.
    pub email: Vec<u8>,
    /// Unsigned seconds since the Unix epoch.
    pub seconds: u64,
    /// Signed timezone offset in semantic minutes, converted from Git's stored decimal `HHMM`.
    /// Noncanonical minute digits are arithmetic; encoding normalizes them and requires a value
    /// in `-19679..=19679` so the canonical representation fits the signed two-byte field.
    pub offset_minutes: i16,
    /// Exact message bytes.
    pub message: Vec<u8>,
}

/// A reflog entry or deletion at one reference and transaction index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogRecord {
    /// Reference whose history contains the record.
    pub name: RecordName,
    /// Transaction index; records sort newest first within each name.
    pub update_index: u64,
    /// Record data, or a tombstone hiding this exact key in older tables.
    pub value: Option<LogValue>,
}

/// Decoded contents of one immutable table.
///
/// References sort by name bytes; logs sort by name then descending update index. Index and
/// object-to-reference accelerator blocks are validated while decoding but are not retained.
///
/// Use this codec when you own table bytes. Use [`crate::refs::References`] for repository
/// transactions; encoding a `Table` does not publish it or allocate a transaction index.
///
/// ```
/// use girt::refs::reftable::{Limits, RefRecord, Table};
/// use girt::refs::{RefName, Target};
/// use girt::{ObjectFormat, ObjectId};
/// let id = ObjectId::for_blob(ObjectFormat::Sha1, b"tag target");
/// let table = Table {
///     format: ObjectFormat::Sha1,
///     min_update_index: 1,
///     max_update_index: 1,
///     references: vec![RefRecord {
///         name: RefName::new("refs/tags/example")?.into(),
///         update_index: 1,
///         target: Some(Target::Direct(id)),
///         peeled: None,
///     }],
///     logs: vec![],
/// };
/// let bytes = table.encode(Limits::default())?;
/// assert_eq!(Table::decode(&bytes, Limits::default())?, table);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Table {
    /// Identity format of all records.
    pub format: ObjectFormat,
    /// Earliest transaction represented by this table.
    pub min_update_index: u64,
    /// Latest transaction represented by this table.
    pub max_update_index: u64,
    /// Stored references, including tombstones.
    pub references: Vec<RefRecord>,
    /// Stored reflog entries, including tombstones.
    pub logs: Vec<LogRecord>,
}

/// Allocation and processing bounds for decoding one table.
///
/// Limits apply before record allocation and decompression. The caller already owns the input
/// bytes. These are finite safety defaults, not measured acceptance thresholds for all consumers.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum encoded table length.
    pub bytes: usize,
    /// Maximum inflated size of any block, including its header.
    pub block_bytes: usize,
    /// Maximum total records, including index and object records.
    pub records: usize,
    /// Maximum reconstructed key or individual value string length.
    pub string_bytes: usize,
    /// Maximum aggregate reconstructed key and value bytes across records.
    pub decoded_bytes: usize,
}

impl Limits {
    /// Sets [`Self::bytes`].
    #[must_use]
    pub fn with_bytes(mut self, bytes: usize) -> Self {
        self.bytes = bytes;
        self
    }

    /// Sets [`Self::block_bytes`].
    #[must_use]
    pub fn with_block_bytes(mut self, block_bytes: usize) -> Self {
        self.block_bytes = block_bytes;
        self
    }

    /// Sets [`Self::records`].
    #[must_use]
    pub fn with_records(mut self, records: usize) -> Self {
        self.records = records;
        self
    }

    /// Sets [`Self::string_bytes`].
    #[must_use]
    pub fn with_string_bytes(mut self, string_bytes: usize) -> Self {
        self.string_bytes = string_bytes;
        self
    }

    /// Sets [`Self::decoded_bytes`].
    #[must_use]
    pub fn with_decoded_bytes(mut self, decoded_bytes: usize) -> Self {
        self.decoded_bytes = decoded_bytes;
        self
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            bytes: 64 * 1024 * 1024,
            block_bytes: 0xff_ffff,
            records: 1_000_000,
            string_bytes: 1024 * 1024,
            decoded_bytes: 128 * 1024 * 1024,
        }
    }
}

impl Limits {
    /// Returns limits for a repository the caller trusts as Git trusts the user's own.
    ///
    /// Git bounds none of these values. The defaults suit untrusted input; these are large enough
    /// that only available memory bounds the work, while keeping headroom for internal arithmetic.
    ///
    /// Block size stays at the format's 24-bit maximum.
    pub const fn trusted() -> Self {
        Self {
            bytes: usize::MAX / 4,
            block_bytes: 0xff_ffff,
            records: usize::MAX / 4,
            string_bytes: usize::MAX / 4,
            decoded_bytes: usize::MAX / 4,
        }
    }
}

/// Reftable framing, version, or resource failure.
///
/// This is the byte codec's error. Filesystem stack operations return
/// [`crate::refs::ReferenceError`] with path and source context. Treat malformed bytes as damaged
/// input; increasing a budget cannot repair framing or a checksum.
///
/// ```
/// use girt::refs::reftable::{Error, Limits, Table};
/// assert!(matches!(
///     Table::decode(b"WRNG", Limits::default()),
///     Err(Error::Malformed(_))
/// ));
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Invalid framing, ordering, checksum or record representation.
    #[error("malformed reftable: {0}")]
    Malformed(&'static str),
    /// Recognized boundary not supported by this codec.
    #[error("unsupported reftable: {0}")]
    Unsupported(&'static str),
    /// A caller-selected budget was exhausted before allocation or processing.
    #[error("reftable resource limit: {0}")]
    Limit(&'static str),
}

/// A table key name, including Git's uppercase pseudorefs.
///
/// Reference operations use [`RefName`]; the wider binary codec also preserves names such as
/// `ORIG_HEAD` while compacting a private worktree stack.
///
/// ```
/// use girt::refs::RefName;
/// use girt::refs::reftable::RecordName;
///
/// let record = RecordName::from(RefName::new(b"refs/heads/topic")?);
/// assert_eq!(record.as_ref(), b"refs/heads/topic");
/// let pseudoref = RecordName::new(b"ORIG_HEAD")?;
/// assert!(pseudoref.reference_name().is_none());
/// assert_eq!(pseudoref.into_bytes(), b"ORIG_HEAD");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct RecordName(Vec<u8>);

impl RecordName {
    /// Validates a full reference name or uppercase/underscore pseudoref.
    ///
    /// # Errors
    ///
    /// Rejects empty names, invalid full references and other one-level names.
    pub fn new(bytes: impl AsRef<[u8]>) -> Result<Self, Error> {
        let bytes = bytes.as_ref();
        if RefName::new(bytes).is_err()
            && (bytes.is_empty() || !bytes.iter().all(|b| b.is_ascii_uppercase() || *b == b'_'))
        {
            return Err(Error::Malformed("table record name"));
        }
        Ok(Self(bytes.to_vec()))
    }

    /// Returns the exact stored name bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns the exact name bytes, consuming the name without copying its buffer.
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Interprets a name within the public `HEAD`/`refs/` operation namespace.
    pub fn reference_name(&self) -> Option<RefName> {
        RefName::new(&self.0).ok()
    }
}

impl AsRef<[u8]> for RecordName {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl From<RefName> for RecordName {
    fn from(name: RefName) -> Self {
        Self(name.into_bytes())
    }
}

impl PartialEq<RefName> for RecordName {
    fn eq(&self, other: &RefName) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn reference_conversion_transfers_non_utf8_buffer() {
        let reference = RefName::new(b"refs/heads/\xff").unwrap();
        let pointer = reference.as_bytes().as_ptr();
        let record = RecordName::from(reference);
        assert_eq!(record.as_ref(), b"refs/heads/\xff");
        let bytes = record.into_bytes();
        assert_eq!(bytes.as_ptr(), pointer);
    }
}
