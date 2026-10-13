//! Byte glob matching for include conditions; bounded dynamic programming avoids backtracking.

pub(super) fn matches(pattern: &[u8], text: &[u8], fold: bool, cells: usize) -> Option<bool> {
    matches_with_pathname(pattern, text, fold, cells, true)
}

pub(crate) fn matches_without_pathname(pattern: &[u8], text: &[u8], cells: usize) -> Option<bool> {
    matches_with_pathname(pattern, text, false, cells, false)
}

#[expect(
    clippy::indexing_slicing,
    reason = "rows have text.len() + 1 cells indexed by 0..=text.len(); pattern positions stay \
              below pattern.len(), and class_end returns an in-bounds `]`"
)]
fn matches_with_pathname(
    pattern: &[u8],
    text: &[u8],
    fold: bool,
    cells: usize,
    pathname: bool,
) -> Option<bool> {
    if pattern
        .len()
        .saturating_add(1)
        .saturating_mul(text.len().saturating_add(1))
        > cells
    {
        return None;
    }
    // Case folding applies when comparing bytes, never to the pattern: class names such as
    // `[:upper:]` stay case-sensitive, as in Git.
    let mut previous = vec![false; text.len() + 1];
    previous[0] = true;
    let mut pos = 0;
    while pos < pattern.len() {
        let mut next = vec![false; text.len() + 1];
        match pattern[pos] {
            b'*' => {
                let start = pos;
                while pattern.get(pos) == Some(&b'*') {
                    pos += 1;
                }
                let recursive = pathname
                    && pos - start >= 2
                    && (start == 0 || pattern[start - 1] == b'/')
                    && (pos == pattern.len() || pattern[pos] == b'/');
                if recursive && pattern.get(pos) == Some(&b'/') {
                    let mut reachable = false;
                    for i in 0..=text.len() {
                        next[i] = previous[i] || (i > 0 && text[i - 1] == b'/' && reachable);
                        reachable |= previous[i];
                    }
                    pos += 1;
                } else {
                    next[0] = previous[0];
                    for i in 1..=text.len() {
                        next[i] = previous[i]
                            || (next[i - 1] && (!pathname || recursive || text[i - 1] != b'/'));
                    }
                }
            }
            b'?' => {
                for i in 1..=text.len() {
                    next[i] = previous[i - 1] && (!pathname || text[i - 1] != b'/');
                }
                pos += 1;
            }
            b'[' => {
                let Some(end) = class_end(pattern, pos + 1) else {
                    return Some(false);
                };
                let members = &pattern[pos + 1..end];
                for i in 1..=text.len() {
                    if previous[i - 1] && (!pathname || text[i - 1] != b'/') {
                        // Git rejects the whole pattern when a class names an unknown set.
                        let byte = text[i - 1];
                        let Some(mut matched) = class(members, byte) else {
                            return Some(false);
                        };
                        if fold && !matched {
                            let other = if byte.is_ascii_lowercase() {
                                byte.to_ascii_uppercase()
                            } else {
                                byte.to_ascii_lowercase()
                            };
                            matched = class(members, other).unwrap_or(false);
                        }
                        next[i] = matched;
                    }
                }
                pos = end + 1;
            }
            byte => {
                let literal = if byte == b'\\' {
                    pos += 1;
                    let Some(byte) = pattern.get(pos) else {
                        return Some(false);
                    };
                    *byte
                } else {
                    byte
                };
                for i in 1..=text.len() {
                    next[i] = previous[i - 1]
                        && (text[i - 1] == literal
                            || (fold && text[i - 1].eq_ignore_ascii_case(&literal)));
                }
                pos += 1;
            }
        }
        previous = next;
    }
    Some(previous[text.len()])
}

/// Finds the closing `]`, tokenizing members exactly as [`class`] does.
///
/// A leading `]` is a member, and a range endpoint is a single member byte, so `[` after `-` never
/// opens a named class.
fn class_end(pattern: &[u8], start: usize) -> Option<usize> {
    let body = pattern.get(start..)?;
    let mut rest = body
        .strip_prefix(b"!")
        .or(body.strip_prefix(b"^"))
        .unwrap_or(body);
    let mut first = true;
    loop {
        if !first && rest.first() == Some(&b']') {
            return Some(pattern.len() - rest.len());
        }
        first = false;
        if let Some((_, after)) = named_class(rest) {
            rest = after;
            continue;
        }
        let (_, after) = class_byte(rest)?;
        rest = match after {
            [b'-', range @ ..] if range.first().is_some_and(|byte| *byte != b']') => {
                class_byte(range).map_or(after, |(_, after)| after)
            }
            _ => after,
        };
    }
}

