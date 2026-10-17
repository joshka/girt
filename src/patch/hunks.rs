use std::io;
use std::ops::Range;

use crate::content_diff::TextDiff;

/// Writes the unified hunks of one text comparison, starting at the first `@@` line.
///
/// Each hunk shows `context_lines` unchanged lines around its changes, as `--unified=<n>` does;
/// changes separated by at most twice that many unchanged lines share a hunk. Ranges follow Git:
/// a count of one is omitted and an empty side starts at the line before it. The header ends
/// with Git's default function context: the nearest original line before the hunk that starts
/// with an ASCII letter, `_` or `$`, cut to 80 bytes and stripped of trailing whitespace.
///
/// Lines are written with their exact bytes after a ` `, `-` or `+` marker. A line without a
/// final LF is followed by `\ No newline at end of file`. No file header is written; see
/// [`Objects::write_patch`](crate::Objects::write_patch) for complete patches. Work is linear in
/// the bytes shown and searched for function context; nothing is buffered.
///
/// # Errors
///
/// Returns the writer's error; earlier lines remain written.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::content_diff::{BinaryMode, ContentDiff, DiffLimits, diff};
/// use girt::patch::write_hunks;
///
/// let old = b"fn main() {\n    one();\n    two();\n}\n";
/// let new = b"fn main() {\n    one();\n    three();\n}\n";
/// let ContentDiff::Text(text) = diff(
///     old,
///     new,
///     BinaryMode::Auto,
///     DiffLimits::default(),
///     &AtomicBool::new(false),
/// )?
/// else {
///     panic!("expected text");
/// };
/// let mut hunks = Vec::new();
/// write_hunks(&mut hunks, &text, 0)?;
/// assert_eq!(
///     hunks,
///     b"@@ -3 +3 @@ fn main() {\n-    two();\n+    three();\n"
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn write_hunks(
    mut out: impl io::Write,
    text: &TextDiff<'_>,
    context_lines: usize,
) -> io::Result<()> {
    let (old, new, edits) = (text.old_bytes(), text.new_bytes(), text.edits());
    let mut function = FunctionContext::default();
    let mut index = 0;
    while index < edits.len() {
        let mut end = index;
        while end + 1 < edits.len()
            && edits[end + 1].old_lines.start - edits[end].old_lines.end
                <= context_lines.saturating_mul(2)
        {
            end += 1;
        }
        let (first, last) = (&edits[index], &edits[end]);
        let leading = context_lines.min(first.old_lines.start);
        let start = lines_before(old, first.old_bytes.start, leading);
        let (finish, trailing) = lines_after(old, last.old_bytes.end, context_lines);
        let old_start = first.old_lines.start - leading;
        let new_start = first.new_lines.start - leading;
        let old_count = last.old_lines.end + trailing - old_start;
        let new_count = last.new_lines.end + trailing - new_start;
        write!(out, "@@ -")?;
        write_range(&mut out, old_start, old_count)?;
        write!(out, " +")?;
        write_range(&mut out, new_start, new_count)?;
        write!(out, " @@")?;
        if let Some(name) = function.before(old, start) {
            out.write_all(b" ")?;
            out.write_all(name)?;
        }
        out.write_all(b"\n")?;
        let mut position = start;
        for edit in &edits[index..=end] {
            write_lines(&mut out, b' ', &old[position..edit.old_bytes.start])?;
            write_lines(&mut out, b'-', &old[edit.old_bytes.clone()])?;
            write_lines(&mut out, b'+', &new[edit.new_bytes.clone()])?;
            position = edit.old_bytes.end;
        }
        write_lines(&mut out, b' ', &old[position..finish])?;
        index = end + 1;
    }
    Ok(())
}

/// Writes a hunk range as `start,count`, omitting `,1` and using the preceding line when empty.
fn write_range(out: &mut impl io::Write, start: usize, count: usize) -> io::Result<()> {
    match count {
        0 => write!(out, "{start},0"),
        1 => write!(out, "{}", start + 1),
        _ => write!(out, "{},{count}", start + 1),
    }
}

/// Writes each LF-delimited line of `bytes` behind `marker`.
fn write_lines(out: &mut impl io::Write, marker: u8, bytes: &[u8]) -> io::Result<()> {
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        out.write_all(&[marker])?;
        out.write_all(line)?;
        if !line.ends_with(b"\n") {
            out.write_all(b"\n\\ No newline at end of file\n")?;
        }
    }
    Ok(())
}

/// The start offset of the `count` lines ending at line boundary `end`.
fn lines_before(bytes: &[u8], end: usize, count: usize) -> usize {
    let mut start = end;
    for _ in 0..count {
        // Every line before a boundary ends with LF, so skip it and find the previous LF.
        start = bytes[..start - 1]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |lf| lf + 1);
    }
    start
}

/// The end offset and number of up to `count` lines starting at line boundary `start`.
fn lines_after(bytes: &[u8], start: usize, count: usize) -> (usize, usize) {
    let mut end = start;
    let mut lines = 0;
    while lines < count && end < bytes.len() {
        end = bytes[end..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |lf| end + lf + 1);
        lines += 1;
    }
    (end, lines)
}

/// Finds hunk-header function names, scanning each original byte at most once across hunks.
#[derive(Default)]
struct FunctionContext {
    /// Lines before this offset have been searched.
    searched: usize,
    /// The nearest match before `searched`.
    found: Option<Range<usize>>,
}

impl FunctionContext {
    /// The nearest function line before line boundary `start`, which never decreases.
    fn before<'a>(&mut self, bytes: &'a [u8], start: usize) -> Option<&'a [u8]> {
        let mut end = start;
        while end > self.searched {
            let line_start = bytes[..end - 1]
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |lf| lf + 1);
            if matches!(bytes[line_start], b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$') {
                self.found = Some(line_start..end);
                break;
            }
            end = line_start;
        }
        self.searched = start;
        let line = &bytes[self.found.clone()?];
        Some(line[..line.len().min(80)].trim_ascii_end())
    }
}
