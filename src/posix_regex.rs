//! POSIX regular expressions, translated to the `regex-syntax` dialect `regex-automata` compiles.
//!
//! Git compiles diff function patterns and `git grep` patterns with POSIX `regcomp`. The
//! translation keeps POSIX meaning where `regex-syntax` differs and refuses constructs it cannot
//! express, such as back-references, rather than guessing.

/// Which POSIX syntax an expression uses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Flavor {
    /// Extended expressions, as `regcomp` with `REG_EXTENDED`.
    Extended,
    /// Basic expressions; as in GNU, `\+`, `\?` and `\|` are operators.
    Basic,
}

/// Translates one POSIX expression to `regex-syntax`, which differs in escapes, brackets,
/// literal operators and anchors.
pub(crate) fn translate(pattern: &str, flavor: Flavor) -> Result<String, &'static str> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut index = 0;
    // At the start of an expression, `*` (and in extended syntax `+`, `?` and `{`) is literal.
    let mut start = true;
    while let Some(&c) = chars.get(index) {
        index += 1;
        let at_start = std::mem::replace(&mut start, false);
        match (c, flavor) {
            ('[', _) => index = bracket(&chars, index, &mut out)?,
            ('\\', _) => {
                let next = *chars.get(index).ok_or("trailing backslash")?;
                index += 1;
                match (next, flavor) {
                    ('(' | '|', Flavor::Basic) => {
                        out.push(next);
                        start = true;
                    }
                    (')', Flavor::Basic) => out.push(')'),
                    ('{', Flavor::Basic) if !at_start => {
                        index = interval(&chars, index, flavor, &mut out)
                            .ok_or("malformed interval")?;
                    }
                    ('+' | '?', Flavor::Basic) if !at_start => out.push(next),
                    ('<', _) => out.push_str(r"\b{start}"),
                    ('>', _) => out.push_str(r"\b{end}"),
                    ('b' | 'B' | 'w' | 'W' | 's' | 'S', _) => {
                        out.push('\\');
                        out.push(next);
                    }
                    ('`', _) => out.push_str(r"\A"),
                    ('\'', _) => out.push_str(r"\z"),
                    ('1'..='9', _) => return Err("back-reference"),
                    _ => literal(next, &mut out),
                }
            }
            ('(' | '|', Flavor::Extended) => {
                out.push(c);
                start = true;
            }
            (')', Flavor::Extended) => out.push(')'),
            ('+' | '?', Flavor::Extended) if !at_start => out.push(c),
            ('{', Flavor::Extended) if !at_start => {
                match interval(&chars, index, flavor, &mut out) {
                    Some(next) => index = next,
                    None => literal('{', &mut out),
                }
            }
            ('*', _) if !at_start => out.push('*'),
            ('^', Flavor::Extended) => {
                out.push('^');
                start = true;
            }
            ('^', Flavor::Basic) if at_start => {
                out.push('^');
                start = true;
            }
            ('$', Flavor::Extended) => out.push('$'),
            // A basic `$` anchors only at the end of an expression.
            ('$', Flavor::Basic)
                if index == chars.len()
                    || matches!(chars.get(index..index + 2), Some(['\\', ')' | '|'])) =>
            {
                out.push('$');
            }
            ('.', _) => out.push('.'),
            _ => literal(c, &mut out),
        }
    }
    Ok(out)
}