/// Splits a leading `[:name:]` into its name and the bytes after it.
///
/// As Git does, the first `]` closes the name and must follow a `:` other than the opening one;
/// otherwise the `[` is an ordinary member. The name may be empty, as in `[::]`.
fn named_class(pattern: &[u8]) -> Option<(&[u8], &[u8])> {
    let body = pattern.strip_prefix(b"[:")?;
    let (name, after) = crate::bytes::split_once(body, b']')?;
    Some((name.strip_suffix(b":")?, after))
}

/// Tests `byte` against class members, or returns `None` for an unknown named class.
fn class(mut pattern: &[u8], byte: u8) -> Option<bool> {
    let negative = matches!(pattern.first(), Some(b'!' | b'^'));
    if negative {
        pattern = pattern.get(1..).unwrap_or_default();
    }
    let mut found = false;
    while !pattern.is_empty() {
        if let Some((name, after)) = named_class(pattern) {
            found |= match name {
                b"alnum" => byte.is_ascii_alphanumeric(),
                b"alpha" => byte.is_ascii_alphabetic(),
                b"blank" => matches!(byte, b' ' | b'\t'),
                b"cntrl" => byte.is_ascii_control(),
                b"digit" => byte.is_ascii_digit(),
                b"graph" => byte.is_ascii_graphic(),
                b"lower" => byte.is_ascii_lowercase(),
                b"print" => byte.is_ascii_graphic() || byte == b' ',
                b"punct" => byte.is_ascii_punctuation(),
                b"space" => byte.is_ascii_whitespace(),
                b"upper" => byte.is_ascii_uppercase(),
                b"xdigit" => byte.is_ascii_hexdigit(),
                _ => return None,
            };
            pattern = after;
            continue;
        }
        let Some((first, rest)) = class_byte(pattern) else {
            break;
        };
        pattern = rest;
        if pattern.len() >= 2
            && let Some(range) = pattern.strip_prefix(b"-")
            && let Some((last, rest)) = class_byte(range)
        {
            found |= first <= byte && byte <= last;
            pattern = rest;
        } else {
            found |= first == byte;
        }
    }
    Some(found != negative)
}

/// Splits one class member, unescaping a backslash that has a following byte.
fn class_byte(pattern: &[u8]) -> Option<(u8, &[u8])> {
    match pattern {
        [b'\\', escaped, rest @ ..] => Some((*escaped, rest)),
        [byte, rest @ ..] => Some((*byte, rest)),
        [] => None,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    #[rstest]
    #[case::star(b"refs/*", b"refs/heads/nested/topic", true)]
    #[case::question(b"a?b", b"a/b", true)]
    #[case::class(b"a[/]b", b"a/b", true)]
    #[case::double_star_is_not_a_component(b"a/**/b", b"a/b", false)]
    #[case::case_sensitive(b"HEAD", b"head", false)]
    #[case::malformed_class(b"[o[:]", b"[", true)]
    #[case::unknown_named_class(b"[o[:foo:]]", b"o", false)]
    fn reference_glob(#[case] pattern: &[u8], #[case] text: &[u8], #[case] expected: bool) {
        assert_eq!(
            matches_without_pathname(pattern, text, 10000),
            Some(expected)
        );
    }

    #[rstest]
    #[case::single(b"a/*/c", b"a/b/d/c", false)]
    #[case::recursive(b"a/**/c", b"a/b/d/c", true)]
    #[case::zero_components(b"a/**/c", b"a/c", true)]
    #[case::tail(b"a/**", b"a/b/d", true)]
    #[case::class(b"[[:digit:]][!a-z]", b"4Z", true)]
    #[case::escape(b"a\\*", b"a*", true)]
    #[case::question(b"a?b", b"a/b", false)]
    #[case::opening_colon_is_literal(b"[o[:]", b"[", true)]
    #[case::opening_colon_closes_class(b"[o[:]:]]", b"o", false)]
    #[case::fuzzed_colon_bracket(b"[][:].l:]h?[*.h]", b"].l:]hxh", true)]
    #[case::unknown_name_rejects_pattern(b"[o[:foo:]]", b"o", false)]
    #[case::trailing_escape_in_class(b"[a\\", b"a", false)]
    #[case::escaped_close_leaves_class_open(b"[a-\\]", b"a", false)]
    fn glob(#[case] pattern: &[u8], #[case] text: &[u8], #[case] expected: bool) {
        assert_eq!(matches(pattern, text, false, 10000), Some(expected));
    }
}
