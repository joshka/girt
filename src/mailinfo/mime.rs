//! Iterative MIME entity extraction with shared header and decoded-output budgets.
use std::sync::atomic::AtomicBool;

use super::headers::{self, HeaderBudget, Headers};
use super::{Error, Limits, check, encoding};

struct Boundary {
    bytes: Vec<u8>,
    first: bool,
}

// Separate logical chunks preserve part boundaries without inventing bytes. A decoded part
// need not end in LF, but the next part still starts a fresh line for patch/header recognition.
#[derive(Default)]
pub(super) struct Body {
    pub(super) bytes: Vec<u8>,
    ends: Vec<usize>,
}
impl Body {
    pub(super) fn lines(&self, start: usize) -> impl Iterator<Item = (usize, &[u8])> {
        self.ends
            .iter()
            .scan(0, move |previous, &end| {
                let base = (*previous).max(start);
                *previous = end;
                Some((base, end))
            })
            .filter(|(base, end)| base < end)
            .flat_map(move |(base, end)| {
                let mut offset = base;
                self.bytes[base..end]
                    .split_inclusive(|&byte| byte == b'\n')
                    .map(move |line| {
                        let start = offset;
                        offset += line.len();
                        (start, line)
                    })
            })
    }
    fn push(&mut self, bytes: &[u8], limit: usize) -> Result<(), Error> {
        if bytes.len() > limit.saturating_sub(self.bytes.len()) {
            return Err(Error::Limit("decoded bytes"));
        }
        self.bytes.extend_from_slice(bytes);
        self.ends.push(self.bytes.len());
        Ok(())
    }
}

pub(super) fn decode(
    outer: &Headers,
    mut input: &[u8],
    header_budget: &mut HeaderBudget,
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Body, Error> {
    if limits.max_mime_parts == 0 {
        return Err(Error::Limit("MIME parts"));
    }
    let mut admitted = 1usize;
    let mut boundaries: Vec<Boundary> = Vec::new();
    let mut next = Some(outer.clone());
    let mut output = Body::default();
    loop {
        check(cancel)?;
        let (scan, leaf) = if let Some(headers) = next.take() {
            let content = ContentType::parse(headers.get(b"content-type"))?;
            content.check_charset()?;
            if content.kind == b"message/rfc822" {
                return Err(Error::Unsupported("encapsulated message MIME"));
            }
            if content.kind.starts_with(b"multipart/") {
                if boundaries.len() >= limits.max_mime_depth {
                    return Err(Error::Limit("MIME depth"));
                }
                if let Some(transfer) = headers.get(b"content-transfer-encoding")
                    && ![b"8bit".as_slice(), b"7bit", b"binary"]
                        .iter()
                        .any(|value| transfer.eq_ignore_ascii_case(value))
                {
                    return Err(Error::Unsupported("encoded multipart container"));
                }
                let bytes = content
                    .boundary
                    .filter(|value| !value.is_empty())
                    .ok_or(Error::Malformed("MIME boundary"))?;
                boundaries.push(Boundary { bytes, first: true });
                continue;
            }
            let scan = if let Some(boundary) = boundaries.last() {
                scan(input, &boundary.bytes, true, cancel)?
            } else {
                Scan {
                    data: input,
                    rest: b"",
                    delimiter: None,
                }
            };
            let remaining = limits.max_decoded_bytes.saturating_sub(output.bytes.len());
            let bytes = transfer(&headers, scan.data, remaining, cancel)?;
            output.push(&bytes, limits.max_decoded_bytes)?;
            (scan, true)
        } else {
            let Some(boundary) = boundaries.last() else {
                break;
            };
            let scan = scan(input, &boundary.bytes, !boundary.first, cancel)?;
            // Only the outer preamble is discarded. Nested preambles are body content; after
            // a child closes, epilogues are skipped while seeking the enclosing boundary.
            if boundary.first && boundaries.len() > 1 {
                output.push(scan.data, limits.max_decoded_bytes)?;
            }
            (scan, false)
        };
        input = scan.rest;
        match scan.delimiter {
            None => break,
            Some(true) => {
                if leaf {
                    output.push(b"\n", limits.max_decoded_bytes)?;
                }
                boundaries.pop();
            }
            Some(false) => {
                if let Some(boundary) = boundaries.last_mut() {
                    boundary.first = false;
                }
                if admitted == limits.max_mime_parts {
                    return Err(Error::Limit("MIME parts"));
                }
                admitted += 1;
                let (headers, body) = headers::read(input, header_budget, false, cancel)?;
                input = body;
                next = Some(headers);
            }
        }
    }
    Ok(output)
}

struct Scan<'a> {
    data: &'a [u8],
    rest: &'a [u8],
    delimiter: Option<bool>,
}
// Only the innermost active delimiter is recognized. An unclosed child therefore retains
// enclosing delimiter-looking lines as body text, as the CLI does, rather than losing bytes.
fn scan<'a>(
    input: &'a [u8],
    boundary: &[u8],
    closing: bool,
    cancel: &AtomicBool,
) -> Result<Scan<'a>, Error> {
    let mut offset = 0;
    for line in input.split_inclusive(|&byte| byte == b'\n') {
        check(cancel)?;
        if let Some(close) = delimiter(line, boundary)
            && (closing || !close)
        {
            return Ok(Scan {
                data: &input[..offset],
                rest: &input[offset + line.len()..],
                delimiter: Some(close),
            });
        }
        offset += line.len();
    }
    Ok(Scan {
        data: input,
        rest: b"",
        delimiter: None,
    })
}

