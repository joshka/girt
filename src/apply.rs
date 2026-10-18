//! Parse unified patches and apply their text hunks to byte content without filesystem access.
//!
//! [`Patch::parse`] decodes the complete input before returning file edits. Each [`FilePatch`]
//! applies to an immutable preimage and returns owned bytes only after every hunk succeeds.
//! [`Patch::apply_tree`] computes a stored-tree replacement and returns an [`AppliedTree`] whose
//! generated objects are installed explicitly. [`Patch::apply_index`] similarly computes an index
//! draft for publication through the existing index lock API. Callers retain control over
//! references and working files and must validate paths for their storage destination.
//! Git text patches, mode changes, additions/deletions, renames and copies are supported, along
//! with traditional unified diffs and Git binary literal/delta patches. Combined diffs are refused.
//!
//! ```
//! use girt::apply::Patch;
//!
//! let patch = Patch::parse(b"--- a/fruit\n+++ b/fruit\n@@ -1 +1 @@\n-pear\n+plum\n")?;
//! let result = patch.files()[0].apply(b"pear\n")?;
//! assert_eq!(result.content, b"plum\n");
//! # Ok::<(), girt::apply::Error>(())
//! ```
mod binary;
pub use binary::BinaryKind;
mod index;
pub use index::{AppliedIndex, IndexError, IndexLimits};
pub(crate) mod parse;
mod text;
mod worktree;
pub use worktree::{
    PreparedWorktree, WorktreeAction, WorktreeError, WorktreeFailure, WorktreeFile, WorktreeLimits,
    WorktreeReport, WorktreeWrite,
};
mod tree;
pub use tree::{AppliedFile, AppliedTree, TreeError, TreeLimits, TreeOptions};

/// Parsed file changes, retaining byte paths, metadata and bounded text/binary payloads.
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

    /// Parses under whole-patch byte, file, hunk and inflated-binary bounds.
    ///
    /// Paths retain prefixes (`a/` and `b/`) and are never interpreted as filesystem paths.
    /// Traditional paths may have tab-separated timestamps. Git quoted paths are decoded.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] without a partial patch for malformed syntax, count overflow, unsupported
    /// operations or exhausted limits. Empty input is malformed.
    pub fn parse_with(bytes: &[u8], limits: Limits) -> Result<Self, Error> {
        parse::parse(bytes, limits)
    }

    /// Reverses paths, modes, text hunks and binary identities without copying payloads.
    ///
    /// Apply the returned patch to the original postimage to undo supported edits. File order and
    /// diagnostic patch lines are retained. Copies remain copies with exchanged paths, as with
    /// Git's reverse application: the reversed destination must be absent. In particular, this
    /// does not remove a copied file while retaining its original source.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] if a binary patch has no reverse block. No storage is read
    /// or written. The parsed patch is consumed, so reversal needs no additional payload budget.
    pub fn reversed(mut self) -> Result<Self, Error> {
        for file in &mut self.files {
            if let Some(binary) = &mut file.binary {
                let reverse = binary.reverse.take().ok_or(Error::Unsupported {
                    line: file.line,
                    feature: "binary reversal without a reverse block",
                })?;
                binary.reverse = Some(std::mem::replace(&mut binary.forward, reverse));
                std::mem::swap(&mut binary.old, &mut binary.new);
            }

            std::mem::swap(&mut file.old_path, &mut file.new_path);
            std::mem::swap(&mut file.old_mode, &mut file.new_mode);
            for hunk in &mut file.hunks {
                std::mem::swap(&mut hunk.old_start, &mut hunk.new_start);
                for line in &mut hunk.lines {
                    line.kind = match line.kind {
                        b'+' => b'-',
                        b'-' => b'+',
                        other => other,
                    };
                }
            }
        }

        Ok(self)
    }

    /// File edits in input order. Duplicate paths are retained.
    pub fn files(&self) -> &[FilePatch] {
        &self.files
    }
}

/// One file's paths, declared metadata and owned text/binary data. `/dev/null` is represented by an
/// absent side.
#[derive(Clone, Debug)]
pub struct FilePatch {
    old_path: Option<Vec<u8>>,
    new_path: Option<Vec<u8>>,
    hunks: Vec<Hunk>,
    line: usize,
    old_mode: Option<crate::EntryMode>,
    new_mode: Option<crate::EntryMode>,
    operation: FileOperation,
    similarity: Option<u8>,
    binary: Option<binary::BinaryPatch>,
}

/// How a file's source relates to its destination.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum FileOperation {
    /// Modify, add or delete at one path.
    #[default]
    Edit,
    /// Remove the source and create the destination.
    Rename,
    /// Retain the source and create the destination.
    Copy,
}

impl FilePatch {
    /// Forward binary encoding, absent for text or metadata-only changes.
    pub fn binary_kind(&self) -> Option<BinaryKind> {
        self.binary.as_ref().map(|patch| patch.forward.kind)
    }

    /// Reverse binary encoding when the patch supplied a second block.
    /// Use [`Patch::reversed`] to select it for application.
    pub fn reverse_binary_kind(&self) -> Option<BinaryKind> {
        self.binary
            .as_ref()
            .and_then(|patch| patch.reverse.as_ref().map(|block| block.kind))
    }

    /// Requested file operation, independent of any content hunks.
    pub fn operation(&self) -> FileOperation {
        self.operation
    }

    /// Declared source mode, when present in Git metadata.
    pub fn old_mode(&self) -> Option<crate::EntryMode> {
        self.old_mode
    }

    /// Declared destination mode, when present in Git metadata.
    pub fn new_mode(&self) -> Option<crate::EntryMode> {
        self.new_mode
    }

    /// Declared rename/copy similarity percentage; it is not recomputed during application.
    pub fn similarity(&self) -> Option<u8> {
        self.similarity
    }

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

    /// Applies text or binary content, preserving bytes for metadata-only edits.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when preimage bytes do not match, hunks overlap, or limits are exceeded.
    /// The input is never mutated and no partial output is returned.
    pub fn apply(&self, preimage: &[u8]) -> Result<Applied, Error> {
        self.apply_with(preimage, Limits::default())
    }

    /// Applies exact-context hunks or identity-checked binary data under input/output bounds.
    ///
    /// Zero-context hunks are accepted. No offset search, fuzz or whitespace normalization occurs.
    /// Unlike Git's default matching, reduced trailing context does not pin a hunk to EOF.
    /// A missing-final-newline marker does require the postimage to end at that line.
    /// New files require an empty preimage; deleted files must produce empty output. This content
    /// operation does not establish whether the source exists or the destination is absent.
    /// Binary patches require full, same-format index identities and verify both preimage and
    /// result hashes. Delta instructions are validated here against the supplied preimage.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for mismatched or overlapping hunks and exhausted bounds. Nothing is
    /// published, even if an earlier hunk succeeded.
    pub fn apply_with(&self, preimage: &[u8], limits: Limits) -> Result<Applied, Error> {
        match &self.binary {
            Some(binary) => binary::apply(self, binary, preimage, limits),
            None => text::apply(self, preimage, limits),
        }
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
    /// Aggregate inflated literal and delta-program bytes across all binary blocks (default 16
    /// MiB).
    pub max_binary_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024,
            max_files: 10_000,
            max_hunks: 100_000,
            max_lines: 100_000,
            max_output_bytes: 16 * 1024 * 1024,
            max_binary_bytes: 16 * 1024 * 1024,
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
