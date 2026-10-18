use crate::content_diff::{BinaryMode, DiffError, DiffLimits};

/// How unresolved text is selected. Clean edits from both sides are always retained.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Favor {
    /// Retain unresolved regions for the caller.
    #[default]
    Conflict,
    /// Select our bytes in overlapping regions.
    Ours,
    /// Select their bytes in overlapping regions.
    Theirs,
    /// Keep common boundary lines once, with our then their conflicting text between them.
    Union,
}

/// Content comparison policy, independent of marker rendering.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct ContentOptions {
    /// Automatic NUL detection, forced text or forced binary (default automatic).
    pub binary: BinaryMode,
    /// Selection of overlapping changes (default unresolved).
    pub favor: Favor,
    /// Line-matching whitespace policy (default exact); accepted output bytes remain original.
    pub whitespace: super::Whitespace,
    /// Bounds applied independently to each of the two base-to-side comparisons.
    pub limits: DiffLimits,
}

impl ContentOptions {
    /// Selects binary classification.
    #[must_use]
    pub fn with_binary(mut self, binary: BinaryMode) -> Self {
        self.binary = binary;
        self
    }

    /// Selects overlapping-change policy.
    #[must_use]
    pub fn with_favor(mut self, favor: Favor) -> Self {
        self.favor = favor;
        self
    }

    /// Selects whitespace used for line matching, preserving original output bytes.
    #[must_use]
    pub fn with_whitespace(mut self, whitespace: super::Whitespace) -> Self {
        self.whitespace = whitespace;
        self
    }

    /// Selects per-comparison resource bounds.
    #[must_use]
    pub fn with_limits(mut self, limits: DiffLimits) -> Self {
        self.limits = limits;
        self
    }
}

/// One resolved span or unresolved region in output order.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ContentHunk {
    /// Bytes accepted from either or both sides, without marker escaping.
    Resolved(Vec<u8>),
    /// Original region payloads; missing final newlines are preserved.
    Conflict {
        /// Common ancestor payload.
        base: Vec<u8>,
        /// Our replacement payload.
        ours: Vec<u8>,
        /// Their replacement payload.
        theirs: Vec<u8>,
    },
}

/// Owned merge result. Unresolved input remains available independently of marker style.
///
/// Binary results contain one whole-file hunk. [`Self::render`] returns ours for an unresolved
/// binary conflict. A clean result may still contain NUL bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentMerge {
    pub(crate) hunks: Vec<ContentHunk>,
    pub(crate) binary: bool,
}

impl ContentMerge {
    /// Returns spans in output order, preserving exact bytes of unresolved inputs.
    pub fn hunks(&self) -> &[ContentHunk] {
        &self.hunks
    }

    /// Whether no unresolved regions remain after favoring.
    pub fn is_clean(&self) -> bool {
        self.hunks
            .iter()
            .all(|h| matches!(h, ContentHunk::Resolved(_)))
    }

    /// Whether content policy classified changed inputs as binary.
    pub fn is_binary(&self) -> bool {
        self.binary
    }
}

/// Conflict marker presentation. This choice never changes retained conflict sides.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConflictStyle {
    /// Show ours and theirs, moving common boundary lines outside markers.
    #[default]
    Merge,
    /// Show all three complete region payloads.
    Diff3,
    /// Show the base and move common side boundary lines outside markers.
    Zdiff3,
}

/// Marker labels and width for [`ContentMerge::render`].
///
/// Labels are arbitrary bytes, including newlines, and are emitted verbatim. Callers presenting
/// untrusted labels should validate them. Width uses a nonzero integer; callers control output
/// allocation through this value.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct MarkerOptions {
    /// Marker presentation (default merge).
    pub style: ConflictStyle,
    /// Repeated marker characters (default seven).
    pub width: std::num::NonZeroUsize,
    /// Label after the opening marker (default `ours`).
    pub ours: Vec<u8>,
    /// Label after the base marker (default `base`).
    pub base: Vec<u8>,
    /// Label after the closing marker (default `theirs`).
    pub theirs: Vec<u8>,
}

impl Default for MarkerOptions {
    fn default() -> Self {
        Self {
            style: ConflictStyle::Merge,
            width: std::num::NonZeroUsize::new(7).unwrap(),
            ours: b"ours".to_vec(),
            base: b"base".to_vec(),
            theirs: b"theirs".to_vec(),
        }
    }
}

/// Merge computation failed without modifying external state or returning partial hunks.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Bounded comparison failed; increase limits or retry with a cleared cancellation flag.
    #[error("comparing merge inputs")]
    Diff(#[from] DiffError),
}
