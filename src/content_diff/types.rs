use std::ops::Range;

/// Classification policy for [`super::diff`], independent of paths and Git attributes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BinaryMode {
    /// Any NUL anywhere in either changed input means binary; all other bytes are text.
    #[default]
    Auto,
    /// Compare all bytes as LF-delimited lines, including NUL and invalid UTF-8.
    Text,
    /// Report changed bytes as binary without producing line edits.
    Binary,
}

/// Exact comparison outcome borrowing the caller's unchanged input buffers.
#[derive(Debug, Eq, PartialEq)]
pub enum ContentDiff<'a> {
    /// Payloads are byte-identical, regardless of binary policy.
    Unchanged,
    /// Payloads differ and policy classifies them as binary; no binary patch is encoded.
    Binary {
        /// Original payload.
        old: &'a [u8],
        /// Replacement payload.
        new: &'a [u8],
    },
    /// Changed text with a complete, deterministic shortest line edit sequence.
    Text(TextDiff<'a>),
}

/// Borrowed text inputs and owned edit ranges, constructed by [`super::diff`].
///
/// Edits are ordered, nonoverlapping and separated by at least one unchanged line. Replace each
/// original byte range by its corresponding new byte range to reconstruct the destination exactly.
/// Coordinates always refer to the original buffers, not progressively edited text. The retained
/// borrows prevent mutation or destruction of either input while these slices remain in use.
#[derive(Debug, Eq, PartialEq)]
pub struct TextDiff<'a> {
    pub(super) old: &'a [u8],
    pub(super) new: &'a [u8],
    pub(super) edits: Vec<ContentEdit>,
}

impl<'a> TextDiff<'a> {
    /// Original bytes, without normalization or UTF-8 conversion.
    pub fn old_bytes(&self) -> &'a [u8] {
        self.old
    }

    /// Replacement bytes, without normalization or UTF-8 conversion.
    pub fn new_bytes(&self) -> &'a [u8] {
        self.new
    }

    /// Coalesced changed spans with zero context; unchanged gaps are implicit.
    pub fn edits(&self) -> &[ContentEdit] {
        &self.edits
    }
}

/// One insertion, deletion or replacement in the original coordinate spaces.
///
/// All ranges are zero-based and half-open. An empty old range denotes insertion; an empty new
/// range denotes deletion. In results from [`super::diff`], at least one range is nonempty and
/// byte endpoints coincide with LF-delimited line boundaries (including end of input).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentEdit {
    /// Lines removed from the original input.
    pub old_lines: Range<usize>,
    /// Lines inserted from the replacement input.
    pub new_lines: Range<usize>,
    /// Exact original bytes removed, including any terminating LF.
    pub old_bytes: Range<usize>,
    /// Exact replacement bytes inserted, including any terminating LF.
    pub new_bytes: Range<usize>,
}

/// Deterministic resource bounds for one [`super::diff`] call; zero is a real bound.
///
/// Memory is O(lines + retained trace positions + edits); input bytes are borrowed. Vector spare
/// capacity and allocator overhead are not exact heap limits. Output contains at most the combined
/// line count of edits. Bounds are checked before line/trace growth; allocation failure follows
/// Rust's allocator behavior. Raising bounds can admit quadratic search/trace growth on unrelated
/// text. The default rejects that work rather than returning an approximate answer.
#[derive(Clone, Copy, Debug)]
pub struct DiffLimits {
    /// Combined input payload bytes, even for equal/binary data (default 64 MiB).
    pub max_input_bytes: usize,
    /// Combined number of text lines, checked before storing each offset (default 1,000,000).
    pub max_lines: usize,
    /// Sum of frontier positions retained, including the current row (default 1,000,000).
    ///
    /// A distance-d row reserves d+1 positions before search. Empty-side comparisons need none.
    pub max_trace: usize,
    /// Cumulative work units (default 64 Mi): one per frontier step, traceback step and line
    /// comparison, plus bytes scanned and the shorter length of each equal-length comparison.
    /// Length-mismatched line comparisons charge only one unit. Byte comparison charges are
    /// conservative, including bytes beyond the first mismatch in a chunk or line.
    pub max_work: usize,
}

impl Default for DiffLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 * 1024 * 1024,
            max_lines: 1_000_000,
            max_trace: 1_000_000,
            max_work: 64 * 1024 * 1024,
        }
    }
}

/// Content comparison failed without returning partial edits or changing external state.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DiffError {
    /// A named input, line, trace or work bound was exceeded, or its arithmetic overflowed.
    #[error("content diff limit exceeded: {0}")]
    Limit(&'static str),
    /// The caller's flag was observed at a cooperative checkpoint.
    #[error("content diff cancelled")]
    Cancelled,
}
