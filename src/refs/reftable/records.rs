use crate::refs::{RefName, Target};
use crate::{ObjectFormat, ObjectId};

/// A stored reference, including a deletion that hides older stack entries.
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
    /// Signed timezone offset in minutes.
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

/// Reftable framing, version, or resource failure.
#[derive(Debug, thiserror::Error)]
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
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
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

    /// Interprets a name within the public `HEAD`/`refs/` operation namespace.
    pub fn reference_name(&self) -> Option<RefName> {
        RefName::new(&self.0).ok()
    }
}

impl From<RefName> for RecordName {
    fn from(name: RefName) -> Self {
        Self(name.as_bytes().to_vec())
    }
}

impl PartialEq<RefName> for RecordName {
    fn eq(&self, other: &RefName) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}
