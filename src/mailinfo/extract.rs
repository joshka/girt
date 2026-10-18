use std::sync::atomic::{AtomicBool, Ordering};

use super::{Error, Limits, Message, Options, Subject, encoding};

/// Extracts one message with default policy and bounds; wraps [`parse_with`].
///
/// # Errors
///
/// Returns malformed/unsupported metadata or MIME errors, or a named limit failure.
pub fn parse(input: &[u8]) -> Result<Message, Error> {
    parse_with(
        input,
        Options::default(),
        Limits::default(),
        &AtomicBool::new(false),
    )
}

/// Extracts one message with explicit policy, bounds and cancellation.
///
/// In-body From/Subject/Date fields override outer metadata when a patch is found. Scissors is
/// applied before recognizing those fields again. A patch starts at `diff -`, `Index: `, or a
/// three-dash separator followed by whitespace/end of line. Patch bytes are not parsed here.
/// Cancellation is checked per header/body line and encoded word; synchronous copies are bounded
/// by admitted input. Returned strings are UTF-8, while body and patch remain bytes. Processing is
/// read-only and keeps no repository or environment state.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] for multipart/non-text MIME and unsupported character sets,
/// [`Error::Malformed`] for invalid headers or encodings, [`Error::Limit`] for exceeded bounds,
/// or [`Error::Cancelled`]. Every failure returns no partial result.
pub fn parse_with(
    input: &[u8],
    options: Options,
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Message, Error> {
    check(cancel)?;
    if input.len() > limits.max_input_bytes {
        return Err(Error::Limit("input bytes"));
    }
    let normalized = normalize(input, cancel)?;
    let input = if normalized.starts_with(b"From ") {
        crate::bytes::split_once(&normalized, b'\n').map_or(&b""[..], |(_, rest)| rest)
    } else {
        &normalized
    };
    let mut budget = HeaderBudget {
        bytes: limits.max_header_bytes,
        fields: limits.max_headers,
    };
    let (outer, body) = headers(input, &mut budget, false, cancel)?;
    let body = decode_body(&outer, body, limits.max_decoded_bytes, cancel)?;
    let decoded_body_bytes = body.len();
    let body = if options.scissors {
        after_scissors(&body, cancel)?
    } else {
        &body
    };
    let body = trim_blank_start(body);
    let (inner, body) = headers(body, &mut budget, true, cancel)?;
    let body = trim_blank_start(body);
    let patch_start = body
        .split_inclusive(|&byte| byte == b'\n')
        .try_fold((0, None), |(offset, found), line| {
            check(cancel)?;
            Ok::<_, Error>((
                offset + line.len(),
                found.or_else(|| patch_line(line).then_some(offset)),
            ))
        })?
        .1
        .unwrap_or(body.len());
    let (log, patch) = body.split_at(patch_start);
    if patch.len() > limits.max_patch_bytes {
        return Err(Error::Limit("patch bytes"));
    }
    let mut decoded = limits.max_decoded_bytes.saturating_sub(decoded_body_bytes);
    let message_id = outer.decode(b"message-id", &mut decoded, cancel)?;
    let fields = if patch.is_empty() {
        outer
    } else {
        outer.overridden(inner)
    };
    if fields.get(b"from").is_some_and(multiple_addresses) {
        return Err(Error::Unsupported("multiple From addresses"));
    }
    let from = fields.decode(b"from", &mut decoded, cancel)?;
    let (author, email) = from.map(|value| sender(&value)).transpose()?.unzip();
    let subject = fields
        .decode(b"subject", &mut decoded, cancel)?
        .map(|value| clean_subject(&value, options.subject));
    let date = fields.decode(b"date", &mut decoded, cancel)?;
    let mut message = Vec::new();
    append(&mut message, log, limits.max_message_bytes, "message bytes")?;
    if options.message_id
        && !patch.is_empty()
        && let Some(id) = &message_id
    {
        append(
            &mut message,
            b"Message-ID: ",
            limits.max_message_bytes,
            "message bytes",
        )?;
        append(
            &mut message,
            id.as_bytes(),
            limits.max_message_bytes,
            "message bytes",
        )?;
        append(
            &mut message,
            b"\n",
            limits.max_message_bytes,
            "message bytes",
        )?;
    }
    check(cancel)?;
    Ok(Message {
        author,
        email,
        subject,
        date,
        message_id,
        message,
        patch: patch.to_vec(),
    })
}

pub(super) fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
fn append(out: &mut Vec<u8>, bytes: &[u8], limit: usize, name: &'static str) -> Result<(), Error> {
    if bytes.len() > limit.saturating_sub(out.len()) {
        return Err(Error::Limit(name));
    }
    out.extend_from_slice(bytes);
    Ok(())
}
fn normalize(input: &[u8], cancel: &AtomicBool) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(input.len());
    for line in input.split_inclusive(|&byte| byte == b'\n') {
        check(cancel)?;
        if let Some(line) = line.strip_suffix(b"\r\n") {
            out.extend_from_slice(line);
            out.push(b'\n');
        } else {
            out.extend_from_slice(line);
        }
    }
    Ok(out)
}

