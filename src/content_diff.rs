//! Byte-preserving line edits and explicit binary classification.
//!
//! [`diff`] compares borrowed payloads without storage or path policy. [`BlobContent`] separately
//! loads a [`crate::TreeChange`]'s blob sides. No attributes, textconv, whitespace normalization,
//! rename detection, patch encoding/application, merge, index or worktree access is performed.
mod blobs;
mod myers;
mod types;

use std::sync::atomic::{AtomicBool, Ordering};

pub use blobs::{BlobContent, ContentReadError};
pub use types::{BinaryMode, ContentDiff, ContentEdit, DiffError, DiffLimits, TextDiff};

/// Compares exact bytes, returning unchanged, binary-changed, or a shortest line edit sequence.
///
/// Lines include their terminating LF. A final nonempty unterminated fragment is a line; empty
/// input has no lines. CR, NUL (in forced text), non-UTF-8 and missing final LF remain significant.
/// Results borrow both inputs; no payload bytes are copied. Edits use zero-based half-open ranges
/// into the original inputs, coalesce adjacent insertions/deletions, and contain no context.
/// Unlisted gaps are byte-identical. They are not unified patch hunks: no context expansion,
/// heuristic boundary shifting or CLI rendering is provided.
///
/// Equal bytes return [`ContentDiff::Unchanged`] in every mode, including binary. Auto mode scans
/// the **entire** payloads for NUL; it does not implement Git attributes or Git's sampling policy.
///
/// The independent Myers implementation minimizes inserted plus deleted lines. It greedily matches
/// equal lines, visits diagonals from insertion-heavy to deletion-heavy, and chooses deletion when
/// predecessor frontiers reach the same old position. This fixes repeated-line alignment; it does
/// not promise Git's heuristic output. With N total lines and edit distance D, search uses O(ND)
/// line comparisons and O(D²) stored frontier positions, subject to [`DiffLimits`]. Byte comparison
/// costs are charged too. Empty sides bypass search. No approximate fallback or partial edits are
/// returned on exhaustion.
///
/// This synchronous, pure operation checks cancellation before work, during scans (at most 4 KiB
/// per chunk), at every frontier/line comparison, during traceback, and before return. Set `cancel`
/// from another thread and leave it set until return. Allocation and destruction cannot be
/// interrupted; this is cooperative cancellation without a wall-clock guarantee. Async callers
/// should use a caller-bounded worker for substantial inputs.
///
/// # Errors
///
/// Returns [`DiffError`] on cancellation, checked-arithmetic overflow or exhaustion of any bound.
/// Input-byte limits apply even to equal/binary inputs; line/trace limits apply only to text
/// search. No external state changes on success or failure.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::content_diff::{BinaryMode, ContentDiff, DiffLimits, diff};
/// let result = diff(
///     b"one\ntwo\n",
///     b"one\nthree",
///     BinaryMode::Auto,
///     DiffLimits::default(),
///     &AtomicBool::new(false),
/// )?;
/// let ContentDiff::Text(text) = result else {
///     panic!("expected text edits")
/// };
/// let edit = &text.edits()[0];
/// assert_eq!(edit.old_lines, 1..2);
/// assert_eq!(&text.old_bytes()[edit.old_bytes.clone()], b"two\n");
/// assert_eq!(&text.new_bytes()[edit.new_bytes.clone()], b"three");
/// # Ok::<(), girt::content_diff::DiffError>(())
/// ```
pub fn diff<'a>(
    old: &'a [u8],
    new: &'a [u8],
    mode: BinaryMode,
    limits: DiffLimits,
    cancel: &AtomicBool,
) -> Result<ContentDiff<'a>, DiffError> {
    let mut budget = Budget {
        remaining: limits.max_work,
        cancel,
    };
    budget.check()?;
    let bytes = old
        .len()
        .checked_add(new.len())
        .ok_or(DiffError::Limit("input bytes"))?;
    if bytes > limits.max_input_bytes {
        return Err(DiffError::Limit("input bytes"));
    }
    if equal(old, new, &mut budget)? {
        budget.check()?;
        return Ok(ContentDiff::Unchanged);
    }
    let binary = match mode {
        BinaryMode::Auto => has_nul(old, &mut budget)? || has_nul(new, &mut budget)?,
        BinaryMode::Text => false,
        BinaryMode::Binary => true,
    };
    if binary {
        budget.check()?;
        return Ok(ContentDiff::Binary { old, new });
    }
    let mut remaining_lines = limits.max_lines;
    let old_lines = Lines::new(old, &mut remaining_lines, &mut budget)?;
    let new_lines = Lines::new(new, &mut remaining_lines, &mut budget)?;
    let edits = myers::edits(&old_lines, &new_lines, limits.max_trace, &mut budget)?;
    budget.check()?;
    Ok(ContentDiff::Text(TextDiff { old, new, edits }))
}

fn equal(old: &[u8], new: &[u8], budget: &mut Budget<'_>) -> Result<bool, DiffError> {
    if old.len() != new.len() {
        return Ok(false);
    }
    for (a, b) in old.chunks(4096).zip(new.chunks(4096)) {
        budget.work(a.len())?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}

fn has_nul(bytes: &[u8], budget: &mut Budget<'_>) -> Result<bool, DiffError> {
    for chunk in bytes.chunks(4096) {
        budget.work(chunk.len())?;
        if chunk.contains(&0) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// End offsets avoid copying lines and give both byte ranges and constant-time line access.
struct Lines<'a> {
    bytes: &'a [u8],
    ends: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(
        bytes: &'a [u8],
        remaining: &mut usize,
        budget: &mut Budget<'_>,
    ) -> Result<Self, DiffError> {
        let mut ends = Vec::new();
        for (chunk_index, chunk) in bytes.chunks(4096).enumerate() {
            budget.work(chunk.len())?;
            for (offset, byte) in chunk.iter().enumerate() {
                if *byte == b'\n' {
                    charge(remaining, 1, "lines")?;
                    ends.push(chunk_index * 4096 + offset + 1);
                }
            }
        }
        if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
            charge(remaining, 1, "lines")?;
            ends.push(bytes.len());
        }
        Ok(Self { bytes, ends })
    }

    fn len(&self) -> usize {
        self.ends.len()
    }

    fn offset(&self, line: usize) -> usize {
        if line == 0 { 0 } else { self.ends[line - 1] }
    }

    fn line(&self, index: usize) -> &[u8] {
        &self.bytes[self.offset(index)..self.ends[index]]
    }

    fn range(&self, lines: &std::ops::Range<usize>) -> std::ops::Range<usize> {
        self.offset(lines.start)..self.offset(lines.end)
    }
}

struct Budget<'a> {
    remaining: usize,
    cancel: &'a AtomicBool,
}

impl Budget<'_> {
    fn check(&self) -> Result<(), DiffError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(DiffError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn work(&mut self, count: usize) -> Result<(), DiffError> {
        self.check()?;
        charge(&mut self.remaining, count, "work")
    }
}

fn charge(remaining: &mut usize, count: usize, name: &'static str) -> Result<(), DiffError> {
    *remaining = remaining.checked_sub(count).ok_or(DiffError::Limit(name))?;
    Ok(())
}

#[cfg(test)]
mod tests;
