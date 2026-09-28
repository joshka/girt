use std::ops::Range;

use thiserror::Error;

use super::Origin;

/// Byte-oriented configuration entries from parsing or explicit layered resolution.
///
/// Supports quoted values, comments, LF/CRLF, continuations, implicit booleans and quoted
/// subsections. Deprecated dotted subsections are folded to ASCII lowercase. Parsing retains
/// include directives without I/O; [`Self::resolve`] expands them and attaches provenance. Scalar
/// lookup returns the last occurrence; [`Self::values`] preserves all occurrences.
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
    pub(super) entries: Vec<Entry>,
    pub(super) sections: Vec<SectionName>,
    pub(super) occurrences: Vec<SectionOccurrence>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct SectionName {
    pub section: Vec<u8>,
    pub subsection: Option<Vec<u8>>,
}

/// One section occurrence in a parsed or resolved configuration snapshot.
///
/// Repeated headers and repeated include visits have distinct ordinals, including empty headers.
/// Environment assignments each form a synthetic occurrence without a physical header. Membership
/// can be noncontiguous because an include inserts entries before the outer section resumes.
/// These borrowed views identify snapshot membership, not writable source sections; use
/// [`super::Document::sections`] to edit a direct file.
#[derive(Debug, Clone, Copy)]
pub struct ConfigSection<'a> {
    ordinal: usize,
    occurrence: &'a SectionOccurrence,
}

impl<'a> ConfigSection<'a> {
    /// Zero-based occurrence position within this snapshot, not a persistent source identity.
    pub fn ordinal(&self) -> usize {
        self.ordinal
    }

    /// Original section-name bytes; matching folds ASCII case.
    pub fn name(&self) -> &'a [u8] {
        &self.occurrence.name.section
    }

    /// Decoded subsection; parsing lowercases deprecated dotted subsections.
    pub fn subsection(&self) -> Option<&'a [u8]> {
        self.occurrence.name.subsection.as_deref()
    }

    /// Increasing member indices into [`Config::entries`], excluding included sections' entries.
    ///
    /// Empty headers have no members. The indices remain valid for the borrowed snapshot only.
    pub fn entry_indices(&self) -> &'a [usize] {
        &self.occurrence.entries
    }
}

#[derive(Debug, Clone)]
pub(super) struct SectionOccurrence {
    pub name: SectionName,
    // Header position before this entry, including empty headers at EOF. Together with member
    // indices this preserves interleaving when a resolved snapshot is supplied as runtime input.
    pub start: usize,
    pub entries: Vec<usize>,
}

/// One variable occurrence, retaining spelling, bytes and source location.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Physical line where this variable begins, or zero for supplied decoded entries.
    pub line: usize,
    /// Source and include ancestry, absent for a purely parsed source.
    pub origin: Option<Origin>,
    /// Original section name bytes; lookup folds ASCII case.
    pub section: Vec<u8>,
    /// Decoded subsection; parsing lowercases deprecated dotted subsections.
    pub subsection: Option<Vec<u8>>,
    /// Original variable name bytes; lookup folds ASCII case.
    pub name: Vec<u8>,
    /// Decoded value; `None` is an implicit boolean.
    pub value: Option<Vec<u8>>,
}

/// Invalid syntax, decoded names, or construction limits in supplied configuration.
#[derive(Debug, Error)]
#[error("configuration line {line}: {reason}")]
pub struct ConfigError {
    /// One-based physical line, or input section ordinal for decoded construction.
    pub line: usize,
    /// Explanation of the rejected syntax.
    pub reason: &'static str,
}

impl Config {
    /// Parses one complete source without reading other files.
    ///
    /// A UTF-8 BOM is accepted only at the start. Escape sequences are `\n`, `\t`, `\b`,
    /// `\\`, and `\"`; backslash-newline joins physical lines. Unquoted surrounding whitespace
    /// is removed, while quoted whitespace and whitespace before a continuation after value text
    /// are preserved. An absent `=` is distinct from an empty value. Comments may contain
    /// uninterpreted NUL bytes; NUL in names, subsections or values and malformed syntax return
    /// an error.
    ///
    /// # Errors
    ///
    /// Returns the physical line and reason for invalid or unsupported syntax.
    pub fn parse(bytes: &[u8]) -> Result<Self, ConfigError> {
        Self::parse_internal::<false>(bytes).map(|(config, _)| config)
    }

    pub(super) fn parse_layout(bytes: &[u8]) -> Result<(Self, Layout), ConfigError> {
        Self::parse_internal::<true>(bytes)
    }

