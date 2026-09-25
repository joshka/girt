//! Byte glob matching for include conditions; bounded dynamic programming avoids backtracking.

pub(super) fn matches(pattern: &[u8], text: &[u8], fold: bool, cells: usize) -> Option<bool> {
    if pattern
        .len()
        .saturating_add(1)
        .saturating_mul(text.len().saturating_add(1))
        > cells
    {
        return None;
    }
    let pattern = if fold {
        pattern.to_ascii_lowercase()
    } else {
        pattern.to_vec()
    };
    let text = if fold {
        text.to_ascii_lowercase()
    } else {
        text.to_vec()
    };
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
                let recursive = pos - start >= 2
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
                        next[i] =
                            previous[i] || (next[i - 1] && (recursive || text[i - 1] != b'/'));
                    }
                }
            }
            b'?' => {
                for i in 1..=text.len() {
                    next[i] = previous[i - 1] && text[i - 1] != b'/';
                }
                pos += 1;
            }
            b'[' => {
                let Some(end) = class_end(&pattern, pos + 1) else {
                    return Some(false);
                };
                for i in 1..=text.len() {
                    next[i] = previous[i - 1]
                        && text[i - 1] != b'/'
                        && class(&pattern[pos + 1..end], text[i - 1]);
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
                    next[i] = previous[i - 1] && text[i - 1] == literal;
                }
                pos += 1;
            }
        }
        previous = next;
    }
    Some(previous[text.len()])
}

fn class_end(pattern: &[u8], mut pos: usize) -> Option<usize> {
    if matches!(pattern.get(pos), Some(b'!' | b'^')) {
        pos += 1;
    }
    if pattern.get(pos) == Some(&b']') {
        pos += 1;
    }
    while pos < pattern.len() {
        if pattern[pos..].starts_with(b"[:") {
            let offset = pattern[pos + 2..].windows(2).position(|s| s == b":]")?;
            pos += offset + 4;
        } else if pattern[pos] == b']' {
            return Some(pos);
        } else {
            pos += if pattern[pos] == b'\\' { 2 } else { 1 };
        }
    }
    None
}

fn class(mut pattern: &[u8], byte: u8) -> bool {
    let negative = matches!(pattern.first(), Some(b'!' | b'^'));
    if negative {
        pattern = &pattern[1..];
    }
    let mut found = false;
    while !pattern.is_empty() {
        if pattern.starts_with(b"[:") {
            let Some(end) = pattern.windows(2).position(|s| s == b":]") else {
                return false;
            };
            found |= match &pattern[2..end] {
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
                _ => false,
            };
            pattern = &pattern[end + 2..];
            continue;
        }
        let (first, rest) = class_byte(pattern);
        pattern = rest;
        if pattern.len() >= 2 && pattern[0] == b'-' {
            let (last, rest) = class_byte(&pattern[1..]);
            found |= first <= byte && byte <= last;
            pattern = rest;
        } else {
            found |= first == byte;
        }
    }
    found != negative
}

fn class_byte(pattern: &[u8]) -> (u8, &[u8]) {
    let pos = usize::from(pattern[0] == b'\\' && pattern.len() > 1);
    (pattern[pos], &pattern[pos + 1..])
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    #[rstest]
    #[case::single(b"a/*/c", b"a/b/d/c", false)]
    #[case::recursive(b"a/**/c", b"a/b/d/c", true)]
    #[case::zero_components(b"a/**/c", b"a/c", true)]
    #[case::tail(b"a/**", b"a/b/d", true)]
    #[case::class(b"[[:digit:]][!a-z]", b"4Z", true)]
    #[case::escape(b"a\\*", b"a*", true)]
    #[case::question(b"a?b", b"a/b", false)]
    fn glob(#[case] pattern: &[u8], #[case] text: &[u8], #[case] expected: bool) {
        assert_eq!(matches(pattern, text, false, 10000), Some(expected));
    }
}
