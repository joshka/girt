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
    let mut git_header = false;
    let mut index_seen = false;
    while let Some(line) = parser.next() {
        refuse(line, parser.line)?;
        if line.starts_with(b"diff --git ") {
            if git_header || trim_lf(&line[11..]).is_empty() {
                return Err(malformed(parser.line, "incomplete Git file header"));
            }
            git_header = true;
            continue;
        }
        if line.starts_with(b"index ") {
            if !git_header || index_seen {
                return Err(malformed(parser.line, "unexpected index header"));
            }
            validate_index(trim_lf(&line[6..]), parser.line)?;
            index_seen = true;
            continue;
        }
        if !line.starts_with(b"--- ") {
            return Err(malformed(parser.line, "expected file header"));
        }
        if files.len() >= limits.max_files {
            return Err(Error::Limit("files"));
        }

        let old_path = path(&line[4..], parser.line)?;
        let next = parser
            .next()
            .ok_or_else(|| malformed(parser.line, "missing new path"))?;
        if !next.starts_with(b"+++ ") {
            return Err(malformed(parser.line, "expected new path"));
        }
        let new_path = path(&next[4..], parser.line)?;
        if old_path.is_none() && new_path.is_none() {
            return Err(malformed(parser.line, "both paths absent"));
        }

        let mut hunks = Vec::new();
        while parser
            .lines
            .peek()
            .is_some_and(|line| line.starts_with(b"@@ "))
        {
            if total_hunks >= limits.max_hunks {
                return Err(Error::Limit("hunks"));
            }
            hunks.push(parser.hunk()?);
            total_hunks += 1;
        }
        if let Some(next) = parser.lines.peek() {
            refuse(next, parser.line + 1)?;
        }
        if hunks.is_empty() {
            return Err(malformed(parser.line, "missing hunks"));
        }
        git_header = false;
        index_seen = false;
        files.push(FilePatch {
            old_path,
            new_path,
            hunks,
        });
    }
    if git_header {
        return Err(malformed(parser.line, "incomplete Git file header"));
    }
    if files.is_empty() {
        return Err(malformed(1, "no file edits"));
    }
    Ok(Patch { files })
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
    } else if [
        b"old mode ".as_slice(),
        b"new mode ",
        b"new file mode ",
        b"deleted file mode ",
        b"rename ",
        b"copy ",
        b"similarity index ",
        b"dissimilarity index ",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
    {
        "extended file operation"
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
