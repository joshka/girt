use std::sync::atomic::AtomicBool;

use super::headers::HeaderBudget;
use super::{Error, Limits, Message, Options, Subject, check, headers, mime};

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
/// Returns [`Error::Unsupported`] for encapsulated/encoded multipart containers or unsupported
/// character sets/transfer encodings,
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
    let (outer, body) = headers::read(input, &mut budget, false, cancel)?;
    let body = mime::decode(&outer, body, &mut budget, limits, cancel)?;
    let decoded_body_bytes = body.bytes.len();
    let start = if options.scissors {
        after_scissors(&body, cancel)?
    } else {
        0
    };
    let start = trim_blank_start(&body, start);
    let (inner, remaining) = headers::read_lines(
        &body.bytes[start..],
        body.lines(start).map(|(_, line)| line),
        &mut budget,
        true,
        cancel,
    )?;
    let start = trim_blank_start(&body, body.bytes.len() - remaining.len());
    let patch_start = body
        .lines(start)
        .try_fold(None, |found, (offset, line)| {
            check(cancel)?;
            Ok::<_, Error>(found.or_else(|| patch_line(line).then_some(offset)))
        })?
        .unwrap_or(body.bytes.len());
    let (log, patch) = (&body.bytes[start..patch_start], &body.bytes[patch_start..]);
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

fn trim_blank_start(body: &mime::Body, mut start: usize) -> usize {
    for (offset, line) in body.lines(start) {
        let Some(line) = line.strip_suffix(b"\n") else {
            break;
        };
        if !line.trim_ascii().is_empty() {
            break;
        }
        start = offset + line.len() + 1;
    }
    start
}
fn patch_line(line: &[u8]) -> bool {
    line.starts_with(b"diff -")
        || line.starts_with(b"Index: ")
        || line
            .strip_prefix(b"---")
            .is_some_and(|rest| rest.is_empty() || rest[0].is_ascii_whitespace())
}
fn after_scissors(body: &mime::Body, cancel: &AtomicBool) -> Result<usize, Error> {
    let mut start = 0;
    for (offset, line) in body.lines(0) {
        check(cancel)?;
        if patch_line(line) {
            break;
        }
        if scissors_line(line) {
            start = offset + line.len();
        }
    }
    Ok(start)
}
// Original CLI observations: a dash or cut marker starts a perforation run. Whitespace
// extends that run and counts as a gap; plain text ends it. In particular, trailing blanks
// can invalidate a cut while leading blanks do not. The visible span excludes edge blanks.
fn scissors_line(line: &[u8]) -> bool {
    let whitespace = |byte: &u8| b" \t\r\n".contains(byte);
    let visible = match (
        line.iter().position(|byte| !whitespace(byte)),
        line.iter().rposition(|byte| !whitespace(byte)),
    ) {
        (Some(first), Some(last)) => last - first + 1,
        _ => 0,
    };
    let mut perforation = 0usize;
    let mut gap = 0usize;
    let mut scissors = false;
    let mut run = false;
    let mut offset = 0;
    while offset < line.len() {
        if [b">8", b"8<", b">%", b"%<"]
            .iter()
            .any(|marker| line[offset..].starts_with(*marker))
        {
            scissors = true;
            run = true;
            perforation += 2;
            offset += 2;
            continue;
        }
        match line[offset] {
            b'-' => {
                perforation += 1;
                run = true;
            }
            byte if run && whitespace(&byte) => {
                perforation += 1;
                gap += 1;
            }
            _ => run = false,
        }
        offset += 1;
    }
    scissors
        && visible >= 8
        && visible < perforation.saturating_mul(3)
        && gap.saturating_mul(2) < perforation
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
