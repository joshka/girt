use std::ops::Range;

/// Content classification and placement of equivalent shortest edits.
///
/// The defaults enable indent-aware placement calibrated against Git CLI observations. Some
/// ambiguous placements still differ; see [`super::diff`]. Disable scoring to compare with
/// `git diff --no-indent-heuristic`; change-group compaction still applies.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::content_diff::{ContentDiff, DiffLimits, Options, diff_with_options};
///
/// let result = diff_with_options(
///     b"head\n\nhead\n  body\n}\n}\n\n\n}\n",
///     b"head\n\nhead\n  body\nhead\n  body\n}\n}\n\n\n}\n",
///     Options::default().with_indent_heuristic(false),
///     DiffLimits::default(),
///     &AtomicBool::new(false),
/// )?;
/// let ContentDiff::Text(text) = result else {
///     panic!("expected text")
/// };
/// assert_eq!(text.edits()[0].new_lines, 4..6);
/// # Ok::<(), girt::content_diff::DiffError>(())
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Options {
    /// How changed payloads are classified (default [`BinaryMode::Auto`]).
    pub binary: BinaryMode,

    /// Prefer structural boundaries using indentation and blank separators (default true).
    pub indent_heuristic: bool,
}

impl Options {
    /// Sets [`Self::binary`].
    #[must_use]
    pub fn with_binary(mut self, binary: BinaryMode) -> Self {
        self.binary = binary;
        self
    }

    /// Sets [`Self::indent_heuristic`], analogous to Git's `--[no-]indent-heuristic`.
    #[must_use]
    pub fn with_indent_heuristic(mut self, enabled: bool) -> Self {
        self.indent_heuristic = enabled;
        self
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            binary: BinaryMode::Auto,
            indent_heuristic: true,
        }
    }
}

