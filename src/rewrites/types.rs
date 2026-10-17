use crate::{
    ObjectId, ObjectKind, ObjectReadError, ReadLimits, TreeCompareError, TreeCompareLimits,
};

/// Source selection for inferred copies, in addition to deleted rename sources.
///
/// Use `Modified` when an added file may derive from a changed file's previous contents.
/// `ModifiedPostimage` serves consumers comparing against its new contents, but the result's
/// `source_id` still names the old content. Neither option searches unchanged source files.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::rewrites::{Copies, Kind, Options};
/// use girt::{EntryMode, InitKind, ObjectFormat, Repository, Tree, TreeEntry};
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Bare,
/// )?;
/// let loose = repo.loose_objects();
/// let original = loose.write_blob(b"original content\n")?;
/// let edited = loose.write_blob(b"changed source\n")?;
/// let entry = |name: &[u8], id| TreeEntry {
///     name: name.to_vec(),
///     mode: EntryMode::Blob,
///     id,
/// };
/// let old = loose.write_tree(&Tree::new(
///     ObjectFormat::Sha1,
///     vec![entry(b"source", original)],
/// )?)?;
/// let new = loose.write_tree(&Tree::new(
///     ObjectFormat::Sha1,
///     vec![entry(b"source", edited), entry(b"copy", original)],
/// )?)?;
/// let copies = repo.objects(Default::default())?.detect_rewrites(
///     Some(old),
///     Some(new),
///     Options::default().with_copies(Copies::Modified),
///     Default::default(),
///     None,
///     &AtomicBool::new(false),
/// )?;
/// assert_eq!(copies.len(), 1);
/// assert_eq!(copies[0].kind, Kind::Copy);
/// assert_eq!(copies[0].source_id, original); // Scored against the source's preimage.
/// assert_eq!(copies[0].target, b"copy");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Copies {
    /// Each deleted source can supply at most one destination.
    #[default]
    Disabled,
    /// Also use preimages of modified regular/executable files and reuse deleted sources.
    /// Unchanged files are not candidates; mode-only changes count as modifications.
    Modified,
    /// Score modified sources by their new blobs, while reporting their old blob identities.
    /// Deleted sources are still scored by their old blobs and can be reused.
    ModifiedPostimage,
}

/// Caller choices for inference, independent of repository configuration.
///
/// ```
/// use girt::rewrites::{Copies, Options};
/// let exact_copies = Options::default()
///     .with_similarity(100)
///     .with_copies(Copies::Modified)
///     .with_track_empty(true);
/// assert_eq!(exact_copies.similarity, 100); // Identity-only; no approximate scoring.
/// let bounded = Options::default().with_candidate_limit(0);
/// assert_eq!(bounded.candidate_limit, 0); // Skip edited matching if any counted pair remains.
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Options {
    /// Minimum retained-byte percentage, inclusive, from 1 through 100 (default 50).
    /// 100 admits only identical object IDs, bypassing approximate scoring and the candidate
    /// limit; independent [`Limits`] still apply and no inexact skip is reported.
    pub similarity: u8,
    /// Whether modified files and already-used deleted files can supply copies.
    pub copies: Copies,
    /// Include empty blobs as exact-match sources (default false).
    pub track_empty: bool,
    /// Bound on edited-content detection, as in Git's `diff.renameLimit` (default 1000).
    ///
    /// Exact matches are attempted first. Edited-content detection is skipped when the remaining
    /// sources multiplied by the remaining destinations exceeds the square of this limit, as Git
    /// does. [`Objects::detect_rewrites_or_exact`](crate::Objects::detect_rewrites_or_exact)
    /// then returns the exact matches with an [`InexactSkipped`] report;
    /// [`Objects::detect_rewrites`](crate::Objects::detect_rewrites) returns
    /// [`Error::Candidates`]. Neither reports an incomplete inference as complete.
    ///
    /// The counts are the sources still available after exact matching (including exact-paired
    /// sources reused for copies) and the unmatched destinations, as Git counts them: binary files
    /// count even when [`Self::approximate_binary`] is false, and empty files count when
    /// [`Self::track_empty`] is true, as with Git's default of detecting empty renames. Git pairs
    /// some files before this check, such as an edited file moved to a path with the same
    /// basename; girt leaves those to edited detection, so near the limit a skip can still differ.
    /// Zero skips edited detection whenever a counted pair remains; Git's configuration value 0
    /// means unlimited, which is `usize::MAX`.
    pub candidate_limit: usize,
    /// Permit approximate matching when either blob contains NUL in its first 8000 bytes.
    /// Exact object-ID matches are still considered (default true).
    pub approximate_binary: bool,
}

impl Options {
    /// Sets [`Self::similarity`].
    #[must_use]
    pub fn with_similarity(mut self, similarity: u8) -> Self {
        self.similarity = similarity;
        self
    }

    /// Sets [`Self::copies`].
    #[must_use]
    pub fn with_copies(mut self, copies: Copies) -> Self {
        self.copies = copies;
        self
    }

    /// Sets [`Self::track_empty`].
    #[must_use]
    pub fn with_track_empty(mut self, track_empty: bool) -> Self {
        self.track_empty = track_empty;
        self
    }

