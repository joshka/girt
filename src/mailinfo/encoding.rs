//! Original RFC 2047 and single-part transfer decoding; no MIME tree traversal.
use std::sync::atomic::AtomicBool;

use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use super::Error;
use super::extract::check;

pub(super) fn base64(input: &[u8]) -> Result<Vec<u8>, Error> {
    let input: Vec<_> = input
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let config = GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true);
    GeneralPurpose::new(&base64::alphabet::STANDARD, config)
        .decode(input)
        .map_err(|_| Error::Malformed("base64"))
}
pub(super) fn quoted(input: &[u8], word: bool, cancel: &AtomicBool) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < input.len() {
        if offset % 4096 == 0 {
            check(cancel)?;
        }
        match input[offset] {
            b'_' if word => out.push(b' '),
            b'=' => {
                if !word && input.get(offset + 1) == Some(&b'\n') {
                    offset += 2;
                    continue;
                }
                if let Some(bytes) = input.get(offset + 1..offset + 3)
                    && let (Some(high), Some(low)) = (
                        (bytes[0] as char).to_digit(16),
                        (bytes[1] as char).to_digit(16),
                    )
                {
                    out.push((high * 16 + low) as u8);
                    offset += 2;
                } else {
                    out.push(b'=');
                }
            }
            byte => out.push(byte),
        }
        offset += 1;
    }
    Ok(out)
}
fn charset(bytes: &[u8], name: &[u8]) -> Result<String, Error> {
    if [b"us-ascii".as_slice(), b"ascii"]
        .iter()
        .any(|value| name.eq_ignore_ascii_case(value))
        && !bytes.is_ascii()
    {
        return Err(Error::Malformed("ASCII metadata"));
    }
    if [b"utf-8".as_slice(), b"utf8", b"us-ascii", b"ascii"]
        .iter()
        .any(|value| name.eq_ignore_ascii_case(value))
    {
        return String::from_utf8(bytes.to_vec()).map_err(|_| Error::Malformed("metadata UTF-8"));
    }
    if [b"iso-8859-1".as_slice(), b"latin1", b"latin-1"]
        .iter()
        .any(|value| name.eq_ignore_ascii_case(value))
    {
        return Ok(bytes.iter().map(|&byte| char::from(byte)).collect());
    }
    Err(Error::Unsupported("header character set"))
}
// Encoded words retain bytes until adjacent words sharing a charset have been joined.
struct Word<'a> {
    charset: &'a [u8],
    kind: &'a [u8],
    content: &'a [u8],
    tail: &'a [u8],
}
fn word(input: &[u8]) -> Result<Word<'_>, Error> {
    let word = input
        .strip_prefix(b"=?")
        .ok_or(Error::Malformed("encoded word"))?;
    let (charset, rest) =
        crate::bytes::split_once(word, b'?').ok_or(Error::Malformed("encoded word"))?;
    let (kind, rest) =
        crate::bytes::split_once(rest, b'?').ok_or(Error::Malformed("encoded word"))?;
    if charset.is_empty() || kind.len() != 1 || charset.iter().any(u8::is_ascii_whitespace) {
        return Err(Error::Malformed("encoded word"));
    }
    let end = rest
        .windows(2)
        .position(|bytes| bytes == b"?=")
        .ok_or(Error::Malformed("encoded word"))?;
    Ok(Word {
        charset,
        kind,
        content: &rest[..end],
        tail: &rest[end + 2..],
    })
}
fn charset_key(name: &[u8]) -> &[u8] {
    if name.eq_ignore_ascii_case(b"utf8") {
        b"utf-8"
    } else if name.eq_ignore_ascii_case(b"ascii") {
        b"us-ascii"
    } else if name.eq_ignore_ascii_case(b"latin1") || name.eq_ignore_ascii_case(b"latin-1") {
        b"iso-8859-1"
    } else {
        name
    }
}

fn flush(out: &mut String, pending: &mut Option<(Vec<u8>, Vec<u8>)>) -> Result<(), Error> {
    if let Some((name, bytes)) = pending.take() {
        out.push_str(&charset(&bytes, &name)?);
    }
    Ok(())
}
pub(super) fn header(input: &[u8], cancel: &AtomicBool) -> Result<String, Error> {
    let mut out = String::new();
    let mut rest = input;
    let mut pending: Option<(Vec<u8>, Vec<u8>)> = None;
    while let Some(start) = rest.windows(2).position(|bytes| bytes == b"=?") {
        check(cancel)?;
        let prefix = &rest[..start];
        let word = word(&rest[start..])?;
        let name = charset_key(word.charset);
        let adjacent = pending.is_some() && prefix.iter().all(u8::is_ascii_whitespace);
        let same = pending
            .as_ref()
            .is_some_and(|(name, _)| name.eq_ignore_ascii_case(charset_key(word.charset)));
        if !adjacent || !same {
            flush(&mut out, &mut pending)?;
            if !adjacent {
                out.push_str(&charset(prefix, b"utf-8")?);
            }
        }
        let bytes = if word.kind.eq_ignore_ascii_case(b"q") {
            quoted(word.content, true, cancel)?
        } else if word.kind.eq_ignore_ascii_case(b"b") {
            base64(word.content)?
        } else {
            return Err(Error::Unsupported("encoded-word transfer encoding"));
        };
        let (_, pending_bytes) = pending.get_or_insert_with(|| (name.to_vec(), Vec::new()));
        pending_bytes.extend_from_slice(&bytes);
        rest = word.tail;
    }
    flush(&mut out, &mut pending)?;
    out.push_str(&charset(rest, b"utf-8")?);
    if out.contains(['\r', '\n', '\0']) {
        return Err(Error::Malformed("decoded header control"));
    }
    Ok(out)
}