    fn parse_internal<const RETAIN_LAYOUT: bool>(
        bytes: &[u8],
    ) -> Result<(Self, Layout), ConfigError> {
        let offset = usize::from(bytes.starts_with(b"\xef\xbb\xbf")) * 3;
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        let mut parser = Parser {
            bytes,
            pos: 0,
            line: 1,
            value_end: 0,
        };
        let mut entries = Vec::new();
        let mut sections = Vec::new();
        let mut occurrences: Vec<SectionOccurrence> = Vec::new();
        let mut layout = Layout::default();
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
                    let start = parser.pos;
                    parser.take();
                    section = parser.name(false)?;
                    subsection = None;
                    if parser.peek() == Some(b'.') {
                        parser.take();
                        let start = parser.pos;
                        while parser
                            .peek()
                            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
                        {
                            parser.take();
                        }
                        subsection = Some(parser.bytes[start..parser.pos].to_ascii_lowercase());
                    }
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
                        return Err(parser.error("expected ]"));
                    }
                    let name = SectionName {
                        section: section.clone(),
                        subsection: subsection.clone(),
                    };
                    sections.push(name.clone());
                    occurrences.push(SectionOccurrence {
                        name,
                        start: entries.len(),
                        entries: Vec::new(),
                    });
                    if RETAIN_LAYOUT {
                        layout.sections.push(SectionSpan {
                            range: start + offset..parser.pos + offset,
                            section: section.clone(),
                            subsection: subsection.clone(),
                        });
                    }
                }
                Some(_) => {
                    if section.is_empty() {
                        return Err(parser.error("variable outside section"));
                    }
                    let start = parser.pos;
                    let line = parser.line;
                    let name = parser.name(true)?;
                    let name_end = parser.pos;
                    parser.space();
                    let mut value_start = name_end;
                    let value = match parser.peek() {
                        Some(b'=') => {
                            parser.take();
                            parser.space();
                            value_start = parser.pos;
                            Some(parser.value()?)
                        }
                        None | Some(b'\n' | b'#' | b';') => None,
                        _ => return Err(parser.error("expected = or end of variable")),
                    };
                    let end = if value.is_some() {
                        parser.value_end
                    } else {
                        name_end
                    };
                    if RETAIN_LAYOUT {
                        layout.entries.push(EntrySpan {
                            range: start + offset..end + offset,
                            value: value_start + offset..end + offset,
                        });
                    }
                    occurrences
                        .last_mut()
                        .expect("section checked above")
                        .entries
                        .push(entries.len());
                    entries.push(Entry {
                        line,
                        origin: None,
                        section: section.clone(),
                        subsection: subsection.clone(),
                        name,
                        value,
                    });
                }
            }
        }
        Ok((
            Self {
                entries,
                sections,
                occurrences,
            },
            layout,
        ))
    }

    /// Whether a section exists, including an empty parsed header.
    ///
    /// Section names use ASCII case folding; subsection bytes are exact. Resolved includes retain
    /// their empty headers. Environment assignments imply their section even though they have no
    /// physical header. This query does not identify a writable source or physical section.
    pub fn contains_section(&self, section: &str, subsection: Option<&[u8]>) -> bool {
        self.sections.iter().any(|header| {
            header.section.eq_ignore_ascii_case(section.as_bytes())
                && header.subsection.as_deref() == subsection
        }) || self.entries.iter().any(|entry| {
            entry.section.eq_ignore_ascii_case(section.as_bytes())
                && entry.subsection.as_deref() == subsection
        })
    }

    /// Lists every subsection of a section, once, sorted by exact subsection bytes.
    ///
    /// Includes empty headers and sections implied by environment assignments. Section names
    /// ignore ASCII case; subsection bytes retain case, non-UTF-8 bytes and the empty name.
    /// Headers without a subsection are excluded. This query does not expose source ownership,
    /// physical section occurrences or precedence order.
    ///
    /// ```
    /// use girt::Config;
    /// let config = Config::parse(b"[remote \"z\"]\nurl=repo\n[remote \"a\"]\n")?;
    /// assert_eq!(config.subsection_names("REMOTE"), [b"a".as_slice(), b"z"]);
    /// # Ok::<(), girt::ConfigError>(())
    /// ```
    pub fn subsection_names(&self, section: &str) -> Vec<&[u8]> {
        let headers = self
            .sections
            .iter()
            .map(|header| (header.section.as_slice(), header.subsection.as_deref()));
        let entries = self
            .entries
            .iter()
            .map(|entry| (entry.section.as_slice(), entry.subsection.as_deref()));
        let names: std::collections::BTreeSet<_> = headers
            .chain(entries)
            .filter_map(|(name, subsection)| {
                name.eq_ignore_ascii_case(section.as_bytes())
                    .then_some(subsection)
                    .flatten()
            })
            .collect();
        names.into_iter().collect()
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

    pub(crate) fn append(&mut self, other: &Self) {
        let offset = self.entries.len();
        self.occurrences
            .extend(other.occurrences.iter().map(|occurrence| {
                SectionOccurrence {
                    name: occurrence.name.clone(),
                    start: occurrence.start + offset,
                    entries: occurrence
                        .entries
                        .iter()
                        .map(|index| index + offset)
                        .collect(),
                }
            }));
        self.entries.extend_from_slice(&other.entries);
        self.sections.extend_from_slice(&other.sections);
    }

    /// Borrows section occurrences in header encounter order, retaining empty and repeated headers.
    ///
    /// Include expansion assigns a fresh identity on every visit and retains outer membership when
    /// an included file returns. This does not change flat [`Self::value`] or [`Self::values`]
    /// semantics or apply any policy to implicit values. No I/O is performed.
    ///
    /// ```
    /// use girt::Config;
    /// let config = Config::parse(b"[core]\nx=first\nx\n[core]\nx=last\n[empty]\n")?;
    /// let sections: Vec<_> = config.section_occurrences().collect();
    /// assert_eq!(sections[0].entry_indices(), &[0, 1]);
    /// assert_eq!(sections[1].entry_indices(), &[2]);
    /// assert!(sections[2].entry_indices().is_empty());
    /// assert!(
    ///     config.entries()[sections[0].entry_indices()[1]]
    ///         .value
    ///         .is_none()
    /// );
    /// # Ok::<(), girt::ConfigError>(())
    /// ```
    pub fn section_occurrences(&self) -> impl ExactSizeIterator<Item = ConfigSection<'_>> {
        self.occurrences
            .iter()
            .enumerate()
            .map(|(ordinal, occurrence)| ConfigSection {
                ordinal,
                occurrence,
            })
    }

    /// Returns occurrences in source or resolved precedence order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct Layout {
    pub entries: Vec<EntrySpan>,
    pub sections: Vec<SectionSpan>,
}

