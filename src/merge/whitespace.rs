use std::sync::atomic::{AtomicBool, Ordering};

use super::ContentOptions;
use crate::content_diff::{self, BinaryMode, ContentDiff, ContentEdit, DiffError, DiffLimits};

/// Whitespace used for matching lines, without normalizing accepted output bytes.
///
/// Non-exact policies use comparison-only keys with original line coordinates and a final-LF
/// flag, including whitespace-only unterminated records. They reserve four work units per raw
/// input byte from each comparison's budget before bounded diff work. Keys use at most two extra
/// bytes per line; returned hunks contain original bytes, never key bytes. Binary classification
/// happens before key construction. Existing ambiguous diff-placement/refinement limits apply.
///
/// ```
/// use girt::merge::{ContentOptions, MarkerOptions, Whitespace, content_with};
/// let options = ContentOptions::default().with_whitespace(Whitespace::IgnoreSpaceChange);
/// let result = content_with(
///     b"a b\n",
///     b"a  b\n",
///     b"a B\n",
///     options,
///     &std::sync::atomic::AtomicBool::new(false),
/// )?;
/// assert!(result.is_clean());
/// assert_eq!(result.render(&MarkerOptions::default()), b"a B\n");
/// # Ok::<(), girt::merge::Error>(())
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Whitespace {
    /// Compare every byte, including CR and missing final LF.
    #[default]
    Exact,
    /// Collapse horizontal ASCII whitespace runs and ignore trailing whitespace.
    /// A run and no whitespace remain different away from the line end.
    IgnoreSpaceChange,
    /// Ignore horizontal ASCII whitespace anywhere in each line.
    IgnoreAllSpace,
    /// Ignore horizontal ASCII whitespace at each line's end.
    IgnoreSpaceAtEol,
    /// Ignore one CR at a line's end.
    IgnoreCrAtEol,
}

pub(super) fn compare(
    base: &[u8],
    side: &[u8],
    options: ContentOptions,
    cancel: &AtomicBool,
) -> Result<(Vec<ContentEdit>, bool), DiffError> {
    if options.whitespace == Whitespace::Exact {
        return Ok(unpack(content_diff::diff(
            base,
            side,
            options.binary,
            options.limits,
            cancel,
        )?));
    }
    check(cancel)?;
    let bytes = base
        .len()
        .checked_add(side.len())
        .ok_or(DiffError::Limit("input bytes"))?;
    if bytes > options.limits.max_input_bytes {
        return Err(DiffError::Limit("input bytes"));
    }
    // Reserve conservative linear work for classification, keys and original line offsets.
    let work = bytes
        .checked_mul(4)
        .ok_or(DiffError::Limit("whitespace work"))?;
    let remaining = options
        .limits
        .max_work
        .checked_sub(work)
        .ok_or(DiffError::Limit("whitespace work"))?;
    let binary = match options.binary {
        BinaryMode::Auto => base.contains(&0) || side.contains(&0),
        BinaryMode::Text => false,
        BinaryMode::Binary => true,
    };
    if base == side {
        return Ok((Vec::new(), false));
    }
    if binary {
        return Ok((Vec::new(), true));
    }
    let a = keys(base, options.whitespace, options.limits, cancel)?;
    let b = keys(side, options.whitespace, options.limits, cancel)?;
    let mut limits = options.limits;
    limits.max_work = remaining;
    // Each original record gets a final-newline flag and a comparison-only LF delimiter.
    limits.max_input_bytes = limits
        .max_input_bytes
        .saturating_add(limits.max_lines.saturating_mul(2));
    let (mut edits, _) = unpack(content_diff::diff(
        &a.bytes,
        &b.bytes,
        BinaryMode::Text,
        limits,
        cancel,
    )?);
    for edit in &mut edits {
        edit.old_bytes = a.offsets[edit.old_lines.start]..a.offsets[edit.old_lines.end];
        edit.new_bytes = b.offsets[edit.new_lines.start]..b.offsets[edit.new_lines.end];
    }
    Ok((edits, false))
}

fn unpack(diff: ContentDiff<'_>) -> (Vec<ContentEdit>, bool) {
    match diff {
        ContentDiff::Text(text) => (text.edits().to_vec(), false),
        ContentDiff::Binary { .. } => (Vec::new(), true),
        ContentDiff::Unchanged => (Vec::new(), false),
    }
}

pub(super) fn equivalent(
    a: &[u8],
    b: &[u8],
    options: ContentOptions,
    cancel: &AtomicBool,
) -> Result<bool, DiffError> {
    if a == b {
        return Ok(true);
    }
    if options.whitespace == Whitespace::Exact {
        return Ok(false);
    }
    Ok(keys(a, options.whitespace, options.limits, cancel)?.bytes
        == keys(b, options.whitespace, options.limits, cancel)?.bytes)
}

struct Keys {
    bytes: Vec<u8>,
    offsets: Vec<usize>,
}

fn keys(
    input: &[u8],
    policy: Whitespace,
    limits: DiffLimits,
    cancel: &AtomicBool,
) -> Result<Keys, DiffError> {
    let mut bytes = Vec::new();
    let mut offsets = vec![0];
    let mut offset = 0;
    for line in input.split_inclusive(|b| *b == b'\n') {
        check(cancel)?;
        if offsets.len() > limits.max_lines {
            return Err(DiffError::Limit("lines"));
        }
        let body = line.strip_suffix(b"\n").unwrap_or(line);
        line_key(body, policy, &mut bytes, cancel)?;
        // This suffix preserves one record even when its normalized body is empty and
        // distinguishes final LF from EOF. Original bodies cannot contain the LF delimiter.
        bytes.push(u8::from(line.ends_with(b"\n")));
        bytes.push(b'\n');
        offset += line.len();
        offsets.push(offset);
    }
    Ok(Keys { bytes, offsets })
}

fn line_key(
    body: &[u8],
    policy: Whitespace,
    output: &mut Vec<u8>,
    cancel: &AtomicBool,
) -> Result<(), DiffError> {
    match policy {
        Whitespace::Exact => output.extend_from_slice(body),
        Whitespace::IgnoreSpaceAtEol => {
            let end = body
                .iter()
                .rposition(|b| !b.is_ascii_whitespace())
                .map_or(0, |p| p + 1);
            output.extend_from_slice(&body[..end]);
        }
        Whitespace::IgnoreCrAtEol => {
            output.extend_from_slice(body.strip_suffix(b"\r").unwrap_or(body))
        }
        Whitespace::IgnoreSpaceChange | Whitespace::IgnoreAllSpace => {
            let mut space = false;
            for (position, byte) in body.iter().enumerate() {
                if position % 65_536 == 0 {
                    check(cancel)?;
                }
                if byte.is_ascii_whitespace() {
                    space = true;
                } else {
                    if space && policy == Whitespace::IgnoreSpaceChange {
                        output.push(b' ');
                    }
                    output.push(*byte);
                    space = false;
                }
            }
        }
    }
    Ok(())
}

fn check(cancel: &AtomicBool) -> Result<(), DiffError> {
    if cancel.load(Ordering::Relaxed) {
        Err(DiffError::Cancelled)
    } else {
        Ok(())
    }
}
