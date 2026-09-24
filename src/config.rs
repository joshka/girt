//! Byte-oriented parsing of one explicitly supplied Git configuration source.
//!
//! This module performs no I/O, includes, interpolation, environment lookup, or precedence merging.
//! Values retain arbitrary bytes. Section and variable names compare ASCII case-insensitively;
//! quoted subsections compare exactly. Repeated keys retain order and scalar lookup returns the
//! last.
use thiserror::Error;

/// Parsed entries from a single source, in occurrence order.
///
/// Supports quoted values, comments, LF/CRLF, continuations, implicit booleans and quoted
/// subsections. Deprecated dotted section syntax is explicitly unsupported. Includes are retained
/// as entries; repository opening rejects them rather than silently skipping another configuration
/// source.
///
/// ```
/// use girt::Config;
/// let config = Config::parse(b"[remote \"origin\"]\nurl = https://example.com/repo\n")?;
/// assert_eq!(
///     config.value("remote", Some(b"origin"), "url"),
///     Some(Some(b"https://example.com/repo".as_slice()))
/// );
/// # Ok::<(), girt::ConfigError>(())
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    entries: Vec<Entry>,
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub section: Vec<u8>,
    pub subsection: Option<Vec<u8>>,
    pub name: Vec<u8>,
    pub value: Option<Vec<u8>>,
}

/// Invalid or unsupported syntax in the supplied configuration bytes.
#[derive(Debug, Error)]
#[error("configuration line {line}: {reason}")]
pub struct ConfigError {
    /// One-based physical line where parsing detected the failure.
    pub line: usize,
    /// Explanation of the rejected syntax.
    pub reason: &'static str,
}

impl Config {
    /// Parses one complete source without reading other files.
    ///
    /// A UTF-8 BOM is accepted only at the start. Escape sequences are `\n`, `\t`, `\b`,
    /// `\\`, and `\"`; backslash-newline joins physical lines. Unquoted surrounding whitespace
    /// is removed, while quoted whitespace is preserved. An absent `=` is distinct from an empty
    /// value. NUL bytes, malformed syntax and dotted section syntax return an error.
    ///
    /// # Errors
    ///
    /// Returns the physical line and reason for invalid or unsupported syntax.
    pub fn parse(bytes: &[u8]) -> Result<Self, ConfigError> {
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        if let Some(position) = bytes.iter().position(|byte| *byte == 0) {
            return Err(ConfigError {
                line: 1 + bytes[..position]
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count(),
                reason: "NUL in configuration",
            });
        }
        let mut parser = Parser {
            bytes,
            pos: 0,
            line: 1,
        };
        let mut entries = Vec::new();
        let mut section = Vec::new();
        let mut subsection = None;
        while parser.pos < bytes.len() {
            parser.space();
            match parser.peek() {
                None => break,
                Some(b'\n') => {
                    parser.take();
                }
                Some(b'#' | b';') => parser.comment(),
                Some(b'[') => {
                    parser.take();
                    section = parser.name(false)?;
                    subsection = None;
                    let separated = matches!(parser.peek(), Some(b' ' | b'\t'));
                    parser.space();
                    if parser.peek() == Some(b'"') {
                        if !separated {
                            return Err(parser.error("subsection requires whitespace"));
                        }
                        parser.take();
                        let mut value = Vec::new();
                        loop {
                            match parser.take() {
                                Some(b'"') => break,
                                Some(b'\\') => match parser.take() {
                                    Some(b'\n' | 0) | None => {
                                        return Err(parser.error("invalid subsection escape"));
                                    }
                                    Some(byte) => value.push(byte),
                                },
                                Some(b'\n' | 0) | None => {
                                    return Err(parser.error("unterminated subsection"));
                                }
                                Some(byte) => value.push(byte),
                            }
                        }
                        subsection = Some(value);
                        parser.space();
                    }
                    if parser.take() != Some(b']') {
                        return Err(parser.error("expected ]; dotted sections are unsupported"));
                    }
                }
                Some(_) => {
                    if section.is_empty() {
                        return Err(parser.error("variable outside section"));
                    }
                    let name = parser.name(true)?;
                    parser.space();
                    let value = match parser.peek() {
                        Some(b'=') => {
                            parser.take();
                            Some(parser.value()?)
                        }
                        None | Some(b'\n' | b'#' | b';') => None,
                        _ => return Err(parser.error("expected = or end of variable")),
                    };
                    entries.push(Entry {
                        section: section.clone(),
                        subsection: subsection.clone(),
                        name,
                        value,
                    });
                }
            }
        }
        Ok(Self { entries })
    }

