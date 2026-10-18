//! Parse unified patches and apply their text hunks to byte content without filesystem access.
//!
//! [`Patch::parse`] validates the complete patch before returning file edits. Each [`FilePatch`]
//! applies to an immutable preimage and returns owned bytes only after every hunk succeeds.
//! [`Patch::apply_tree`] computes a stored-tree replacement and returns an [`AppliedTree`] whose
//! generated objects are installed explicitly. Callers retain control over references, the index
//! and working files and must validate paths for their storage destination.
//! Git text patches and traditional unified diffs are supported. Extended operations, binary and
//! combined diffs are explicitly refused in this initial implementation.
//!
//! ```
//! use girt::apply::Patch;
//!
//! let patch = Patch::parse(b"--- a/fruit\n+++ b/fruit\n@@ -1 +1 @@\n-pear\n+plum\n")?;
//! let result = patch.files()[0].apply(b"pear\n")?;
//! assert_eq!(result.content, b"plum\n");
//! # Ok::<(), girt::apply::Error>(())
//! ```
mod parse;
mod text;
mod tree;
pub use tree::{AppliedFile, AppliedTree, TreeError, TreeLimits, TreeOptions};

/// Validated text patch, retaining exact path and content bytes.
#[derive(Clone, Debug)]
pub struct Patch {
    files: Vec<FilePatch>,
}

impl Patch {
    /// Parses with [`Limits::default`]. No storage is read or written.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for malformed, unsupported or resource-limited input.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        Self::parse_with(bytes, Limits::default())
    }

    /// Parses under whole-patch byte, file and hunk bounds.
    ///
    /// Paths retain prefixes (`a/` and `b/`) and are never interpreted as filesystem paths.
    /// Traditional paths may have tab-separated timestamps. Git quoted paths are decoded.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] without a partial patch for malformed syntax, count overflow, unsupported
    /// extended operations or exhausted limits. Empty input is malformed.
    pub fn parse_with(bytes: &[u8], limits: Limits) -> Result<Self, Error> {
        parse::parse(bytes, limits)
    }

    /// File edits in input order. Duplicate paths are retained.
    pub fn files(&self) -> &[FilePatch] {
        &self.files
    }
}

/// One file's paths and text hunks. `/dev/null` is represented by an absent side.
#[derive(Clone, Debug)]
pub struct FilePatch {
    old_path: Option<Vec<u8>>,
    new_path: Option<Vec<u8>>,
    hunks: Vec<Hunk>,
}

impl FilePatch {
    /// Preimage path, including any prefix, or `None` for a new file.
    pub fn old_path(&self) -> Option<&[u8]> {
        self.old_path.as_deref()
    }

    /// Postimage path, including any prefix, or `None` for a deleted file.
    pub fn new_path(&self) -> Option<&[u8]> {
        self.new_path.as_deref()
    }

    /// Number of text hunks.
    pub fn hunk_count(&self) -> usize {
        self.hunks.len()
    }

    /// Applies all hunks at their declared positions using default bounds.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when preimage bytes do not match, hunks overlap, or limits are exceeded.
    /// The input is never mutated and no partial output is returned.
    pub fn apply(&self, preimage: &[u8]) -> Result<Applied, Error> {
        self.apply_with(preimage, Limits::default())
    }

    /// Applies exact-context hunks under input and output bounds.
    ///
    /// Zero-context hunks are accepted. No offset search, fuzz or whitespace normalization occurs.
    /// Unlike Git's default matching, reduced trailing context does not pin a hunk to EOF.
    /// A missing-final-newline marker does require the postimage to end at that line.
    /// New files require an empty preimage; deleted files must produce empty output. This content
    /// operation does not establish whether the source exists or the destination is absent.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for mismatched or overlapping hunks and exhausted bounds. Nothing is
    /// published, even if an earlier hunk succeeded.
    pub fn apply_with(&self, preimage: &[u8], limits: Limits) -> Result<Applied, Error> {
        text::apply(self, preimage, limits)
    }
}

/// Successful application, ready for caller-controlled publication.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Applied {
    /// Complete postimage bytes.
    pub content: Vec<u8>,
    /// Applied hunk locations in patch order.
    pub hunks: Vec<HunkResult>,
}

/// Location of an applied hunk; line positions are zero-based.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct HunkResult {
    /// One-based line of the hunk header in the patch.
    pub patch_line: usize,
    /// Preimage line at which the hunk was applied.
    pub old_start: usize,
    /// Postimage line at which the hunk begins.
    pub new_start: usize,
}

/// Whole-input and output bounds. Defaults permit 16 MiB and 100,000 hunks in 10,000 files.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum patch or preimage bytes, checked before allocating their representation.
    pub max_input_bytes: usize,
    /// Maximum file edits in one patch.
    pub max_files: usize,
    /// Maximum hunks across the patch.
    pub max_hunks: usize,
    /// Maximum content lines across all hunks (default 100,000). Bounds per-line allocations.
    pub max_lines: usize,
    /// Maximum postimage bytes for one content application.
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024,
            max_files: 10_000,
            max_hunks: 100_000,
            max_lines: 100_000,
            max_output_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Patch parsing or content application failed without external effects.
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[non_exhaustive]
pub enum Error {
    /// Syntax or hunk counts are invalid.
    #[error("malformed patch at line {line}: {reason}")]
    Malformed {
        /// One-based patch line.
        line: usize,
        /// Static diagnostic, containing no input content.
        reason: &'static str,
    },
    /// A recognized format is not supported.
    #[error("unsupported patch at line {line}: {feature}")]
    Unsupported {
        /// One-based patch line.
        line: usize,
        /// Recognized operation or format.
        feature: &'static str,
    },
    /// A caller-selected resource bound was exceeded.
    #[error("patch limit exceeded: {0}")]
    Limit(&'static str),
    /// A hunk cannot be applied at its declared position.
    #[error("preimage mismatch for hunk at patch line {line}")]
    Mismatch {
        /// One-based patch line of the failing hunk header.
        line: usize,
    },
}

#[derive(Clone, Debug)]
struct Hunk {
    line: usize,
    old_start: usize,
    new_start: usize,
    lines: Vec<Line>,
}

#[derive(Clone, Debug)]
struct Line {
    kind: u8,
    bytes: Vec<u8>,
}
