use std::ops::Range;

use super::CommitError;

/// A borrowed commit payload with structural headers and uninterpreted values.
///
/// Use this view to inspect identity bytes or recover signing input even when dates or required
/// fields cannot be interpreted. [`Self::parse`] checks only header framing: each
/// header starts with a nonempty printable ASCII name, a space, and an arbitrary value;
/// continuation lines start with one space. Required fields, their ordering, object IDs, and dates
/// are unchecked. A blank line separates headers from an arbitrary byte message. The original bytes
/// are borrowed; only header ranges allocate, with memory proportional to the number of headers.
/// [`Self::from_bytes`] also indexes legacy framing without requiring a separator or final LF.
///
/// Header indices identify occurrences in payload order, including repeated names. Removal does
/// not select by hash format or assume that multiple signature headers sign the same payload.
/// Signing and verification policy belong to the caller.
///
/// ```
/// use girt::CommitPayload;
/// let bytes = b"author A <a> unusual date\ngpgsig first\n second\nx keep\n\nbody\xff";
/// let payload = CommitPayload::parse(bytes)?;
/// let (index, signature) = payload
///     .headers()
///     .enumerate()
///     .find(|(_, header)| header.name == b"gpgsig")
///     .unwrap();
/// assert_eq!(signature.unfolded_value(), b"first\nsecond");
/// assert_eq!(
///     payload.without_headers(&[index]).unwrap(),
///     b"author A <a> unusual date\nx keep\n\nbody\xff"
/// );
/// assert_eq!(payload.as_bytes(), bytes);
/// # Ok::<(), girt::CommitError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitPayload<'a> {
    bytes: &'a [u8],
    headers: Vec<Range<usize>>,
    message_start: usize,
}

impl<'a> CommitPayload<'a> {
    /// Checks header framing without interpreting or normalizing any value.
    ///
    /// Empty header sections are accepted. NUL, CR and non-UTF-8 values remain inspectable.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError::MissingSeparator`] if no empty separator line exists, or
    /// [`CommitError::InvalidHeader`] for an orphan continuation or invalid header name/framing.
    /// Truncation inside the message cannot be detected without an external length or object ID.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, CommitError> {
        let mut headers: Vec<Range<usize>> = Vec::new();
        let mut start = 0;
        for line in bytes.split_inclusive(|&byte| byte == b'\n') {
            if !line.ends_with(b"\n") {
                return Err(CommitError::MissingSeparator);
            }
            let end = start + line.len();
            if line == b"\n" {
                return Ok(Self {
                    bytes,
                    headers,
                    message_start: end,
                });
            }
            if line.starts_with(b" ") {
                headers.last_mut().ok_or(CommitError::InvalidHeader)?.end = end;
            } else {
                let space = line
                    .iter()
                    .position(|&byte| byte == b' ')
                    .ok_or(CommitError::InvalidHeader)?;
                if space == 0
                    || !line[..space]
                        .iter()
                        .all(|byte| (b'!'..=b'~').contains(byte))
                {
                    return Err(CommitError::InvalidHeader);
                }
                headers.push(start..end);
            }
            start = end;
        }
        Err(CommitError::MissingSeparator)
    }

    /// Indexes imported physical headers without requiring canonical framing or a separator.
    ///
    /// The first empty line starts the message. Without one the message is empty. A leading
    /// space extends the preceding record when one exists; orphan continuations, tab-leading
    /// lines and lines without a framing space remain opaque records. Unterminated final records
    /// are retained. This view performs no semantic validation and has no external effects.
    pub fn from_bytes(bytes: &'a [u8]) -> Self {
        let mut headers: Vec<Range<usize>> = Vec::new();
        let mut start = 0;
        for line in bytes.split_inclusive(|&b| b == b'\n') {
            let end = start + line.len();
            if line == b"\n" {
                return Self {
                    bytes,
                    headers,
                    message_start: end,
                };
            }
            if line.starts_with(b" ") && !headers.is_empty() {
                headers.last_mut().expect("nonempty").end = end;
            } else {
                headers.push(start..end);
            }
            start = end;
        }
        Self {
            bytes,
            headers,
            message_start: bytes.len(),
        }
    }

    /// Borrows the original payload, including all header framing and message bytes.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Iterates over every header occurrence in original order, including required headers.
    pub fn headers(&self) -> impl ExactSizeIterator<Item = CommitHeaderRef<'a>> + '_ {
        self.headers.iter().map(|span| {
            let raw = &self.bytes[span.clone()];
            let line = raw.strip_suffix(b"\n").unwrap_or(raw);
            let first = line.split(|&b| b == b'\n').next().unwrap_or_default();
            let space = first.iter().position(|&b| b == b' ');
            match space {
                Some(space) => CommitHeaderRef {
                    name: &line[..space],
                    value: &line[space + 1..],
                    has_value_separator: true,
                },
                None => CommitHeaderRef {
                    name: first,
                    value: &[],
                    has_value_separator: false,
                },
            }
        })
    }

    /// Borrows bytes after the separator, without adding or removing a final newline.
    pub fn message(&self) -> &'a [u8] {
        &self.bytes[self.message_start..]
    }

    /// Copies the payload except for the selected header occurrences and their continuations.
    ///
    /// Indices are zero-based positions in [`Self::headers`], not occurrence counts for a name.
    /// Selection order does not matter; repeated indices remove a header only once. An empty
    /// selection copies the entire payload. Returns `None` if any index is out of range.
    /// The separator, message, unselected headers (including signature headers), and all their
    /// lexical details remain byte-identical. Removing all headers retains the separator line.
    ///
    /// Select one occurrence to recover its candidate signing input, or select multiple spans
    /// explicitly when a signing format requires it. This operation does not infer which headers
    /// were signed, verify signatures, or validate decoded fields. The source remains unchanged.
    pub fn without_headers(&self, indices: &[usize]) -> Option<Vec<u8>> {
        let mut indices = indices.to_vec();
        indices.sort_unstable();
        indices.dedup();
        if indices
            .last()
            .is_some_and(|&index| index >= self.headers.len())
        {
            return None;
        }
        let removed: usize = indices.iter().map(|&index| self.headers[index].len()).sum();
        let mut output = Vec::with_capacity(self.bytes.len() - removed);
        let mut start = 0;
        for index in indices {
            let span = &self.headers[index];
            output.extend_from_slice(&self.bytes[start..span.start]);
            start = span.end;
        }
        output.extend_from_slice(&self.bytes[start..]);
        Some(output)
    }
}

