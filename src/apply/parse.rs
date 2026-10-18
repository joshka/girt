use super::{Error, FilePatch, Hunk, Limits, Line, Patch};

pub(super) fn parse(bytes: &[u8], limits: Limits) -> Result<Patch, Error> {
    if bytes.len() > limits.max_input_bytes {
        return Err(Error::Limit("input bytes"));
    }
    let mut parser = Parser {
        lines: bytes.split_inclusive(is_lf as fn(&u8) -> bool).peekable(),
        line: 0,
        remaining_lines: limits.max_lines,
    };
    let mut files = Vec::new();
    let mut total_hunks = 0;
    while let Some(line) = parser.lines.peek().copied() {
        refuse(line, parser.line + 1)?;
        if files.len() >= limits.max_files {
            return Err(Error::Limit("files"));
        }
        let file = if line.starts_with(b"diff --git ") {
            parser.git_file(limits, &mut total_hunks)?
        } else if line.starts_with(b"--- ") {
            let mut file = empty_file(parser.line + 1);
            parser.text_file(&mut file, limits, &mut total_hunks)?;
            file
        } else {
            return Err(malformed(parser.line + 1, "expected file header"));
        };
        files.push(file);
    }
    if files.is_empty() {
        return Err(malformed(1, "no file edits"));
    }
    Ok(Patch { files })
}

fn empty_file(line: usize) -> FilePatch {
    FilePatch {
        old_path: None,
        new_path: None,
        hunks: Vec::new(),
        line,
        old_mode: None,
        new_mode: None,
        operation: super::FileOperation::Edit,
        similarity: None,
    }
}

type Lines<'a> = std::iter::Peekable<std::slice::SplitInclusive<'a, u8, fn(&u8) -> bool>>;

struct Parser<'a> {
    lines: Lines<'a>,
    line: usize,
    remaining_lines: usize,
}

