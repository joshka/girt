//! The `ident` attribute: `$Id$` keywords.

use std::borrow::Cow;

use crate::ObjectId;

/// Each `$Id$`, or `$Id:` followed by text and `$` on the same line, as a byte range.
fn keywords(bytes: &[u8]) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        while let Some(found) = find(bytes.get(at..)?, b"$Id") {
            let start = at + found;
            let after = start + 3;
            match bytes.get(after) {
                Some(b'$') => {
                    at = after + 1;
                    return Some(start..at);
                }
                Some(b':') => {
                    let line = bytes.get(after + 1..).unwrap_or_default();
                    let close = line.iter().position(|&byte| byte == b'$' || byte == b'\n');
                    match close {
                        Some(close) if line.get(close) == Some(&b'$') => {
                            at = after + 1 + close + 1;
                            return Some(start..at);
                        }
                        _ => at = after,
                    }
                }
                _ => at = after,
            }
        }
        None
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Whether an expanded keyword's text is someone else's: after one leading and one trailing
/// space, it still contains a space. Git writes those unchanged on checkout.
fn foreign(keyword: &[u8]) -> bool {
    let text = keyword
        .get(4..keyword.len().saturating_sub(1))
        .unwrap_or_default();
    let text = text.strip_prefix(b" ").unwrap_or(text);
    let text = text.strip_suffix(b" ").unwrap_or(text);
    text.contains(&b' ')
}

fn replace<'a>(bytes: Cow<'a, [u8]>, with: &[u8], collapsing: bool) -> Cow<'a, [u8]> {
    let ranges: Vec<_> = keywords(&bytes)
        .filter(|range| {
            let keyword = bytes.get(range.clone()).unwrap_or_default();
            if collapsing {
                range.len() != 4
            } else {
                !foreign(keyword)
            }
        })
        .collect();
    if ranges.is_empty() {
        return bytes;
    }
    let mut out = Vec::with_capacity(bytes.len() + ranges.len() * with.len());
    let mut start = 0;
    for range in ranges {
        out.extend_from_slice(bytes.get(start..range.start).unwrap_or_default());
        out.extend_from_slice(with);
        start = range.end;
    }
    out.extend_from_slice(bytes.get(start..).unwrap_or_default());
    Cow::Owned(out)
}

/// Collapses every keyword to `$Id$`, as Git stores it.
pub(super) fn collapse(bytes: Cow<'_, [u8]>) -> Cow<'_, [u8]> {
    replace(bytes, b"$Id$", true)
}

/// Expands every keyword to `$Id: <id> $`, except foreign ones.
pub(super) fn expand(bytes: Cow<'_, [u8]>, id: ObjectId) -> Cow<'_, [u8]> {
    let with = format!("$Id: {id} $");
    replace(bytes, with.as_bytes(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_matches_git() {
        // Observed with `git add` under `ident`.
        let input = b"$Id$ x $Id: abc $ y $Id:$ z $Id: no\nclose $ w $Id:foo$ $ID$ $Id";
        let expected = b"$Id$ x $Id$ y $Id$ z $Id: no\nclose $ w $Id$ $ID$ $Id";
        assert_eq!(&*collapse(Cow::Borrowed(input)), expected);
    }

    #[test]
    fn expand_keeps_foreign_keywords() {
        // Observed with `git checkout` under `ident`.
        let id: ObjectId = "deb0e62babfba8c6f140ad6cdf6684e95f4b949e".parse().unwrap();
        for kept in [
            &b"$Id: foreign words $"[..],
            b"$Id: x y$",
            b"$Id:  x $",
            b"$Id: x  $",
        ] {
            assert_eq!(&*expand(Cow::Borrowed(kept), id), kept);
        }
        for replaced in [&b"$Id:  $"[..], b"$Id: a:b $", b"$Id:x $", b"$Id: x$"] {
            assert_ne!(&*expand(Cow::Borrowed(replaced), id), replaced);
        }
        // Collapsing ignores the rule.
        assert_eq!(&*collapse(Cow::Borrowed(b"$Id: foreign words $")), b"$Id$");
    }

    #[test]
    fn expand_replaces_closed_keywords() {
        let id: ObjectId = "deb0e62babfba8c6f140ad6cdf6684e95f4b949e".parse().unwrap();
        let out = expand(Cow::Borrowed(b"A $Id: old $ B $Id:x\nC $Id$\n"), id);
        assert_eq!(
            &*out,
            b"A $Id: deb0e62babfba8c6f140ad6cdf6684e95f4b949e $ B $Id:x\nC \
              $Id: deb0e62babfba8c6f140ad6cdf6684e95f4b949e $\n"
        );
    }
}
