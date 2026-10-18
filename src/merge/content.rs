use std::sync::atomic::AtomicBool;

use super::{ContentHunk, ContentMerge, ContentOptions, Error, Favor};
use crate::content_diff::ContentEdit;

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
/// combined input size. [`super::Whitespace`] uses comparison-only keys and original line ranges;
/// output retains accepted original bytes and final-LF distinctions. A side with no comparison
/// edits selects the entire opposite payload; two edit-free comparisons select theirs. Automatic
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
    let (left, left_binary) = super::whitespace::compare(base, ours, options, cancel)?;
    let (right, right_binary) = super::whitespace::compare(base, theirs, options, cancel)?;
    let binary = left_binary || right_binary;
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
    // Git selects the entire opposite payload when one comparison has no edits,
    // including whitespace-only differences. Both empty comparisons select theirs.
    if left.is_empty() {
        return Ok(resolved(theirs, false));
    }
    if right.is_empty() {
        return Ok(resolved(ours, false));
    }
    combine([base, ours, theirs], &left, &right, options, cancel)
}

fn resolved(bytes: &[u8], binary: bool) -> ContentMerge {
    ContentMerge {
        hunks: vec![ContentHunk::Resolved(bytes.to_vec())],
        binary,
    }
}

fn combine(
    inputs: [&[u8]; 3],
    left: &[ContentEdit],
    right: &[ContentEdit],
    options: ContentOptions,
    cancel: &AtomicBool,
) -> Result<ContentMerge, Error> {
    let [base, ours, theirs] = inputs;
    let mut hunks = Vec::new();
    let (mut l, mut r, mut cursor) = (0, 0, 0);
    let (mut ours_cursor, mut theirs_cursor) = (0, 0);
    while l < left.len() || r < right.len() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(crate::content_diff::DiffError::Cancelled.into());
        }
        let start = left
            .get(l)
            .map_or(usize::MAX, |e| e.old_bytes.start)
            .min(right.get(r).map_or(usize::MAX, |e| e.old_bytes.start));
        let gap = records(&base[cursor..start]);
        let ours_start = advance(ours, ours_cursor, gap);
        let theirs_start = advance(theirs, theirs_cursor, gap);
        append(&mut hunks, &ours[ours_cursor..ours_start]);
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
        let (a, ours_end) = apply(base, ours, &left[begin_l..l], start..end, ours_start);
        let (b, theirs_end) = apply(base, theirs, &right[begin_r..r], start..end, theirs_start);
        let original = &base[start..end];
        let hunk = if begin_r == r {
            ContentHunk::Resolved(a)
        } else if begin_l == l {
            ContentHunk::Resolved(b)
        } else if super::whitespace::equivalent(&a, &b, options, cancel)? {
            ContentHunk::Resolved(a)
        } else {
            choose(original.to_vec(), a, b, options.favor, false)
        };
        match hunk {
            ContentHunk::Resolved(bytes) => append(&mut hunks, &bytes),
            conflict => hunks.push(conflict),
        }
        cursor = end;
        ours_cursor = ours_end;
        theirs_cursor = theirs_end;
    }
    append(&mut hunks, &ours[ours_cursor..]);
    Ok(ContentMerge {
        hunks,
        binary: false,
    })
}

fn apply(
    base: &[u8],
    side: &[u8],
    edits: &[ContentEdit],
    region: std::ops::Range<usize>,
    side_start: usize,
) -> (Vec<u8>, usize) {
    let mut bytes = Vec::new();
    let mut cursor = region.start;
    let mut side_cursor = side_start;
    for edit in edits {
        let matched = advance(
            side,
            side_cursor,
            records(&base[cursor..edit.old_bytes.start]),
        );
        bytes.extend_from_slice(&side[side_cursor..matched]);
        bytes.extend_from_slice(&side[edit.new_bytes.clone()]);
        cursor = edit.old_bytes.end;
        side_cursor = edit.new_bytes.end;
    }
    let end = advance(side, side_cursor, records(&base[cursor..region.end]));
    bytes.extend_from_slice(&side[side_cursor..end]);
    (bytes, end)
}

fn records(bytes: &[u8]) -> usize {
    bytes.split_inclusive(|b| *b == b'\n').count()
}

fn advance(bytes: &[u8], start: usize, records: usize) -> usize {
    start
        + bytes[start..]
            .split_inclusive(|b| *b == b'\n')
            .take(records)
            .map(<[u8]>::len)
            .sum::<usize>()
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