impl<'a> Parser<'a> {
    fn next(&mut self) -> Option<&'a [u8]> {
        let next = self.lines.next()?;
        self.line += 1;
        Some(next)
    }

    fn git_file(&mut self, limits: Limits, total_hunks: &mut usize) -> Result<FilePatch, Error> {
        let header = self.next().unwrap();
        let mut file = empty_file(self.line);
        let fallback = git_paths(trim_lf(&header[11..]), self.line);
        let mut index_seen = false;
        let mut operation_paths: [Option<Vec<u8>>; 2] = [None, None];
        let (mut added, mut deleted) = (false, false);
        while let Some(line) = self.lines.peek().copied() {
            if line.starts_with(b"diff --git ") || line.starts_with(b"--- ") {
                break;
            }
            refuse(line, self.line + 1)?;
            self.next();
            let at = self.line;
            if let Some(bytes) = line.strip_prefix(b"index ") {
                if index_seen {
                    return Err(malformed(at, "duplicate index header"));
                }
                validate_index(trim_lf(bytes), at)?;
                if let Some(mode) = trim_lf(bytes).split(|b| *b == b' ').nth(1) {
                    let mode = mode_value(mode, at)?;
                    if file.old_mode.is_none() {
                        file.old_mode = Some(mode);
                    }
                    if file.new_mode.is_none() {
                        file.new_mode = Some(mode);
                    }
                }
                index_seen = true;
            } else if let Some(bytes) = line.strip_prefix(b"old mode ") {
                set_mode(&mut file.old_mode, mode_value(trim_lf(bytes), at)?, at)?;
            } else if let Some(bytes) = line.strip_prefix(b"new mode ") {
                set_mode(&mut file.new_mode, mode_value(trim_lf(bytes), at)?, at)?;
            } else if let Some(bytes) = line.strip_prefix(b"new file mode ") {
                if added || deleted {
                    return Err(malformed(at, "conflicting file creation/deletion"));
                }
                added = true;
                set_mode(&mut file.new_mode, mode_value(trim_lf(bytes), at)?, at)?;
            } else if let Some(bytes) = line.strip_prefix(b"deleted file mode ") {
                if added || deleted {
                    return Err(malformed(at, "conflicting file creation/deletion"));
                }
                deleted = true;
                set_mode(&mut file.old_mode, mode_value(trim_lf(bytes), at)?, at)?;
            } else if let Some(bytes) = line.strip_prefix(b"similarity index ") {
                if file.similarity.is_some() {
                    return Err(malformed(at, "duplicate similarity"));
                }
                let value = trim_lf(bytes)
                    .strip_suffix(b"%")
                    .ok_or_else(|| malformed(at, "invalid similarity"))?;
                let value = number(value, at)?;
                if value > 100 {
                    return Err(malformed(at, "invalid similarity"));
                }
                file.similarity = Some(value as u8);
            } else if let Some((operation, side, bytes)) = operation_header(line) {
                if file.operation != super::FileOperation::Edit && file.operation != operation {
                    return Err(malformed(at, "conflicting rename/copy"));
                }
                file.operation = operation;
                if operation_paths[side].is_some() {
                    return Err(malformed(at, "duplicate operation path"));
                }
                let raw = trim_lf(bytes);
                let name = if raw.starts_with(b"\"") {
                    quoted_path(raw, at)?
                } else {
                    raw.to_vec()
                };
                if name.is_empty() || name.contains(&0) {
                    return Err(malformed(at, "invalid operation path"));
                }
                let mut path = if side == 0 {
                    b"a/".to_vec()
                } else {
                    b"b/".to_vec()
                };
                path.extend(name);
                operation_paths[side] = Some(path);
            } else {
                return Err(malformed(at, "unknown extended header"));
            }
        }
        if self
            .lines
            .peek()
            .is_some_and(|line| line.starts_with(b"--- "))
        {
            self.text_file(&mut file, limits, total_hunks)?;
        }
        if !file.hunks.is_empty()
            && ((added && file.old_path.is_some()) || (deleted && file.new_path.is_some()))
        {
            return Err(malformed(file.line, "file kind disagrees with text paths"));
        }
        if file.hunks.is_empty() {
            let paths = match (&operation_paths[0], &operation_paths[1]) {
                (Some(old), Some(new)) => (old.clone(), new.clone()),
                (None, None) => fallback?,
                _ => return Err(malformed(file.line, "incomplete operation paths")),
            };
            if paths.0.is_empty()
                || paths.1.is_empty()
                || paths.0.contains(&0)
                || paths.1.contains(&0)
            {
                return Err(malformed(file.line, "invalid Git path"));
            }
            file.old_path = (!added).then_some(paths.0);
            file.new_path = (!deleted).then_some(paths.1);
        }
        if file.operation != super::FileOperation::Edit {
            if added || deleted || operation_paths.iter().any(Option::is_none) {
                return Err(malformed(file.line, "incomplete or conflicting operation"));
            }
            if file.old_path != operation_paths[0] || file.new_path != operation_paths[1] {
                return Err(malformed(
                    file.line,
                    "operation paths disagree with text headers",
                ));
            }
        } else if file.similarity.is_some() {
            return Err(malformed(file.line, "similarity without rename/copy"));
        }
        if file.hunks.is_empty()
            && file.operation == super::FileOperation::Edit
            && !added
            && !deleted
            && (file.old_mode.is_none()
                || file.new_mode.is_none()
                || file.old_mode == file.new_mode)
        {
            return Err(malformed(file.line, "no file changes"));
        }
        if added {
            file.old_path = None;
            file.old_mode = None;
        }
        if deleted {
            file.new_path = None;
            file.new_mode = None;
        }
        Ok(file)
    }

    fn text_file(
        &mut self,
        file: &mut FilePatch,
        limits: Limits,
        total_hunks: &mut usize,
    ) -> Result<(), Error> {
        let old = self.next().unwrap();
        file.old_path = path(&old[4..], self.line)?;
        let new = self
            .next()
            .ok_or_else(|| malformed(self.line, "missing new path"))?;
        if !new.starts_with(b"+++ ") {
            return Err(malformed(self.line, "expected new path"));
        }
        file.new_path = path(&new[4..], self.line)?;
        if file.old_path.is_none() && file.new_path.is_none() {
            return Err(malformed(self.line, "both paths absent"));
        }
        while self
            .lines
            .peek()
            .is_some_and(|line| line.starts_with(b"@@ "))
        {
            if *total_hunks >= limits.max_hunks {
                return Err(Error::Limit("hunks"));
            }
            file.hunks.push(self.hunk()?);
            *total_hunks += 1;
        }
        if let Some(next) = self.lines.peek() {
            refuse(next, self.line + 1)?;
        }
        if file.hunks.is_empty() {
            return Err(malformed(self.line, "missing hunks"));
        }
        Ok(())
    }

    fn hunk(&mut self) -> Result<Hunk, Error> {
        let header = self.next().unwrap();
        let line = self.line;
        let (old_start, old_count, new_start, new_count) = header_ranges(header, line)?;
        let mut lines: Vec<Line> = Vec::new();
        let (mut old, mut new) = (0usize, 0usize);
        let (mut old_ended, mut new_ended) = (false, false);
        while old < old_count || new < new_count {
            let bytes = self
                .next()
                .ok_or_else(|| malformed(self.line, "truncated hunk"))?;
            let kind = bytes[0];
            if !matches!(kind, b' ' | b'-' | b'+') || !bytes.ends_with(b"\n") {
                return Err(malformed(self.line, "invalid hunk line"));
            }
            if (kind != b'+' && old_ended) || (kind != b'-' && new_ended) {
                return Err(malformed(
                    self.line,
                    "content follows missing final newline",
                ));
            }
            old += usize::from(kind != b'+');
            new += usize::from(kind != b'-');
            if old > old_count || new > new_count {
                return Err(malformed(self.line, "hunk count mismatch"));
            }
            self.remaining_lines = self
                .remaining_lines
                .checked_sub(1)
                .ok_or(Error::Limit("content lines"))?;
            lines.push(Line {
                kind,
                bytes: bytes[1..].to_vec(),
            });
            if self
                .lines
                .peek()
                .is_some_and(|line| line.starts_with(b"\\ No newline"))
            {
                let marker = self.next().unwrap();
                if trim_lf(marker) != b"\\ No newline at end of file" {
                    return Err(malformed(self.line, "invalid newline marker"));
                }
                lines.last_mut().unwrap().bytes.pop();
                old_ended |= kind != b'+';
                new_ended |= kind != b'-';
            }
        }
        Ok(Hunk {
            line,
            old_start,
            new_start,
            lines,
        })
    }
}

