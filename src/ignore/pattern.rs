use super::rules::charge;
use super::{Case, Error};

/// Compiled once so repeated queries do not rescan classes or allocate pattern copies.
#[derive(Debug)]
pub(super) struct Pattern {
    pub negative: bool,
    directory: bool,
    anchored: bool,
    case: Case,
    expression: Expression,
}
#[derive(Debug)]
enum Expression {
    Invalid,
    Literal(Vec<u8>),
    Glob(Vec<Token>),
}
#[derive(Debug)]
enum Token {
    Byte(u8),
    Any,
    Star,
    Recursive,
    Directories,
    Class([u64; 4]),
}
impl Pattern {
    pub fn parse(raw: &[u8], case: Case) -> Option<Self> {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let raw = raw.split(|b| *b == 0).next().unwrap_or_default();
        let mut end = raw.len();
        while end > 0 && raw[end - 1] == b' ' {
            let escapes = raw[..end - 1]
                .iter()
                .rev()
                .take_while(|b| **b == b'\\')
                .count();
            if escapes % 2 == 1 {
                break;
            }
            end -= 1;
        }
        let mut bytes = &raw[..end];
        if bytes.is_empty() || bytes[0] == b'#' {
            return None;
        }
        let negative = bytes[0] == b'!';
        if negative {
            bytes = &bytes[1..];
        }
        let directory = bytes.ends_with(b"/");
        if directory {
            bytes = &bytes[..bytes.len() - 1];
        }
        let anchored = bytes.contains(&b'/');
        bytes = bytes.strip_prefix(b"/").unwrap_or(bytes);
        let expression = compile(bytes, case).unwrap_or(Expression::Invalid);
        Some(Self {
            negative,
            directory,
            anchored,
            case,
            expression,
        })
    }

    pub fn matches(&self, path: &[u8], directory: bool, work: &mut usize) -> Result<bool, Error> {
        if self.directory && !directory {
            return Ok(false);
        }
        charge(work, path.len())?;
        let text = if self.anchored {
            path
        } else {
            path.rsplit(|b| *b == b'/').next().unwrap_or(path)
        };
        match &self.expression {
            Expression::Invalid => Ok(false),
            Expression::Literal(literal) => Ok(match self.case {
                Case::Sensitive => literal == text,
                Case::AsciiInsensitive => literal.eq_ignore_ascii_case(text),
            }),
            Expression::Glob(tokens) => {
                // Most ignore rules have literal prefixes. Reject these without allocating rows.
                for (index, token) in tokens.iter().enumerate() {
                    let Token::Byte(expected) = token else {
                        break;
                    };
                    charge(work, 1)?;
                    let Some(actual) = text.get(index) else {
                        return Ok(false);
                    };
                    let equal = match self.case {
                        Case::Sensitive => expected == actual,
                        Case::AsciiInsensitive => expected.eq_ignore_ascii_case(actual),
                    };
                    if !equal {
                        return Ok(false);
                    }
                }
                charge(
                    work,
                    tokens
                        .len()
                        .saturating_add(1)
                        .saturating_mul(text.len().saturating_add(1)),
                )?;
                Ok(glob(tokens, text, self.case))
            }
        }
    }
}

fn compile(pattern: &[u8], case: Case) -> Option<Expression> {
    if pattern.is_empty() {
        return None;
    }
    let mut tokens = Vec::new();
    let mut pos = 0;
    while pos < pattern.len() {
        let token = match pattern[pos] {
            b'\\' => {
                pos += 1;
                Token::Byte(*pattern.get(pos)?)
            }
            b'?' => Token::Any,
            b'*' => {
                let start = pos;
                while pattern.get(pos + 1) == Some(&b'*') {
                    pos += 1;
                }
                let recursive = pos > start
                    && (start == 0 || pattern[start - 1] == b'/')
                    && (pos + 1 == pattern.len()
                        || pattern[pos + 1] == b'/'
                        || pattern[pos + 1..].starts_with(b"\\/"));
                if recursive && pattern.get(pos + 1) == Some(&b'/') {
                    pos += 1;
                    Token::Directories
                } else if recursive {
                    Token::Recursive
                } else {
                    Token::Star
                }
            }
            b'[' => {
                let (bits, end) = class(pattern, pos + 1, case)?;
                pos = end;
                Token::Class(bits)
            }
            byte => Token::Byte(byte),
        };
        tokens.push(token);
        pos += 1;
    }
    if tokens.iter().all(|t| matches!(t, Token::Byte(_))) {
        let literal = tokens
            .into_iter()
            .map(|t| match t {
                Token::Byte(b) => b,
                _ => unreachable!(),
            })
            .collect();
        Some(Expression::Literal(literal))
    } else {
        Some(Expression::Glob(tokens))
    }
}

