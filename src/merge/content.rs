use std::sync::atomic::AtomicBool;

use super::{ContentHunk, ContentMerge, ContentOptions, Error, Favor};
use crate::content_diff::{self, ContentDiff, ContentEdit};

/// Merges base, ours and theirs with default comparison bounds and automatic binary detection.
///
/// This is [`content_with`] with default options and no cancellation.
///
/// # Errors
///
/// Returns bounded comparison errors; no external state changes.
pub fn content(base: &[u8], ours: &[u8], theirs: &[u8]) -> Result<ContentMerge, Error> {
    content_with(
        base,
        ours,
        theirs,
        ContentOptions::default(),
        &AtomicBool::new(false),
    )
}

/// Merges LF-delimited byte lines, retaining unresolved three-way regions.
///
/// Exact equality resolves identical sides or one unchanged side before conflict classification.
/// Each comparison independently enforces [`ContentOptions::limits`], even for equal inputs.
/// Total comparison work is at most twice that bound. Output and retained regions are linear in
/// combined input size; neither missing final LF nor invalid UTF-8 is normalized. Automatic
/// binary classification tests for NUL anywhere in any input. Union does not resolve binary
/// conflicts. Cancellation is checked by both comparisons and between output regions.
///
/// # Errors
///
/// Returns [`Error::Diff`] on input, line, trace or work exhaustion or cancellation. No partial
/// result is returned. Clearing cancellation and raising bounds permits a fresh attempt.
pub fn content_with(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    options: ContentOptions,
    cancel: &AtomicBool,
) -> Result<ContentMerge, Error> {
    let left = content_diff::diff(base, ours, options.binary, options.limits, cancel)?;
    let right = content_diff::diff(base, theirs, options.binary, options.limits, cancel)?;
    let binary =
        matches!(left, ContentDiff::Binary { .. }) || matches!(right, ContentDiff::Binary { .. });
    if ours == theirs || base == theirs {
        return Ok(resolved(ours, binary));
    }
    if base == ours {
        return Ok(resolved(theirs, binary));
    }
    if binary {
        let hunk = choose(
            base.to_vec(),
            ours.to_vec(),
            theirs.to_vec(),
            options.favor,
            true,
        );
        return Ok(ContentMerge {
            hunks: vec![hunk],
            binary,
        });
    }
    let left = edits(left);
    let right = edits(right);
    combine(base, ours, theirs, &left, &right, options.favor, cancel)
}

fn resolved(bytes: &[u8], binary: bool) -> ContentMerge {
    ContentMerge {
        hunks: vec![ContentHunk::Resolved(bytes.to_vec())],
        binary,
    }
}

fn edits(diff: ContentDiff<'_>) -> Vec<ContentEdit> {
    match diff {
        ContentDiff::Text(text) => text.edits().to_vec(),
        _ => Vec::new(),
    }
}

fn combine(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    left: &[ContentEdit],
    right: &[ContentEdit],
    favor: Favor,
    cancel: &AtomicBool,
) -> Result<ContentMerge, Error> {
    let mut hunks = Vec::new();
    let (mut l, mut r, mut cursor) = (0, 0, 0);
    while l < left.len() || r < right.len() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(crate::content_diff::DiffError::Cancelled.into());
        }
        let start = left
            .get(l)
            .map_or(usize::MAX, |e| e.old_bytes.start)
            .min(right.get(r).map_or(usize::MAX, |e| e.old_bytes.start));
        append(&mut hunks, &base[cursor..start]);
        let (begin_l, begin_r) = (l, r);
        let mut end = start;
        // Connected base intervals include touching edits. Extend until neither side grows.
        loop {
            let previous = (l, r);
            while l < left.len() && left[l].old_bytes.start <= end {
                end = end.max(left[l].old_bytes.end);
                l += 1;
            }
            while r < right.len() && right[r].old_bytes.start <= end {
                end = end.max(right[r].old_bytes.end);
                r += 1;
            }
            if previous == (l, r) {
                break;
            }
        }
        let a = apply(base, ours, &left[begin_l..l], start, end);
        let b = apply(base, theirs, &right[begin_r..r], start, end);
        let original = &base[start..end];
        let hunk = if a == b || b == original {
            ContentHunk::Resolved(a)
        } else if a == original {
            ContentHunk::Resolved(b)
        } else {
            choose(original.to_vec(), a, b, favor, false)
        };
        match hunk {
            ContentHunk::Resolved(bytes) => append(&mut hunks, &bytes),
            conflict => hunks.push(conflict),
        }
        cursor = end;
    }
    append(&mut hunks, &base[cursor..]);
    Ok(ContentMerge {
        hunks,
        binary: false,
    })
}

fn apply(base: &[u8], side: &[u8], edits: &[ContentEdit], start: usize, end: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut cursor = start;
    for edit in edits {
        bytes.extend_from_slice(&base[cursor..edit.old_bytes.start]);
        bytes.extend_from_slice(&side[edit.new_bytes.clone()]);
        cursor = edit.old_bytes.end;
    }
    bytes.extend_from_slice(&base[cursor..end]);
    bytes
}

fn choose(
    base: Vec<u8>,
    mut ours: Vec<u8>,
    theirs: Vec<u8>,
    favor: Favor,
    binary: bool,
) -> ContentHunk {
    match favor {
        Favor::Ours => ContentHunk::Resolved(ours),
        Favor::Theirs => ContentHunk::Resolved(theirs),
        Favor::Union if !binary => {
            let (prefix, suffix) = super::render::common_boundaries(&ours, &theirs);
            let tail = ours.split_off(ours.len() - suffix);
            let replacement = &theirs[prefix..theirs.len() - suffix];
            if !ours.is_empty() && !ours.ends_with(b"\n") && !replacement.is_empty() {
                ours.push(b'\n');
            }
            ours.extend_from_slice(replacement);
            ours.extend_from_slice(&tail);
            ContentHunk::Resolved(ours)
        }
        _ => ContentHunk::Conflict { base, ours, theirs },
    }
}

fn append(hunks: &mut Vec<ContentHunk>, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    if let Some(ContentHunk::Resolved(previous)) = hunks.last_mut() {
        previous.extend_from_slice(bytes);
    } else {
        hunks.push(ContentHunk::Resolved(bytes.to_vec()));
    }
}