#[derive(Clone, Default)]
struct Headers(Vec<(Vec<u8>, Vec<u8>)>);
struct HeaderBudget {
    bytes: usize,
    fields: usize,
}
impl Headers {
    fn get(&self, name: &[u8]) -> Option<&[u8]> {
        self.0
            .iter()
            .rev()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_slice())
    }
    fn decode(
        &self,
        name: &[u8],
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<Option<String>, Error> {
        self.get(name)
            .map(|value| {
                let text = encoding::header(value, cancel)?;
                if text.len() > *budget {
                    return Err(Error::Limit("decoded bytes"));
                }
                *budget -= text.len();
                Ok(text)
            })
            .transpose()
    }
    fn overridden(mut self, inner: Self) -> Self {
        for name in [b"from".as_slice(), b"subject", b"date"] {
            if let Some(value) = inner.get(name) {
                self.0.retain(|(key, _)| !key.eq_ignore_ascii_case(name));
                self.0.push((name.to_vec(), value.to_vec()));
            }
        }
        self
    }
}
fn headers<'a>(
    input: &'a [u8],
    budget: &mut HeaderBudget,
    in_body: bool,
    cancel: &AtomicBool,
) -> Result<(Headers, &'a [u8]), Error> {
    let mut result = Headers::default();
    let mut offset = 0;
    for line in input.split_inclusive(|&byte| byte == b'\n') {
        check(cancel)?;
        let value = line.strip_suffix(b"\n").unwrap_or(line);
        if value.is_empty() {
            offset += line.len();
            break;
        }
        let continuation = value.starts_with(b" ") || value.starts_with(b"\t");
        let field = crate::bytes::split_once(value, b':');
        if !continuation && field.is_none() {
            break;
        }
        if in_body
            && !continuation
            && !field.is_some_and(|(key, _)| {
                [b"from".as_slice(), b"subject", b"date"]
                    .iter()
                    .any(|name| key.eq_ignore_ascii_case(name))
            })
        {
            break;
        }
        if line.len() > budget.bytes {
            return Err(Error::Limit("header bytes"));
        }
        budget.bytes -= line.len();
        if value.contains(&0) {
            return Err(Error::Malformed("header NUL"));
        }
        if continuation {
            let (_, previous) = result
                .0
                .last_mut()
                .ok_or(Error::Malformed("header continuation"))?;
            previous.push(b' ');
            previous.extend_from_slice(value.trim_ascii());
        } else if let Some((name, value)) = field {
            if budget.fields == 0 {
                return Err(Error::Limit("headers"));
            }
            budget.fields -= 1;
            let mut value = value.trim_ascii().to_vec();
            if in_body && !name.eq_ignore_ascii_case(b"subject") {
                value.push(b' ');
            }
            result.0.push((name.to_ascii_lowercase(), value));
        }
        offset += line.len();
    }
    Ok((result, &input[offset..]))
}