/// Translates a POSIX bracket expression starting after `[`; returns the index after its `]`.
fn bracket(chars: &[char], mut index: usize, out: &mut String) -> Result<usize, &'static str> {
    out.push('[');
    if chars.get(index) == Some(&'^') {
        out.push('^');
        index += 1;
    }
    let mut first = true;
    loop {
        let c = *chars.get(index).ok_or("unterminated bracket expression")?;
        if c == ']' && !first {
            out.push(']');
            return Ok(index + 1);
        }
        first = false;
        if c == '['
            && let Some(&kind @ (':' | '=' | '.')) = chars.get(index + 1)
        {
            let body = chars.get(index + 2..).unwrap_or_default();
            let end = body
                .windows(2)
                .position(|pair| pair == [kind, ']'])
                .ok_or("unterminated bracket class")?;
            if kind != ':' {
                return Err("collating element or equivalence class");
            }
            out.push_str("[:");
            out.extend(body.iter().take(end));
            out.push_str(":]");
            index += 2 + end + 2;
            continue;
        }
        literal(c, out);
        index += 1;
        if chars.get(index) == Some(&'-')
            && let Some(&high) = chars.get(index + 1)
            && high != ']'
        {
            out.push('-');
            literal(high, out);
            index += 2;
        }
    }
}

/// Translates `{n}`, `{n,}`, `{,m}` or `{n,m}` after the opening brace; `None` when malformed.
fn interval(chars: &[char], index: usize, flavor: Flavor, out: &mut String) -> Option<usize> {
    let close: &[char] = match flavor {
        Flavor::Extended => &['}'],
        Flavor::Basic => &['\\', '}'],
    };
    let rest = chars.get(index..)?;
    let length = rest
        .iter()
        .position(|c| !(c.is_ascii_digit() || *c == ','))?;
    let body: String = rest.get(..length)?.iter().collect();
    if !rest.get(length..)?.starts_with(close) || body.matches(',').count() > 1 || body.is_empty() {
        return None;
    }
    let (low, high) = body.split_once(',').unwrap_or((&body, &body));
    if low.is_empty() && high.is_empty() {
        return None;
    }
    out.push('{');
    out.push_str(if low.is_empty() { "0" } else { low });
    if body.contains(',') {
        out.push(',');
        out.push_str(high);
    }
    out.push('}');
    Some(index + length + close.len())
}

/// Characters `regex-syntax` treats specially; escaping any of them yields the literal.
fn is_meta(c: char) -> bool {
    matches!(
        c,
        '\\' | '.'
            | '+'
            | '*'
            | '?'
            | '('
            | ')'
            | '|'
            | '['
            | ']'
            | '{'
            | '}'
            | '^'
            | '$'
            | '#'
            | '&'
            | '-'
            | '~'
    )
}

pub(crate) fn literal(c: char, out: &mut String) {
    if is_meta(c) {
        out.push('\\');
    }
    out.push(c);
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::{Flavor, translate};

    /// Translations keep POSIX meaning where `regex-syntax` differs.
    #[rstest]
    #[case::bracket_backslash_literal(r"[\t]", Flavor::Extended, r"[\\t]")]
    #[case::bracket_leading_close(r"[]a]", Flavor::Extended, r"[\]a]")]
    #[case::bracket_class(r"[[:alpha:]_]", Flavor::Extended, r"[[:alpha:]_]")]
    #[case::leading_star_literal("*a", Flavor::Extended, r"\*a")]
    #[case::invalid_interval_literal("a{x", Flavor::Extended, r"a\{x")]
    #[case::interval("a{2,}", Flavor::Extended, "a{2,}")]
    #[case::open_low_interval("a{,3}", Flavor::Extended, "a{0,3}")]
    #[case::word_start(r"\<fn\>", Flavor::Extended, r"\b{start}fn\b{end}")]
    #[case::basic_group(r"\(a\)*", Flavor::Basic, "(a)*")]
    #[case::basic_literal_operators("(a)+?|{", Flavor::Basic, r"\(a\)\+\?\|\{")]
    #[case::basic_interval(r"a\{2\}", Flavor::Basic, "a{2}")]
    #[case::basic_inner_caret_literal("a^b$c$", Flavor::Basic, r"a\^b\$c$")]
    fn translates(#[case] pattern: &str, #[case] flavor: Flavor, #[case] expected: &str) {
        assert_eq!(translate(pattern, flavor).unwrap(), expected);
    }
}