    /// Returns all occurrences, preserving order and distinguishing implicit from empty values.
    /// Names use ASCII case folding; subsection bytes are exact. No key-string splitting is done.
    pub fn values<'a>(
        &'a self,
        section: &'a str,
        subsection: Option<&'a [u8]>,
        name: &'a str,
    ) -> impl DoubleEndedIterator<Item = Option<&'a [u8]>> + 'a {
        self.entries
            .iter()
            .filter(move |entry| {
                entry.section.eq_ignore_ascii_case(section.as_bytes())
                    && entry.subsection.as_deref() == subsection
                    && entry.name.eq_ignore_ascii_case(name.as_bytes())
            })
            .map(|entry| entry.value.as_deref())
    }

    /// Returns the last occurrence; outer `None` means absent, inner `None` means implicit true.
    pub fn value(
        &self,
        section: &str,
        subsection: Option<&[u8]>,
        name: &str,
    ) -> Option<Option<&[u8]>> {
        self.entries
            .iter()
            .rev()
            .find(|entry| {
                entry.section.eq_ignore_ascii_case(section.as_bytes())
                    && entry.subsection.as_deref() == subsection
                    && entry.name.eq_ignore_ascii_case(name.as_bytes())
            })
            .map(|entry| entry.value.as_deref())
    }

    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    line: usize,
}
impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        match self.bytes.get(self.pos).copied() {
            Some(b'\r') if self.bytes.get(self.pos + 1) == Some(&b'\n') => Some(b'\n'),
            byte => byte,
        }
    }
    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        if self.bytes.get(self.pos) == Some(&b'\r') && byte == b'\n' {
            self.pos += 1;
        }
        self.pos += 1;
        if byte == b'\n' {
            self.line += 1;
        }
        Some(byte)
    }
    fn error(&self, reason: &'static str) -> ConfigError {
        ConfigError {
            line: self.line,
            reason,
        }
    }
    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.take();
        }
    }
    fn comment(&mut self) {
        while !matches!(self.peek(), None | Some(b'\n')) {
            self.take();
        }
    }
    fn name(&mut self, variable: bool) -> Result<Vec<u8>, ConfigError> {
        let start = self.pos;
        if variable && !self.peek().is_some_and(|b| b.is_ascii_alphabetic()) {
            return Err(self.error("invalid variable name"));
        }
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            self.take();
        }
        if start == self.pos {
            return Err(self.error("empty name"));
        }
        Ok(self.bytes[start..self.pos].to_ascii_lowercase())
    }
    fn value(&mut self) -> Result<Vec<u8>, ConfigError> {
        self.space();
        let mut value = Vec::new();
        let mut quoted = false;
        let mut keep = 0;
        while let Some(byte) = self.take() {
            match byte {
                0 => return Err(self.error("NUL in value")),
                b'\n' => {
                    if quoted {
                        return Err(self.error("unterminated quote"));
                    }
                    break;
                }
                b'"' => {
                    quoted = !quoted;
                    keep = value.len();
                }
                b'#' | b';' if !quoted => {
                    self.comment();
                    break;
                }
                b'\\' => {
                    let escaped = match self.take() {
                        Some(b'\n') => continue,
                        Some(b'n') => b'\n',
                        Some(b't') => b'\t',
                        Some(b'b') => 8,
                        Some(b'\\') => b'\\',
                        Some(b'"') => b'"',
                        _ => return Err(self.error("invalid escape")),
                    };
                    value.push(escaped);
                    keep = value.len();
                }
                b' ' | b'\t' if !quoted && value.is_empty() => {}
                byte => {
                    value.push(byte);
                    if quoted || !matches!(byte, b' ' | b'\t') {
                        keep = value.len();
                    }
                }
            }
        }
        if quoted {
            return Err(self.error("unterminated quote"));
        }
        value.truncate(keep);
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    #[rstest]
    #[case::quote(b"[core]\nx = \" a # ; \" ; comment\n", b" a # ; ")]
    #[case::continuation(b"[core]\nx = ab\\\ncd\n", b"abcd")]
    #[case::escapes(b"[core]\nx = \\n\\t\\b\\\\\\\"\n", b"\n\t\x08\\\"")]
    #[case::bytes(b"[core]\nx = \xff\n", b"\xff")]
    #[case::empty_quote_prefix(b"[core]\nx = \"\"  value\n", b"value")]
    #[case::continued_prefix(b"[core]\nx = \\\n  value\n", b"value")]
    fn parses_values(#[case] input: &[u8], #[case] expected: &[u8]) {
        let config = Config::parse(input).unwrap();
        assert_eq!(config.value("CORE", None, "X"), Some(Some(expected)));
    }
    #[rstest]
    #[case::outside(b"x = y")]
    #[case::escape(b"[core]\nx = \\q")]
    #[case::quote(b"[core]\nx = \"no")]
    #[case::nul(b"[core]\nx = \0")]
    #[case::dotted(b"[remote.origin]\nx=y")]
    #[case::double_cr_escape(b"[core]\nx = a\\\r\r\nb\n")]
    fn rejects_syntax(#[case] input: &[u8]) {
        assert!(Config::parse(input).is_err());
    }
    #[test]
    fn repeated_keys_and_subsections() {
        let config = Config::parse(b"[Remote \"Origin\"]\nURL\nurl=\nurl=last\n").unwrap();
        assert_eq!(
            config
                .values("remote", Some(b"Origin"), "url")
                .collect::<Vec<_>>(),
            vec![None, Some(b"".as_slice()), Some(b"last".as_slice())]
        );
        assert_eq!(config.value("remote", Some(b"origin"), "url"), None);
    }
}