#[derive(Debug, Clone)]
pub(super) struct EntrySpan {
    pub range: Range<usize>,
    pub value: Range<usize>,
}

#[derive(Debug, Clone)]
pub(super) struct SectionSpan {
    pub range: Range<usize>,
    pub section: Vec<u8>,
    pub subsection: Option<Vec<u8>>,
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    line: usize,
    value_end: usize,
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
        Ok(self.bytes[start..self.pos].to_vec())
    }
    fn value(&mut self) -> Result<Vec<u8>, ConfigError> {
        self.space();
        let mut value = Vec::new();
        let mut quoted = false;
        let mut keep = 0;
        self.value_end = self.pos;
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
                    self.value_end = self.pos;
                }
                b'#' | b';' if !quoted => {
                    self.comment();
                    break;
                }
                b'\\' => {
                    let escaped = match self.take() {
                        Some(b'\n') => {
                            keep = value.len();
                            self.value_end = self.pos;
                            continue;
                        }
                        Some(b'n') => b'\n',
                        Some(b't') => b'\t',
                        Some(b'b') => 8,
                        Some(b'\\') => b'\\',
                        Some(b'"') => b'"',
                        _ => return Err(self.error("invalid escape")),
                    };
                    value.push(escaped);
                    keep = value.len();
                    self.value_end = self.pos;
                }
                b' ' | b'\t' if !quoted && value.is_empty() => {}
                byte => {
                    value.push(byte);
                    if quoted || !matches!(byte, b' ' | b'\t') {
                        keep = value.len();
                        self.value_end = self.pos;
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
    #[case::absent(b"", vec![])]
    #[case::bare(b"[remote]", vec![])]
    #[case::empty_header(b"[remote \"a\"]", vec![b"a".as_slice()])]
    #[case::empty_name(b"[remote \"\"]", vec![b"".as_slice()])]
    #[case::section_case(b"[REMOTE \"a\"]\n[Remote \"a\"]", vec![b"a".as_slice()])]
    #[case::subsection_case(b"[remote \"a\"]\n[remote \"A\"]", vec![b"A".as_slice(), b"a"])]
    #[case::non_utf8(b"[remote \"\xff\"]\n[remote \"a\"]", vec![b"a".as_slice(), b"\xff"])]
    #[case::entries_and_empty(b"[remote \"z\"]\nurl=repo\n[remote \"a\"]\n[remote \"z\"]", vec![b"a".as_slice(), b"z"])]
    #[case::other_section(b"[url \"a\"]\ninsteadOf=b", vec![])]
    fn complete_subsection_names(#[case] bytes: &[u8], #[case] expected: Vec<&[u8]>) {
        assert_eq!(
            Config::parse(bytes).unwrap().subsection_names("remote"),
            expected
        );
    }

    #[test]
    fn section_occurrences_preserve_headers_and_membership() {
        let config = Config::parse(b"\xef\xbb\xbf[Core] x=one\r\nx\r\n[empty]\r\n[remote.UPPER]\r\nurl=repo\r\n[remote \"UPPER\"]\r\n[Core]\r\nx=two\r\n").unwrap();
        let sections: Vec<_> = config.section_occurrences().collect();
        assert_eq!(
            sections.iter().map(|s| s.ordinal()).collect::<Vec<_>>(),
            [0, 1, 2, 3, 4]
        );
        assert_eq!(
            sections
                .iter()
                .map(|s| s.entry_indices())
                .collect::<Vec<_>>(),
            [&[0, 1][..], &[], &[2], &[], &[3]]
        );
        assert_eq!(sections[0].name(), b"Core");
        assert_eq!(sections[2].subsection(), Some(b"upper".as_slice()));
        assert_eq!(sections[3].subsection(), Some(b"UPPER".as_slice()));
        assert_eq!(
            config.values("core", None, "x").collect::<Vec<_>>(),
            [Some(b"one".as_slice()), None, Some(b"two".as_slice())]
        );
        assert_eq!(
            config.value("core", None, "x"),
            Some(Some(b"two".as_slice()))
        );
    }

    #[test]
    fn append_rebases_members_and_retains_empty_headers() {
        let mut config = Config::parse(b"[core]\nx=one\n[empty]\n").unwrap();
        config.append(&Config::parse(b"[empty]\n[core]\nx=two\n[tail]\n").unwrap());
        let members: Vec<_> = config
            .section_occurrences()
            .map(|section| section.entry_indices())
            .collect();
        assert_eq!(members, [&[0][..], &[], &[], &[1], &[]]);
        assert_eq!(
            config.entries()[1].value.as_deref(),
            Some(b"two".as_slice())
        );
    }

    #[test]
    fn append_preserves_empty_headers_and_entry_order() {
        let mut config = Config::parse(b"[remote \"z\"]\nurl=first\n").unwrap();
        config.append(&Config::parse(b"[remote \"a\"]\n[remote \"z\"]\nurl=second\n").unwrap());
        assert!(config.contains_section("remote", Some(b"a")));
        assert_eq!(config.subsection_names("remote"), [b"a".as_slice(), b"z"]);
        assert_eq!(
            config
                .values("remote", Some(b"z"), "url")
                .collect::<Vec<_>>(),
            [Some(b"first".as_slice()), Some(b"second".as_slice())]
        );
    }

    #[test]
    fn legacy_remote_names_stay_in_first_entry_order() {
        let config = Config::parse(b"[remote \"empty\"]\n[remote \"z\"]\nurl=repo\n[remote \"a\"]\nurl=repo\n[remote \"z\"]\nfetch=HEAD").unwrap();
        assert_eq!(
            crate::remote::Remote::names(&config),
            [b"z".as_slice(), b"a"]
        );
        assert_eq!(
            crate::remote::RemoteUrls::names(&config),
            [b"z".as_slice(), b"a"]
        );
        assert_eq!(
            config.subsection_names("remote"),
            [b"a".as_slice(), b"empty", b"z"]
        );
    }

    #[rstest]
    #[case::quote(b"[core]\nx = \" a # ; \" ; comment\n", b" a # ; ")]
    #[case::continuation(b"[core]\nx = ab\\\ncd\n", b"abcd")]
    #[case::escapes(b"[core]\nx = \\n\\t\\b\\\\\\\"\n", b"\n\t\x08\\\"")]
    #[case::bytes(b"[core]\nx = \xff\n", b"\xff")]
    #[case::continued_space(b"[core]\nx = first  \\\n\n", b"first  ")]
    #[case::continued_space_comment(b"[core]\nx = first\t \\\n# comment\n", b"first\t ")]
    #[case::continued_empty(b"[core]\nx = \\\n\n", b"")]
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
    #[case::nul_quoted(b"[core]\nx = \"a\0b\"")]
    #[case::nul_key(b"[core]\nx\0 = a")]
    #[case::nul_section(b"[co\0re]\nx=a")]
    #[case::nul_subsection(b"[core \"a\0b\"]\nx=a")]
    #[case::nul_subsection_escape(b"[core \"a\\\0b\"]\nx=a")]
    #[case::nul_between(b"[core]\n\0\nx=a")]
    #[case::quoted_comment(b"[core]\nx=\"#\0\"")]
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
