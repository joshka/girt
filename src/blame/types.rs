use crate::content_diff::DiffLimits;
use crate::{ObjectId, ReadLimits};

/// Consecutive destination lines with consecutive origins. Coordinates are one-based.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Hunk {
    /// Commit that last changed these lines.
    pub commit: ObjectId,
    /// Literal repository-relative path in that commit, without UTF-8 conversion.
    pub original_path: Vec<u8>,
    /// First line in the original file.
    pub original_start: usize,
    /// First line in the requested file.
    pub final_start: usize,
    /// Number of consecutive lines.
    pub line_count: usize,
}

/// Whole-walk bounds plus per-comparison and per-object bounds.
///
/// Memory includes at most `max_lines` pending line mappings per history state. `max_steps`
/// bounds processed states and parent comparisons, including merge fan-out. `max_bytes` bounds
/// cumulative attribution blob bytes read; rename-inference reads use `rewrites` instead.
/// Tree, read and diff limits apply separately to each operation.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum history states and parent comparisons combined.
    pub max_steps: usize,
    /// Maximum lines in any admitted file.
    pub max_lines: usize,
    /// Cumulative attribution blob bytes read, excluding separately bounded rename inference.
    pub max_bytes: usize,
    /// Bounds for each object read.
    pub read: ReadLimits,
    /// Bounds for each Myers comparison.
    pub diff: DiffLimits,
    /// Bounds for each rename comparison.
    pub rewrites: crate::rewrites::Limits,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_steps: 100_000,
            max_lines: 1_000_000,
            max_bytes: 512 * 1024 * 1024,
            read: ReadLimits::default(),
            diff: DiffLimits::default(),
            rewrites: crate::rewrites::Limits::default(),
        }
    }
}

impl Limits {
    /// Sets the bound for history work.
    #[must_use]
    pub fn with_max_steps(mut self, value: usize) -> Self {
        self.max_steps = value;
        self
    }
    /// Sets the bound for file line count.
    #[must_use]
    pub fn with_max_lines(mut self, value: usize) -> Self {
        self.max_lines = value;
        self
    }
    /// Sets the bound for cumulative blob bytes.
    #[must_use]
    pub fn with_max_bytes(mut self, value: usize) -> Self {
        self.max_bytes = value;
        self
    }
    /// Sets the bound for each object read.
    #[must_use]
    pub fn with_read(mut self, value: ReadLimits) -> Self {
        self.read = value;
        self
    }
    /// Sets the bound for each content comparison.
    #[must_use]
    pub fn with_diff(mut self, value: DiffLimits) -> Self {
        self.diff = value;
        self
    }
    /// Sets the bound for each rename comparison.
    #[must_use]
    pub fn with_rewrites(mut self, value: crate::rewrites::Limits) -> Self {
        self.rewrites = value;
        self
    }
}

/// Attribution failed; no partial attribution is returned and nothing is written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Cancellation was requested.
    #[error("blame cancelled")]
    Cancelled,
    /// A whole-walk budget was exhausted.
    #[error("blame {0} limit exceeded")]
    Limit(&'static str),
    /// Invalid literal repository-relative path.
    #[error("invalid blame path")]
    InvalidPath,
    /// The requested file does not exist.
    #[error("blame path is absent")]
    Absent,
    /// A tree entry is not a regular file.
    #[error("blame requires a regular file")]
    NotFile,
    /// The requested file contains a NUL byte. Older versions remain raw line inputs.
    #[error("binary blame is unsupported")]
    Binary,
    /// Attribution cannot continue beyond a shallow boundary.
    #[error("blame reached shallow boundary {0}")]
    Shallow(ObjectId),
    /// Rename inference could not complete under its candidate bound.
    #[error("blame rename inference was incomplete")]
    IncompleteRenames,
    /// A lower-level read or comparison failed.
    #[error(transparent)]
    Operation(#[from] crate::Error),
}
