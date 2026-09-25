use crate::{
    ObjectId, ObjectKind, ObjectReadError, ReadLimits, TreeCompareError, TreeCompareLimits,
};

/// Source selection for inferred copies, in addition to deleted rename sources.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Copies {
    /// Each deleted source can supply at most one destination.
    #[default]
    Disabled,
    /// Also use preimages of modified regular/executable files and reuse deleted sources.
    /// Unchanged files are not candidates; mode-only changes count as modifications.
    Modified,
}

/// Caller choices for inference, independent of repository configuration.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Minimum retained-byte percentage, inclusive, from 1 through 100 (default 50).
    /// 100 admits only identical object IDs, bypassing approximate scoring.
    pub similarity: u8,
    /// Whether modified files and already-used deleted files can supply copies.
    pub copies: Copies,
    /// Include empty blobs as exact-match sources (default false).
    pub track_empty: bool,
    /// Maximum sources or remaining destinations for edited-content detection (default 1000).
    /// Exact matches are attempted first. Exceeding this limit returns a structured error,
    /// with no partial results; it never silently reports an incomplete inference as complete.
    /// Zero permits exact detection only if there are no remaining candidate pairs.
    pub candidate_limit: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            similarity: 50,
            copies: Copies::Disabled,
            track_empty: false,
            candidate_limit: 1000,
        }
    }
}

/// Resource bounds for a complete inference call, including its structural comparison.
///
/// Memory includes bounded tree comparison storage, retained blob bytes, at most one span per
/// payload byte, candidate pairs and output paths. Span keys have at most 64 bytes, with map-node
/// overhead; these bounds are not exact heap/RSS limits. Allocation failure follows Rust's
/// allocator behavior. No files are written and no handles remain owned by the result.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Structural comparison bounds, including path and change counts.
    pub trees: TreeCompareLimits,
    /// Independent reconstruction bounds for each blob read.
    pub read: ReadLimits,
    /// Aggregate distinct candidate blob bytes retained (default 64 MiB).
    pub max_blob_bytes: usize,
    /// Maximum total span occurrences across cached candidate blobs (default 1,000,000).
    pub max_spans: usize,
    /// Maximum candidate pairs scored or considered for exact selection (default 1,000,000).
    pub max_pairs: usize,
    /// Conservative byte/lookup/comparison work budget (default 100,000,000).
    pub max_work: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            trees: TreeCompareLimits::default(),
            read: ReadLimits::default(),
            max_blob_bytes: 64 * 1024 * 1024,
            max_spans: 1_000_000,
            max_pairs: 1_000_000,
            max_work: 100_000_000,
        }
    }
}

/// How an inferred source is used in this result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// The deleted source's first selected destination.
    Rename,
    /// A modified source or another destination reusing a deleted source.
    Copy,
}

/// One inferred relationship, ordered by destination raw path bytes in returned results.
///
/// Exact object IDs score 100. Edited content is split at LF or after 64 significant bytes;
/// equal spans contribute the minimum occurrence count times span length, divided by the larger
/// raw payload length. Ordering of spans is immaterial. For text (no NUL in the first 8000 bytes),
/// CR immediately preceding LF is omitted. Binary spans preserve CR and all other bytes.
/// Percentages are rounded down; threshold comparison uses the unrounded fraction.
///
/// This independent byte-equality heuristic follows observed Git span behavior, but does not
/// reproduce every observed Git score or pairing shortcut. The repeated-byte discrepancy
/// remains under investigation; its cause has not been established. A score of 100 for edited
/// content can mean reordered spans, not identical bytes. `Options::similarity == 100` requires
/// identity instead. No finite compatibility corpus establishes universal heuristic parity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rewrite {
    /// Original relative Git path bytes.
    pub source: Vec<u8>,
    /// Added relative Git path bytes.
    pub target: Vec<u8>,
    /// Original blob identity, including for a modified-source copy.
    pub source_id: ObjectId,
    /// Added blob identity.
    pub target_id: ObjectId,
    /// Rounded-down retained-byte percentage.
    pub similarity: u8,
    /// Rename or copy classification before target filtering.
    pub kind: Kind,
}

/// Inference failed without returning partial records or modifying the repository.
///
/// Callers may raise an exhausted bound or retry cancellation with a cleared flag. Missing/corrupt
/// storage needs repair or a refreshed object view; retrying unchanged inputs does not repair it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Similarity must be between 1 and 100 inclusive.
    #[error("rewrite similarity must be in 1..=100, got {0}")]
    Similarity(u8),
    /// Structural tree comparison failed.
    #[error("comparing rewrite trees: {0}")]
    Trees(#[from] TreeCompareError),
    /// A candidate blob could not be read or verified.
    #[error("reading rewrite blob {id}: {source}")]
    Read {
        /// Candidate identity.
        id: ObjectId,
        /// Storage failure, preserving its recovery classification.
        #[source]
        source: ObjectReadError,
    },
    /// Candidate object is missing.
    #[error("missing rewrite blob {0}")]
    Missing(ObjectId),
    /// Candidate object has a non-blob kind.
    #[error("rewrite object {id} is {actual:?}, not a blob")]
    NotBlob {
        /// Candidate identity.
        id: ObjectId,
        /// Verified object kind.
        actual: ObjectKind,
    },
    /// Edited detection would exceed the requested candidate limit after exact matching.
    #[error("rewrite candidates ({sources} sources, {targets} targets) exceed limit {limit}")]
    Candidates {
        /// Eligible sources.
        sources: usize,
        /// Unpaired destinations.
        targets: usize,
        /// Caller-specified limit.
        limit: usize,
    },
    /// A named resource budget was exhausted; no partial result is returned.
    #[error("rewrite resource limit exhausted: {0}")]
    Limit(&'static str),
    /// Caller cancellation was observed.
    #[error("rewrite detection cancelled")]
    Cancelled,
}