    /// Sets [`Self::candidate_limit`].
    #[must_use]
    pub fn with_candidate_limit(mut self, candidate_limit: usize) -> Self {
        self.candidate_limit = candidate_limit;
        self
    }

    /// Sets [`Self::approximate_binary`].
    #[must_use]
    pub fn with_approximate_binary(mut self, approximate_binary: bool) -> Self {
        self.approximate_binary = approximate_binary;
        self
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            similarity: 50,
            copies: Copies::Disabled,
            track_empty: false,
            candidate_limit: 1000,
            approximate_binary: true,
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
#[non_exhaustive]
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

impl Limits {
    /// Sets [`Self::trees`].
    #[must_use]
    pub fn with_trees(mut self, trees: TreeCompareLimits) -> Self {
        self.trees = trees;
        self
    }

    /// Sets [`Self::read`].
    #[must_use]
    pub fn with_read(mut self, read: ReadLimits) -> Self {
        self.read = read;
        self
    }

    /// Sets [`Self::max_blob_bytes`].
    #[must_use]
    pub fn with_max_blob_bytes(mut self, max_blob_bytes: usize) -> Self {
        self.max_blob_bytes = max_blob_bytes;
        self
    }

    /// Sets [`Self::max_spans`].
    #[must_use]
    pub fn with_max_spans(mut self, max_spans: usize) -> Self {
        self.max_spans = max_spans;
        self
    }

    /// Sets [`Self::max_pairs`].
    #[must_use]
    pub fn with_max_pairs(mut self, max_pairs: usize) -> Self {
        self.max_pairs = max_pairs;
        self
    }

    /// Sets [`Self::max_work`].
    #[must_use]
    pub fn with_max_work(mut self, max_work: usize) -> Self {
        self.max_work = max_work;
        self
    }
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
#[non_exhaustive]
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
#[non_exhaustive]
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

/// Inferred rewrites, with an explicit report when edited-content detection was skipped.
///
/// Returned by [`Objects::detect_rewrites_or_exact`](crate::Objects::detect_rewrites_or_exact).
///
/// Always inspect `inexact_skipped` before presenting the result as complete. A UI can retain
/// exact pairs, leave all other additions/deletions unpaired, and offer a retry using
/// [`InexactSkipped::suggested_limit`]. The [fallback
/// example](crate::Objects::detect_rewrites_or_exact) shows the full lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Detection {
    /// Inferred relationships, ordered by destination raw path bytes.
    ///
    /// When [`Self::inexact_skipped`] is set, these are the exact matches only.
    pub rewrites: Vec<Rewrite>,
    /// Set when [`Options::candidate_limit`] prevented edited-content detection.
    ///
    /// `None` means detection ran to completion under the requested options.
    pub inexact_skipped: Option<InexactSkipped>,
}

/// Edited-content detection was skipped because too many candidates remained after exact matching.
///
/// Destinations without an exact match stay unpaired, even where an edited source would match.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct InexactSkipped {
    /// Sources still available after exact matching, counted as
    /// [`Options::candidate_limit`] describes.
    pub sources: usize,
    /// Destinations remaining after exact matching, counted the same way.
    pub targets: usize,
    /// Caller-specified [`Options::candidate_limit`].
    pub limit: usize,
}

impl InexactSkipped {
    /// A [`Options::candidate_limit`] that permits edited detection for these candidates.
    ///
    /// This is the larger of the two counts, the value Git suggests for `diff.renameLimit`. It is
    /// sufficient but not always minimal: one source against three destinations needs only 2.
    ///
    /// The suggestion addresses candidate selection only. Independent byte, pair and work budgets
    /// in [`Limits`] can still refuse the retry.
    #[must_use]
    pub fn suggested_limit(&self) -> usize {
        self.sources.max(self.targets)
    }
}

/// Inference failed without returning partial records or modifying the repository.
///
/// Callers may raise an exhausted bound or retry cancellation with a cleared flag. Missing/corrupt
/// storage needs repair or a refreshed object view; retrying unchanged inputs does not repair it.
///
/// [`Error::Candidates`] is a policy refusal from the strict API; retry with a chosen larger
/// candidate limit or use [`crate::Objects::detect_rewrites_or_exact`] and report the skip.
/// [`Error::Limit`] instead means a resource budget failed, including during exact matching, and
/// neither API returns partial records. Invalid similarity is a caller error, not corrupt storage.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::rewrites::{Error, Limits, Options};
/// use girt::{InitKind, ObjectFormat, Repository};
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Bare,
/// )?;
/// let result = repo.objects(Default::default())?.detect_rewrites(
///     None,
///     None,
///     Options::default().with_similarity(0),
///     Limits::default(),
///     None,
///     &AtomicBool::new(false),
/// );
/// assert!(matches!(result, Err(Error::Similarity(0))));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Similarity must be between 1 and 100 inclusive.
    #[error("rewrite similarity must be in 1..=100, got {0}")]
    Similarity(u8),
    /// Structural tree comparison failed.
    #[error("comparing rewrite trees")]
    Trees(#[from] TreeCompareError),
    /// A candidate blob could not be read or verified.
    #[error("reading rewrite blob {id}")]
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
    ///
    /// Only [`Objects::detect_rewrites`](crate::Objects::detect_rewrites) returns this; see
    /// [`InexactSkipped`] for the fields.
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
