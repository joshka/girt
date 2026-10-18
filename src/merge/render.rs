use super::{ConflictStyle, ContentHunk, ContentMerge, MarkerOptions};

impl ContentMerge {
    /// Renders resolved bytes and conflict markers without changing the stored result.
    ///
    /// Binary conflicts emit ours verbatim. Text conflict payloads receive a terminating LF
    /// when needed to put the following marker on its own line. Resolved bytes remain exact.
    /// Merge and zdiff3 move common complete boundary lines outside markers; diff3 retains
    /// full region payloads. Labels are emitted verbatim, preceded by a space when nonempty.
    pub fn render(&self, options: &MarkerOptions) -> Vec<u8> {
        let mut output = Vec::new();
        for hunk in &self.hunks {
            match hunk {
                ContentHunk::Resolved(bytes) => output.extend_from_slice(bytes),
                ContentHunk::Conflict { ours, .. } if self.binary => output.extend_from_slice(ours),
                ContentHunk::Conflict { base, ours, theirs } => {
                    render_conflict(&mut output, base, ours, theirs, options);
                }
            }
        }
        output
    }
}

fn render_conflict(
    output: &mut Vec<u8>,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    options: &MarkerOptions,
) {
    let (prefix, suffix) = if options.style == ConflictStyle::Diff3 {
        (0, 0)
    } else {
        common_boundaries(ours, theirs)
    };
    output.extend_from_slice(&ours[..prefix]);
    marker(output, b'<', &options.ours, options);
    payload(output, &ours[prefix..ours.len() - suffix]);
    if options.style != ConflictStyle::Merge {
        marker(output, b'|', &options.base, options);
        payload(output, base);
    }
    marker(output, b'=', &[], options);
    payload(output, &theirs[prefix..theirs.len() - suffix]);
    marker(output, b'>', &options.theirs, options);
    output.extend_from_slice(&ours[ours.len() - suffix..]);
}

pub(super) fn common_boundaries(ours: &[u8], theirs: &[u8]) -> (usize, usize) {
    let mut prefix = 0;
    for (a, b) in ours
        .split_inclusive(|b| *b == b'\n')
        .zip(theirs.split_inclusive(|b| *b == b'\n'))
    {
        if a != b {
            break;
        }
        prefix += a.len();
    }
    let a = &ours[prefix..];
    let b = &theirs[prefix..];
    // Reverse iteration must retain the LF on its line, including unterminated final lines.
    let a: Vec<_> = a.split_inclusive(|b| *b == b'\n').collect();
    let b: Vec<_> = b.split_inclusive(|b| *b == b'\n').collect();
    let mut suffix = 0;
    for (a, b) in a.iter().rev().zip(b.iter().rev()) {
        if a != b {
            break;
        }
        suffix += a.len();
    }
    (prefix, suffix)
}

fn marker(output: &mut Vec<u8>, byte: u8, label: &[u8], options: &MarkerOptions) {
    output.extend(std::iter::repeat_n(byte, options.width.get()));
    if !label.is_empty() {
        output.push(b' ');
        output.extend_from_slice(label);
    }
    output.push(b'\n');
}

fn payload(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(bytes);
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        output.push(b'\n');
    }
}