fn transfer(
    headers: &Headers,
    body: &[u8],
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    let transfer = headers.get(b"content-transfer-encoding").unwrap_or(b"8bit");
    let out = if transfer.eq_ignore_ascii_case(b"base64") {
        encoding::base64(body)?
    } else if transfer.eq_ignore_ascii_case(b"quoted-printable") {
        encoding::quoted(body, false, cancel)?
    } else if [b"8bit".as_slice(), b"7bit", b"binary"]
        .iter()
        .any(|value| transfer.eq_ignore_ascii_case(value))
    {
        if body.len() > limit {
            return Err(Error::Limit("decoded bytes"));
        }
        body.to_vec()
    } else {
        return Err(Error::Unsupported("transfer encoding"));
    };
    if out.len() > limit {
        return Err(Error::Limit("decoded bytes"));
    }
    Ok(out)
}

fn delimiter(line: &[u8], boundary: &[u8]) -> Option<bool> {
    let rest = line.strip_prefix(b"--")?.strip_prefix(boundary)?;
    Some(rest.starts_with(b"--"))
}

#[derive(Default)]
struct ContentType {
    kind: Vec<u8>,
    boundary: Option<Vec<u8>>,
    charset: Option<Vec<u8>>,
}
impl ContentType {
    fn parse(value: Option<&[u8]>) -> Result<Self, Error> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let mut result = Self::default();
        let mut start = 0;
        let mut quoted = false;
        let mut escaped = false;
        let mut first = true;
        for offset in 0..=value.len() {
            let byte = value.get(offset).copied();
            if byte.is_none() || byte == Some(b';') && !quoted {
                let part = value[start..offset].trim_ascii();
                if first {
                    result.kind = part.to_ascii_lowercase();
                    first = false;
                } else if let Some((name, value)) = crate::bytes::split_once(part, b'=') {
                    let value = parameter(value.trim_ascii())?;
                    if name.trim_ascii().eq_ignore_ascii_case(b"boundary") {
                        result.boundary = Some(value);
                    } else if name.trim_ascii().eq_ignore_ascii_case(b"charset") {
                        result.charset = Some(value);
                    }
                }
                start = offset + 1;
            }
            if escaped {
                escaped = false;
            } else if quoted && byte == Some(b'\\') {
                escaped = true;
            } else if byte == Some(b'"') {
                quoted = !quoted;
            }
        }
        if quoted {
            return Err(Error::Malformed("MIME parameter quote"));
        }
        Ok(result)
    }
    fn check_charset(&self) -> Result<(), Error> {
        if let Some(charset) = &self.charset
            && ![b"utf-8".as_slice(), b"utf8", b"us-ascii", b"ascii"]
                .iter()
                .any(|value| charset.eq_ignore_ascii_case(value))
        {
            return Err(Error::Unsupported("body character set"));
        }
        Ok(())
    }
}
fn parameter(value: &[u8]) -> Result<Vec<u8>, Error> {
    let Some(value) = value.strip_prefix(b"\"") else {
        return Ok(value.to_vec());
    };
    // The CLI terminates a quoted parameter at the next quote, without unescaping it.
    let end = value
        .iter()
        .position(|&byte| byte == b'"')
        .ok_or(Error::Malformed("MIME parameter quote"))?;
    Ok(value[..end].to_vec())
}
