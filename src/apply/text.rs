use super::{Applied, Error, FilePatch, HunkResult, Limits};

pub(super) const OUTPUT_BYTES: &str = "output bytes";

pub(super) fn apply(file: &FilePatch, preimage: &[u8], limits: Limits) -> Result<Applied, Error> {
    if preimage.len() > limits.max_input_bytes {
        return Err(Error::Limit("input bytes"));
    }
    if file.hunks.len() > limits.max_hunks {
        return Err(Error::Limit("hunks"));
    }
    let mut result = Applied {
        content: Vec::new(),
        hunks: Vec::new(),
    };
    let mut remaining = preimage;
    let mut old_line = 0;
    let mut new_line = 0;
    let mut postimage_ended = false;
    let mut remaining_lines = limits.max_lines;
    for hunk in &file.hunks {
        let mismatch = || Error::Mismatch { line: hunk.line };
        if postimage_ended
            || hunk.old_start < old_line
            || (file.old_path.is_none() && !preimage.is_empty())
        {
            return Err(mismatch());
        }
        while old_line < hunk.old_start {
            let line = take_line(&mut remaining).ok_or_else(mismatch)?;
            append(&mut result.content, line, limits)?;
            old_line += 1;
            new_line += 1;
        }
        if hunk.new_start != new_line {
            return Err(mismatch());
        }
        result.hunks.push(HunkResult {
            patch_line: hunk.line,
            old_start: old_line,
            new_start: new_line,
        });
        for line in &hunk.lines {
            remaining_lines = remaining_lines
                .checked_sub(1)
                .ok_or(Error::Limit("content lines"))?;
            if line.kind != b'+' {
                let actual = take_line(&mut remaining).ok_or_else(mismatch)?;
                if actual != line.bytes {
                    return Err(mismatch());
                }
                old_line += 1;
            }
            if line.kind != b'-' {
                if postimage_ended {
                    return Err(mismatch());
                }
                postimage_ended = !line.bytes.ends_with(b"\n");
                append(&mut result.content, &line.bytes, limits)?;
                new_line += 1;
            }
        }
    }
    if postimage_ended && !remaining.is_empty() {
        return Err(Error::Mismatch {
            line: file.hunks.last().unwrap().line,
        });
    }
    append(&mut result.content, remaining, limits)?;
    if file.new_path.is_none() && !result.content.is_empty() {
        return Err(Error::Mismatch {
            line: file.hunks.last().unwrap().line,
        });
    }
    Ok(result)
}

fn take_line<'a>(remaining: &mut &'a [u8]) -> Option<&'a [u8]> {
    if remaining.is_empty() {
        return None;
    }
    let end = remaining
        .iter()
        .position(|b| *b == b'\n')
        .map_or(remaining.len(), |i| i + 1);
    let (line, rest) = remaining.split_at(end);
    *remaining = rest;
    Some(line)
}

fn append(output: &mut Vec<u8>, bytes: &[u8], limits: Limits) -> Result<(), Error> {
    if bytes.len() > limits.max_output_bytes.saturating_sub(output.len()) {
        return Err(Error::Limit(OUTPUT_BYTES));
    }
    output.extend_from_slice(bytes);
    Ok(())
}