fn header_ranges(header: &[u8], line: usize) -> Result<(usize, usize, usize, usize), Error> {
    let mut fields = header.split(|b| *b == b' ');
    if fields.next() != Some(b"@@".as_slice()) {
        return Err(malformed(line, "invalid hunk header"));
    }
    let old = fields
        .next()
        .ok_or_else(|| malformed(line, "missing old range"))?;
    let new = fields
        .next()
        .ok_or_else(|| malformed(line, "missing new range"))?;
    if !fields.next().is_some_and(|field| trim_lf(field) == b"@@") {
        return Err(malformed(line, "missing hunk terminator"));
    }
    let (old_start, old_count) = range(old, b'-', line)?;
    let (new_start, new_count) = range(new, b'+', line)?;
    Ok((old_start, old_count, new_start, new_count))
}

fn range(bytes: &[u8], prefix: u8, line: usize) -> Result<(usize, usize), Error> {
    if bytes.first() != Some(&prefix) {
        return Err(malformed(line, "invalid range prefix"));
    }
    let mut parts = bytes[1..].split(|b| *b == b',');
    let start = number(parts.next().unwrap(), line)?;
    let count = parts
        .next()
        .map(|part| number(part, line))
        .transpose()?
        .unwrap_or(1);
    if parts.next().is_some() || (start == 0 && count != 0) {
        return Err(malformed(line, "invalid range"));
    }
    Ok((if count == 0 { start } else { start - 1 }, count))
}

fn number(bytes: &[u8], line: usize) -> Result<usize, Error> {
    if bytes.is_empty() {
        return Err(malformed(line, "empty range number"));
    }
    bytes.iter().try_fold(0usize, |n, b| {
        if !b.is_ascii_digit() {
            return Err(malformed(line, "invalid range number"));
        }
        n.checked_mul(10)
            .and_then(|n| n.checked_add((b - b'0') as usize))
            .ok_or_else(|| malformed(line, "range overflow"))
    })
}

fn path(bytes: &[u8], line: usize) -> Result<Option<Vec<u8>>, Error> {
    let bytes = trim_lf(bytes);
    let path = if bytes.first() == Some(&b'"') {
        quoted_path(bytes, line)?
    } else {
        bytes.split(|b| *b == b'\t').next().unwrap().to_vec()
    };
    if path.is_empty() || path.contains(&0) {
        return Err(malformed(line, "empty or NUL path"));
    }
    Ok((path != b"/dev/null").then_some(path))
}

fn quoted_path(bytes: &[u8], line: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        if b == b'"' {
            if i != bytes.len() && bytes[i] != b'\t' {
                return Err(malformed(line, "trailing quoted path bytes"));
            }
            return Ok(out);
        }
        if b != b'\\' {
            out.push(b);
            continue;
        }
        let escape = *bytes
            .get(i)
            .ok_or_else(|| malformed(line, "truncated path escape"))?;
        i += 1;
        let value = match escape {
            b'a' => 7,
            b'b' => 8,
            b't' => 9,
            b'n' => 10,
            b'v' => 11,
            b'f' => 12,
            b'r' => 13,
            b'"' => b'"',
            b'\\' => b'\\',
            b'0'..=b'3' => {
                let rest = bytes
                    .get(i..i + 2)
                    .ok_or_else(|| malformed(line, "truncated octal escape"))?;
                if !rest.iter().all(|b| matches!(b, b'0'..=b'7')) {
                    return Err(malformed(line, "invalid octal escape"));
                }
                i += 2;
                (escape - b'0') * 64 + (rest[0] - b'0') * 8 + (rest[1] - b'0')
            }
            _ => return Err(malformed(line, "invalid path escape")),
        };
        out.push(value);
    }
    Err(malformed(line, "unterminated quoted path"))
}