fn decode_body(
    headers: &Headers,
    body: &[u8],
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    if let Some(content_type) = headers.get(b"content-type") {
        let text =
            std::str::from_utf8(content_type).map_err(|_| Error::Malformed("content type"))?;
        let mut parts = text.split(';');
        if !parts
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("text/plain")
        {
            return Err(Error::Unsupported("MIME content type"));
        }
        for part in parts {
            if let Some((key, value)) = part.split_once('=')
                && key.trim().eq_ignore_ascii_case("charset")
            {
                let charset = value.trim().trim_matches('"');
                if !["utf-8", "utf8", "us-ascii", "ascii"]
                    .iter()
                    .any(|value| charset.eq_ignore_ascii_case(value))
                {
                    return Err(Error::Unsupported("body character set"));
                }
            }
        }
    }
    let transfer = headers.get(b"content-transfer-encoding").unwrap_or(b"8bit");
    let out = if transfer.eq_ignore_ascii_case(b"base64") {
        encoding::base64(body)?
    } else if transfer.eq_ignore_ascii_case(b"quoted-printable") {
        encoding::quoted(body, false, cancel)?
    } else if [b"8bit".as_slice(), b"7bit", b"binary"]
        .iter()
        .any(|value| transfer.eq_ignore_ascii_case(value))
    {
        body.to_vec()
    } else {
        return Err(Error::Unsupported("transfer encoding"));
    };
    if out.len() > limit {
        return Err(Error::Limit("decoded bytes"));
    }
    Ok(out)
}
fn trim_blank_start(mut body: &[u8]) -> &[u8] {
    while let Some((line, rest)) = crate::bytes::split_once(body, b'\n') {
        if !line.trim_ascii().is_empty() {
            break;
        }
        body = rest;
    }
    body
}
fn patch_line(line: &[u8]) -> bool {
    line.starts_with(b"diff -")
        || line.starts_with(b"Index: ")
        || line
            .strip_prefix(b"---")
            .is_some_and(|rest| rest.is_empty() || rest[0].is_ascii_whitespace())
}
fn after_scissors<'a>(body: &'a [u8], cancel: &AtomicBool) -> Result<&'a [u8], Error> {
    let mut start = 0;
    let mut offset = 0;
    for line in body.split_inclusive(|&byte| byte == b'\n') {
        check(cancel)?;
        if patch_line(line) {
            break;
        }
        offset += line.len();
        if scissors_line(line)? {
            start = offset;
        }
    }
    Ok(&body[start..])
}
// Admit the standard dashed cut line explicitly. Other marker-bearing forms are refused
// until the broader mailinfo heuristic has independent compatibility coverage.
fn scissors_line(line: &[u8]) -> Result<bool, Error> {
    let markers = [b">8", b"8<", b">%", b"%<"];
    let Some(index) = line
        .windows(2)
        .position(|pair| markers.iter().any(|marker| pair == *marker))
    else {
        return Ok(false);
    };
    let mut clean = line.strip_suffix(b"\n").unwrap_or(line);
    clean = clean.strip_suffix(b"\r").unwrap_or(clean);
    clean = clean.trim_ascii_start();
    if let Some(rest) = clean.strip_prefix(b"#") {
        clean = rest.trim_ascii_start();
    }
    let marker = &line[index..index + 2];
    let index = clean
        .windows(2)
        .position(|pair| pair == marker)
        .ok_or(Error::Unsupported("scissors line"))?;
    let (left, right) = (&clean[..index], &clean[index + 2..]);
    let left = left.strip_suffix(b" ").unwrap_or(left);
    let right = right.strip_prefix(b" ").unwrap_or(right);
    if clean.len() >= 8
        && left.len() >= 2
        && right.len() >= 2
        && left.iter().chain(right).all(|&byte| byte == b'-')
    {
        Ok(true)
    } else {
        Err(Error::Unsupported("scissors line"))
    }
}

fn multiple_addresses(value: &[u8]) -> bool {
    let mut quoted = false;
    let mut escaped = false;
    let mut count = 0;
    for &byte in value {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if quoted => escaped = true,
            b'"' => quoted = !quoted,
            b'<' if !quoted => count += 1,
            _ => {}
        }
    }
    count > 1
}

fn sender(value: &str) -> Result<(String, String), Error> {
    let value = value.trim();
    if let Some((name, rest)) = value.rsplit_once('<')
        && let Some((email, _)) = rest.split_once('>')
    {
        let name = name
            .trim()
            .trim_matches('"')
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
        return Ok((
            if name.is_empty() || name.contains(['<', '>']) {
                email.to_owned()
            } else {
                name
            },
            email.to_owned(),
        ));
    }
    if let Some((email, name)) = value.split_once(" (") {
        return Ok((name.trim_end_matches(')').to_owned(), email.to_owned()));
    }
    if value.contains('@') {
        return Ok((value.to_owned(), value.to_owned()));
    }
    Err(Error::Malformed("From address"))
}
fn clean_subject(value: &str, policy: Subject) -> String {
    if policy == Subject::Keep {
        return value.to_owned();
    }
    let mut rest = value.trim();
    let mut kept = Vec::new();
    loop {
        if rest
            .get(..3)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("re:"))
        {
            rest = rest[3..].trim_start();
            continue;
        }
        if let Some(bracket) = rest.strip_prefix('[')
            && let Some((label, tail)) = bracket.split_once(']')
        {
            if policy == Subject::KeepNonPatch && !label.contains("PATCH") {
                kept.push(format!("[{label}]"));
            }
            rest = tail.trim_start();
            continue;
        }
        break;
    }
    kept.push(rest.to_owned());
    kept.join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