/// Two reusable rows bound memory to O(path length). Each token visits each path byte once.
fn glob(tokens: &[Token], text: &[u8], case: Case) -> bool {
    let mut previous = vec![false; text.len() + 1];
    let mut next = vec![false; text.len() + 1];
    previous[0] = true;
    for token in tokens {
        next.fill(false);
        match token {
            Token::Star | Token::Recursive => {
                next[0] = previous[0];
                for i in 1..=text.len() {
                    next[i] = previous[i]
                        || (next[i - 1]
                            && (matches!(token, Token::Recursive) || text[i - 1] != b'/'));
                }
            }
            Token::Directories => {
                let mut reachable = false;
                for i in 0..=text.len() {
                    next[i] = previous[i] || (i > 0 && text[i - 1] == b'/' && reachable);
                    reachable |= previous[i];
                }
            }
            _ => {
                for i in 1..=text.len() {
                    let byte = text[i - 1];
                    let matched = match token {
                        Token::Byte(expected) => match case {
                            Case::Sensitive => *expected == byte,
                            Case::AsciiInsensitive => expected.eq_ignore_ascii_case(&byte),
                        },
                        Token::Any => byte != b'/',
                        Token::Class(bits) => {
                            byte != b'/'
                                && bits[usize::from(byte) / 64] & (1u64 << (byte % 64)) != 0
                        }
                        _ => unreachable!(),
                    };
                    next[i] = previous[i - 1] && matched;
                }
            }
        }
        std::mem::swap(&mut previous, &mut next);
    }
    previous[text.len()]
}

fn class(pattern: &[u8], mut pos: usize, case: Case) -> Option<([u64; 4], usize)> {
    let negative = matches!(pattern.get(pos), Some(b'!' | b'^'));
    pos += usize::from(negative);
    let mut bits = [0u64; 4];
    let mut first = true;
    while let Some(&byte) = pattern.get(pos) {
        if byte == b']' && !first {
            if negative {
                bits.iter_mut().for_each(|word| *word = !*word);
            }
            return Some((bits, pos));
        }
        first = false;
        if pattern[pos..].starts_with(b"[:") {
            let end = pos + 2 + pattern[pos + 2..].windows(2).position(|s| s == b":]")?;
            let predicate = named_class(&pattern[pos + 2..end])?;
            for b in 0..=u8::MAX {
                if predicate(b) {
                    insert(&mut bits, b, case);
                }
            }
            pos = end + 2;
        } else {
            let (low, next) = class_byte(pattern, pos)?;
            pos = next;
            if pattern.get(pos) == Some(&b'-') && pattern.get(pos + 1).is_some_and(|b| *b != b']') {
                let (high, next) = class_byte(pattern, pos + 1)?;
                for b in low..=high {
                    insert(&mut bits, b, case);
                }
                pos = next;
            } else {
                insert(&mut bits, low, case);
            }
        }
    }
    None
}
fn class_byte(pattern: &[u8], mut pos: usize) -> Option<(u8, usize)> {
    if pattern.get(pos) == Some(&b'\\') {
        pos += 1;
    }
    Some((*pattern.get(pos)?, pos + 1))
}
fn insert(bits: &mut [u64; 4], byte: u8, case: Case) {
    bits[usize::from(byte) / 64] |= 1u64 << (byte % 64);
    if case == Case::AsciiInsensitive {
        let lower = byte.to_ascii_lowercase();
        let upper = byte.to_ascii_uppercase();
        bits[usize::from(lower) / 64] |= 1u64 << (lower % 64);
        bits[usize::from(upper) / 64] |= 1u64 << (upper % 64);
    }
}
fn named_class(name: &[u8]) -> Option<fn(u8) -> bool> {
    Some(match name {
        b"alnum" => |b| b.is_ascii_alphanumeric(),
        b"alpha" => |b| b.is_ascii_alphabetic(),
        b"blank" => |b| matches!(b, b' ' | b'\t'),
        b"cntrl" => |b| b.is_ascii_control(),
        b"digit" => |b| b.is_ascii_digit(),
        b"graph" => |b| b.is_ascii_graphic(),
        b"lower" => |b| b.is_ascii_lowercase(),
        b"print" => |b| b.is_ascii_graphic() || b == b' ',
        b"punct" => |b| b.is_ascii_punctuation(),
        b"space" => |b| b.is_ascii_whitespace(),
        b"upper" => |b| b.is_ascii_uppercase(),
        b"xdigit" => |b| b.is_ascii_hexdigit(),
        _ => return None,
    })
}