fn refuse(line: &[u8], at: usize) -> Result<(), Error> {
    let feature = if line.starts_with(b"diff --cc ")
        || line.starts_with(b"diff --combined ")
        || line.starts_with(b"@@@")
    {
        "combined diff"
    } else if line.starts_with(b"GIT binary patch") || line.starts_with(b"Binary files ") {
        "binary patch"
    } else {
        return Ok(());
    };
    Err(Error::Unsupported { line: at, feature })
}

fn trim_lf(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

fn malformed(line: usize, reason: &'static str) -> Error {
    Error::Malformed { line, reason }
}

fn is_lf(byte: &u8) -> bool {
    *byte == b'\n'
}

fn validate_index(bytes: &[u8], line: usize) -> Result<(), Error> {
    let mut fields = bytes.split(|b| *b == b' ');
    let ids = fields.next().unwrap();
    let Some(split) = ids.windows(2).position(|pair| pair == b"..") else {
        return Err(malformed(line, "invalid index identities"));
    };
    let (old, rest) = ids.split_at(split);
    let new = &rest[2..];
    if old.is_empty()
        || new.is_empty()
        || old.len() > 64
        || new.len() > 64
        || !old.iter().chain(new).all(u8::is_ascii_hexdigit)
    {
        return Err(malformed(line, "invalid index identities"));
    }
    if let Some(mode) = fields.next()
        && !matches!(mode, b"100644" | b"100755" | b"120000")
    {
        return Err(Error::Unsupported {
            line,
            feature: "index mode",
        });
    }
    if fields.next().is_some() {
        return Err(malformed(line, "trailing index fields"));
    }
    Ok(())
}

fn mode_value(bytes: &[u8], line: usize) -> Result<crate::EntryMode, Error> {
    match bytes {
        b"100644" => Ok(crate::EntryMode::Blob),
        b"100755" => Ok(crate::EntryMode::Executable),
        b"120000" => Ok(crate::EntryMode::Symlink),
        _ => Err(Error::Unsupported {
            line,
            feature: "file mode",
        }),
    }
}

fn set_mode(
    slot: &mut Option<crate::EntryMode>,
    value: crate::EntryMode,
    line: usize,
) -> Result<(), Error> {
    if slot.is_some() {
        return Err(malformed(line, "duplicate mode"));
    }
    *slot = Some(value);
    Ok(())
}

fn operation_header(line: &[u8]) -> Option<(super::FileOperation, usize, &[u8])> {
    [
        (b"rename from ".as_slice(), super::FileOperation::Rename, 0),
        (b"rename to ".as_slice(), super::FileOperation::Rename, 1),
        (b"copy from ".as_slice(), super::FileOperation::Copy, 0),
        (b"copy to ".as_slice(), super::FileOperation::Copy, 1),
    ]
    .into_iter()
    .find_map(|(prefix, operation, side)| {
        line.strip_prefix(prefix)
            .map(|bytes| (operation, side, bytes))
    })
}

fn git_paths(bytes: &[u8], line: usize) -> Result<(Vec<u8>, Vec<u8>), Error> {
    if bytes.starts_with(b"\"") {
        let mut escaped = false;
        let end = bytes
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(at, byte)| {
                if escaped {
                    escaped = false;
                    None
                } else if *byte == b'\\' {
                    escaped = true;
                    None
                } else if *byte == b'"' {
                    Some(at + 1)
                } else {
                    None
                }
            })
            .ok_or_else(|| malformed(line, "unterminated Git path"))?;
        let old = quoted_path(&bytes[..end], line)?;
        let rest = bytes
            .get(end + 1..)
            .filter(|_| bytes.get(end) == Some(&b' '))
            .ok_or_else(|| malformed(line, "missing Git destination"))?;
        let new = if rest.starts_with(b"\"") {
            quoted_path(rest, line)?
        } else {
            rest.to_vec()
        };
        return Ok((old, new));
    }
    for at in bytes
        .windows(3)
        .enumerate()
        .filter_map(|(at, bytes)| (bytes == b" b/").then_some(at))
    {
        let old = &bytes[..at];
        let new = &bytes[at + 1..];
        if old.strip_prefix(b"a/") == new.strip_prefix(b"b/") {
            return Ok((old.to_vec(), new.to_vec()));
        }
    }
    Err(malformed(
        line,
        "ambiguous Git paths without authoritative file headers",
    ))
}