/// Classification policy for [`super::diff`], independent of paths and Git attributes.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::content_diff::{BinaryMode, ContentDiff, DiffLimits, diff};
///
/// let cancel = AtomicBool::new(false);
/// let old = b"key\0old\n";
/// let new = b"key\0new\n";
///
/// assert!(matches!(
///     diff(old, new, BinaryMode::Auto, DiffLimits::default(), &cancel)?,
///     ContentDiff::Binary { .. }
/// ));
/// assert!(matches!(
///     diff(old, new, BinaryMode::Text, DiffLimits::default(), &cancel)?,
///     ContentDiff::Text(_)
/// ));
/// # Ok::<(), girt::content_diff::DiffError>(())
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
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
///
/// An empty-file addition can have unchanged content. Retain the structural [`crate::TreeChange`]
/// so a file's existence and mode are not lost when rendering the content outcome.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::content_diff::{BinaryMode, ContentDiff, DiffLimits, diff};
/// let result = diff(
///     b"same\0bytes",
///     b"same\0bytes",
///     BinaryMode::Binary,
///     DiffLimits::default(),
///     &AtomicBool::new(false),
/// )?;
/// assert_eq!(result, ContentDiff::Unchanged);
/// # Ok::<(), girt::content_diff::DiffError>(())
/// ```
#[derive(Debug, Eq, PartialEq)]
#[non_exhaustive]
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
///
/// # Reconstruct the destination
///
/// Apply edits using the original coordinates. Copy unchanged gaps from the old buffer and
/// replacement ranges from the new buffer; do not adjust later offsets as you go.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::content_diff::{BinaryMode, ContentDiff, DiffLimits, diff};
///
/// let ContentDiff::Text(text) = diff(
///     b"one\ntwo\nthree\n",
///     b"ONE\ntwo\nTHREE",
///     BinaryMode::Text,
///     DiffLimits::default(),
///     &AtomicBool::new(false),
/// )?
/// else {
///     panic!("expected text edits");
/// };
///
/// let mut result = Vec::new();
/// let mut cursor = 0;
/// for edit in text.edits() {
///     result.extend_from_slice(&text.old_bytes()[cursor..edit.old_bytes.start]);
///     result.extend_from_slice(&text.new_bytes()[edit.new_bytes.clone()]);
///     cursor = edit.old_bytes.end;
/// }
/// result.extend_from_slice(&text.old_bytes()[cursor..]);
///
/// assert_eq!(result, text.new_bytes());
/// # Ok::<(), girt::content_diff::DiffError>(())
/// ```
///
/// # Reconstruct exact replacement bytes
///
/// Line ranges count LF-delimited records. Byte ranges preserve CRLF, non-UTF-8 bytes and an
/// unterminated final line; a patch renderer must not add a newline while slicing them.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::content_diff::{BinaryMode, ContentDiff, DiffLimits, diff};
///
/// let old = b"keep\r\nold\xff";
/// let new = b"keep\r\nnew\xff";
/// let ContentDiff::Text(text) = diff(
///     old,
///     new,
///     BinaryMode::Text,
///     DiffLimits::default(),
///     &AtomicBool::new(false),
/// )?
/// else {
///     panic!("requested text");
/// };
///
/// let mut rebuilt = Vec::new();
/// let mut cursor = 0;
/// for edit in text.edits() {
///     rebuilt.extend_from_slice(&old[cursor..edit.old_bytes.start]);
///     rebuilt.extend_from_slice(&new[edit.new_bytes.clone()]);
///     cursor = edit.old_bytes.end;
/// }
/// rebuilt.extend_from_slice(&old[cursor..]);
///
/// assert_eq!(rebuilt, new);
/// # Ok::<(), girt::content_diff::DiffError>(())
/// ```
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
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::content_diff::{BinaryMode, ContentDiff, DiffLimits, diff};
///
/// let ContentDiff::Text(text) = diff(
///     b"keep\n",
///     b"keep\ninserted",
///     BinaryMode::Auto,
///     DiffLimits::default(),
///     &AtomicBool::new(false),
/// )?
/// else {
///     panic!("expected text");
/// };
///
/// let edit = &text.edits()[0];
/// assert!(edit.old_bytes.is_empty());
/// assert_eq!(edit.old_lines, 1..1);
/// assert_eq!(&text.new_bytes()[edit.new_bytes.clone()], b"inserted");
/// # Ok::<(), girt::content_diff::DiffError>(())
/// ```
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
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::content_diff::{BinaryMode, DiffError, DiffLimits, diff};
/// let limits = DiffLimits::default().with_max_input_bytes(3);
/// assert!(matches!(
///     diff(
///         b"ab",
///         b"ab",
///         BinaryMode::Auto,
///         limits,
///         &AtomicBool::new(false)
///     ),
///     Err(DiffError::Limit("input bytes"))
/// ));
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
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

impl DiffLimits {
    /// Sets [`Self::max_input_bytes`].
    #[must_use]
    pub fn with_max_input_bytes(mut self, max_input_bytes: usize) -> Self {
        self.max_input_bytes = max_input_bytes;
        self
    }

    /// Sets [`Self::max_lines`].
    #[must_use]
    pub fn with_max_lines(mut self, max_lines: usize) -> Self {
        self.max_lines = max_lines;
        self
    }

    /// Sets [`Self::max_trace`].
    #[must_use]
    pub fn with_max_trace(mut self, max_trace: usize) -> Self {
        self.max_trace = max_trace;
        self
    }

    /// Sets [`Self::max_work`].
    #[must_use]
    pub fn with_max_work(mut self, max_work: usize) -> Self {
        self.max_work = max_work;
        self
    }
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
///
/// A limit failure supplies no approximate edits. Choose a larger budget, offer a binary/raw
/// view, or let the user explicitly request more work. Cancellation requires a fresh call after
/// the application clears or replaces its flag.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::content_diff::{BinaryMode, DiffError, DiffLimits, diff};
/// assert!(matches!(
///     diff(
///         b"a",
///         b"b",
///         BinaryMode::Auto,
///         DiffLimits::default(),
///         &AtomicBool::new(true)
///     ),
///     Err(DiffError::Cancelled)
/// ));
/// ```
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