/// A header occurrence borrowing its exact name and folded value from a [`CommitPayload`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommitHeaderRef<'a> {
    /// Whether the first physical line contains a framing space. Always true after strict
    /// [`CommitPayload::parse`]; legacy bare records from [`CommitPayload::from_bytes`] report
    /// false. Signature recognition must require this field as well as the exact header name.
    pub has_value_separator: bool,

    /// Exact name bytes. Legacy records can have an empty or non-ASCII name.
    pub name: &'a [u8],

    /// Exact value bytes, including LF and framing spaces on continuations, without the final LF.
    /// Identity values can be passed to [`crate::Signature::parse`] for optional interpretation.
    pub value: &'a [u8],
}

impl CommitHeaderRef<'_> {
    /// Copies the value, removing exactly one framing space after each continuation newline.
    ///
    /// Blank continuation lines become empty lines; additional spaces and all other bytes remain.
    pub fn unfolded_value(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(self.value.len());
        for (index, line) in self.value.split(|&byte| byte == b'\n').enumerate() {
            if index == 0 {
                output.extend_from_slice(line);
            } else {
                output.push(b'\n');
                output.extend_from_slice(line.strip_prefix(b" ").unwrap_or(line));
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::first(0, b"gpgsig-sha256 two\ngpgsig three\n\nbody\0")]
    #[case::middle(1, b"gpgsig one\n folded\ngpgsig three\n\nbody\0")]
    #[case::last(2, b"gpgsig one\n folded\ngpgsig-sha256 two\n\nbody\0")]
    fn removes_only_selected_span(#[case] index: usize, #[case] expected: &[u8]) {
        let bytes = b"gpgsig one\n folded\ngpgsig-sha256 two\ngpgsig three\n\nbody\0";
        let payload = CommitPayload::parse(bytes).unwrap();
        assert_eq!(payload.without_headers(&[index]).unwrap(), expected);
        assert_eq!(payload.as_bytes(), bytes);
        assert_eq!(payload.message(), b"body\0");
        assert_eq!(payload.without_headers(&[3]), None);
    }

    #[test]
    fn removes_selected_set_in_payload_order() {
        let bytes = b"gpgsig one\nx keep\ngpgsig-sha256 two\n\nbody";
        let payload = CommitPayload::parse(bytes).unwrap();
        assert_eq!(
            payload.without_headers(&[2, 0, 2]).unwrap(),
            b"x keep\n\nbody"
        );
        assert_eq!(payload.without_headers(&[]).unwrap(), bytes);
        assert_eq!(payload.without_headers(&[0, usize::MAX]), None);
        assert_eq!(payload.as_bytes(), bytes);
    }

    #[test]
    fn unfolds_only_framing_spaces() {
        let payload = CommitPayload::parse(b"gpgsig \xff\n \n  x\n \n\n").unwrap();
        let header = payload.headers().next().unwrap();
        assert_eq!(header.value, b"\xff\n \n  x\n ");
        assert_eq!(header.unfolded_value(), b"\xff\n\n x\n");
        assert_eq!(payload.without_headers(&[0]).unwrap(), b"\n");
    }

    #[rstest]
    #[case::empty(b"\nbody")]
    #[case::reordered(b"committer anything\nauthor missing date\nauthor duplicate\n\n")]
    #[case::opaque(b"author \xff\0\r\n continuation\n\n\xff")]
    fn retains_uninterpreted_headers(#[case] bytes: &[u8]) {
        assert_eq!(CommitPayload::parse(bytes).unwrap().as_bytes(), bytes);
    }

    #[rstest]
    #[case::orphan(b" continuation\n\n", CommitError::InvalidHeader)]
    #[case::no_space(b"gpgsig\n\n", CommitError::InvalidHeader)]
    #[case::tab(b"\tcontinued\n\n", CommitError::InvalidHeader)]
    #[case::bad_name(b"gpg\0sig x\n\n", CommitError::InvalidHeader)]
    #[case::truncated(b"gpgsig x\n continuation", CommitError::MissingSeparator)]
    #[case::missing_separator(b"gpgsig x\n", CommitError::MissingSeparator)]
    #[case::empty(b"", CommitError::MissingSeparator)]
    fn rejects_unframed_payloads(#[case] bytes: &[u8], #[case] error: CommitError) {
        assert_eq!(CommitPayload::parse(bytes), Err(error));
    }
}
