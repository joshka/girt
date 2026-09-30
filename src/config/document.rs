use std::ops::Range;

use super::parse::Layout;
use super::{Config, ConfigError};

/// One losslessly retained configuration file, without include expansion.
///
/// Edits preserve bytes outside targeted syntax ranges, including comments, whitespace, unknown
/// variables and duplicate ordering. New entries use quoted values and LF; existing line endings
/// remain unchanged. Removed syntax leaves its surrounding whitespace/comments in place. No files
/// are read or written. Occurrence indices come from [`Self::config`] and become stale after edits.
#[derive(Debug, Clone)]
pub struct Document {
    bytes: Vec<u8>,
    config: Config,
    layout: Layout,
}

/// One physical section header and its direct-file assignment occurrences.
///
/// Repeated headers are distinct even when their decoded names match. This view borrows its
/// [`Document`]; ordinal and entry indices describe that document's current state and become stale
/// after any edit. Comments and empty sections do not contribute entry occurrences.
#[derive(Debug, Clone)]
pub struct DocumentSection<'a> {
    ordinal: usize,
    name: &'a [u8],
    subsection: Option<&'a [u8]>,
    entries: Range<usize>,
}

impl<'a> DocumentSection<'a> {
    /// Zero-based physical header position, suitable for [`Document::remove_sections`].
    pub fn ordinal(&self) -> usize {
        self.ordinal
    }

    /// Original section-name bytes; name matching usually folds ASCII case.
    pub fn name(&self) -> &'a [u8] {
        self.name
    }

    /// Exact quoted subsection bytes, or lowercase deprecated dotted subsection bytes.
    pub fn subsection(&self) -> Option<&'a [u8]> {
        self.subsection
    }

    /// Assignment occurrence range in [`Document::config`]'s [`Config::entries`].
    ///
    /// Empty sections have an empty range at the next assignment's position, or at the end.
    pub fn entry_range(&self) -> Range<usize> {
        self.entries.clone()
    }
}

impl Document {
    /// Parses one file, retaining its exact bytes (including a leading BOM).
    ///
    /// # Errors
    ///
    /// Returns the same syntax failures as [`Config::parse`].
    pub fn parse(bytes: &[u8]) -> Result<Self, ConfigError> {
        let (config, layout) = Config::parse_layout(bytes)?;
        Ok(Self {
            bytes: bytes.to_vec(),
            config,
            layout,
        })
    }

    /// Exact current file bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Current direct-file entries; includes remain unexpanded directives.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Iterates physical section headers in file order, including empty and repeated sections.
    ///
    /// Views borrow this document. Their ordinals and entry occurrence ranges become stale after
    /// edits. Includes remain ordinary direct-file entries; no included files are read.
    pub fn sections(&self) -> impl ExactSizeIterator<Item = DocumentSection<'_>> {
        (0..self.layout.sections.len()).map(|ordinal| self.section_at(ordinal))
    }

    fn section_at(&self, ordinal: usize) -> DocumentSection<'_> {
        let section = &self.layout.sections[ordinal];
        let next_header = self
            .layout
            .sections
            .get(ordinal + 1)
            .map_or(self.bytes.len(), |next| next.range.start);
        let start = self
            .layout
            .entries
            .partition_point(|entry| entry.range.start < section.range.end);
        let end = self
            .layout
            .entries
            .partition_point(|entry| entry.range.start < next_header);
        DocumentSection {
            ordinal,
            name: &section.section,
            subsection: section.subsection.as_deref(),
            entries: start..end,
        }
    }

    /// Removes selected physical headers and their assignments in one atomic edit.
    ///
    /// Preserves comments, whitespace and all unrelated bytes just like [`Self::remove_section`].
    /// Ordinals come from [`Self::sections`] in the current document; edits make previous ordinals
    /// stale. Duplicates are accepted and removed once. An empty selection is an exact no-op.
    ///
    /// # Errors
    ///
    /// Validates every ordinal before editing. An invalid ordinal or resulting syntax error leaves
    /// the entire document unchanged.
    ///
    /// # Examples
    ///
    /// ```
    /// use girt::config::Document;
    /// let mut document =
    ///     Document::parse(b"[remote \"origin\"]\nurl=first\n[remote \"origin\"]\nurl=second\n")?;
    /// let second = document.sections().nth(1).unwrap();
    /// assert_eq!(second.entry_range(), 1..2);
    /// document.remove_sections(&[second.ordinal()])?;
    /// assert_eq!(document.config().entries().len(), 1);
    /// # Ok::<(), girt::ConfigError>(())
    /// ```
    pub fn remove_sections(&mut self, ordinals: &[usize]) -> Result<(), ConfigError> {
        if ordinals
            .iter()
            .any(|&ordinal| ordinal >= self.layout.sections.len())
        {
            return Err(invalid("invalid section ordinal"));
        }
        if ordinals.is_empty() {
            return Ok(());
        }
        let mut selected = ordinals.to_vec();
        selected.sort_unstable();
        selected.dedup();
        let mut edits = Vec::new();
        for ordinal in selected {
            edits.push((self.layout.sections[ordinal].range.clone(), Vec::new()));
            let entries = self.section_at(ordinal).entry_range();
            edits.extend(
                self.layout.entries[entries]
                    .iter()
                    .map(|entry| (entry.range.clone(), Vec::new())),
            );
        }
        self.apply(edits)
    }

    /// Replaces one decoded occurrence value, preserving its key spelling and trailing comment.
    ///
    /// Implicit booleans become explicit assignments. Empty values remain distinct from implicit
    /// booleans. Other occurrences are untouched.
    ///
    /// # Errors
    ///
    /// Invalid indices or NUL values fail without changing the document.
    pub fn set_value(&mut self, occurrence: usize, value: &[u8]) -> Result<(), ConfigError> {
        let entry = self
            .config
            .entries()
            .get(occurrence)
            .ok_or_else(invalid_index)?;
        let mut replacement = Vec::new();
        if entry.value.is_none() {
            replacement.extend_from_slice(b" = ");
        }
        replacement.extend(quote(value, false)?);
        self.apply(vec![(
            self.layout.entries[occurrence].value.clone(),
            replacement,
        )])
    }

    /// Removes one assignment, preserving neighboring comments and line endings.
    ///
    /// # Errors
    ///
    /// An invalid occurrence index leaves the document unchanged.
    pub fn remove(&mut self, occurrence: usize) -> Result<(), ConfigError> {
        let span = self
            .layout
            .entries
            .get(occurrence)
            .ok_or_else(invalid_index)?;
        self.apply(vec![(span.range.clone(), Vec::new())])
    }

    /// Appends a new section header and explicit assignment at EOF.
    ///
    /// Repeated sections are intentional: existing entries and include ordering stay intact. The
    /// new occurrence follows every existing occurrence in this file. Values are opaque bytes;
    /// typed validation belongs to their consumer.
    ///
    /// # Errors
    ///
    /// Invalid section/key names, NUL bytes or newlines in subsections leave the document
    /// unchanged.
    pub fn append(
        &mut self,
        section: &str,
        subsection: Option<&[u8]>,
        name: &str,
        value: &[u8],
    ) -> Result<(), ConfigError> {
        validate_variable_name(name)?;
        let Some(last) = self.layout.sections.iter().rposition(|s| {
            s.section.eq_ignore_ascii_case(section.as_bytes()) && s.subsection.as_deref() == subsection
        }) else {
            return self.append_section(section, subsection, &[(name, value)]);
        };
        let entries = self.section_at(last).entry_range();
        let anchor = match entries.last() {
            Some(entry) => self.layout.entries[entry].range.end,
            None => self.layout.sections[last].range.end,
        };
        let insert_at = self.bytes[anchor..]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(self.bytes.len(), |i| anchor + i + 1);
        let mut addition = Vec::new();
        if insert_at == self.bytes.len() && !self.bytes.ends_with(b"\n") {
            addition.push(b'\n');
        }
        addition.push(b'\t');
        addition.extend_from_slice(name.as_bytes());
        addition.extend_from_slice(b" = ");
        addition.extend(quote(value, false)?);
        addition.push(b'\n');
        let mut bytes = self.bytes.clone();
        bytes.splice(insert_at..insert_at, addition);
        *self = Self::parse(&bytes)?;
        Ok(())
    }

    /// Appends one section with ordered explicit assignments at EOF.
    ///
    /// Existing bytes and include ordering are preserved. Values use canonical quoting and LF;
    /// empty values remain explicit. An empty entry list appends an empty section. Like
    /// [`Self::append`], this creates a new header even if the section already exists.
    ///
    /// # Errors
    ///
    /// Invalid section/key names, NUL values or newlines in subsections leave the entire document
    /// unchanged, including when a later assignment is invalid.
    pub fn append_section(
        &mut self,
        section: &str,
        subsection: Option<&[u8]>,
        entries: &[(&str, &[u8])],
    ) -> Result<(), ConfigError> {
        let mut addition = header(section.as_bytes(), subsection)?;
        addition.push(b'\n');
        for &(name, value) in entries {
            validate_variable_name(name)?;
            addition.push(b'\t');
            addition.extend_from_slice(name.as_bytes());
            addition.extend_from_slice(b" = ");
            addition.extend(quote(value, false)?);
            addition.push(b'\n');
        }
        let mut bytes = self.bytes.clone();
        // Git starts a new header on its own line. A trailing backslash-newline continuation
        // additionally needs a blank line to terminate it.
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        if bytes.ends_with(b"\\\n") || bytes.ends_with(b"\\\r\n") {
            bytes.push(b'\n');
        }
        bytes.extend(addition);
        *self = Self::parse(&bytes)?;
        Ok(())
    }

    /// Whether a matching header exists, including empty or deprecated dotted sections.
    pub fn contains_section(&self, section: &str, subsection: Option<&[u8]>) -> bool {
        self.layout.sections.iter().any(|s| {
            s.section.eq_ignore_ascii_case(section.as_bytes())
                && s.subsection.as_deref() == subsection
        })
    }

    /// Renames every matching section header without changing its entries or comments.
    ///
    /// # Errors
    ///
    /// Invalid destination subsection bytes fail without changing any header.
    pub fn rename_section(
        &mut self,
        section: &str,
        old: Option<&[u8]>,
        new: Option<&[u8]>,
    ) -> Result<(), ConfigError> {
        let replacement = header(section.as_bytes(), new)?;
        let edits = self
            .layout
            .sections
            .iter()
            .filter(|s| {
                s.section.eq_ignore_ascii_case(section.as_bytes()) && s.subsection.as_deref() == old
            })
            .map(|s| (s.range.clone(), replacement.clone()))
            .collect();
        self.apply(edits)
    }

    /// Removes matching headers and assignments, leaving comments and unrelated bytes intact.
    ///
    /// # Errors
    ///
    /// Returns a syntax error if the resulting document cannot be parsed; the original is retained.
    pub fn remove_section(
        &mut self,
        section: &str,
        subsection: Option<&[u8]>,
    ) -> Result<(), ConfigError> {
        let ordinals: Vec<_> = self
            .sections()
            .filter(|physical| {
                physical.name().eq_ignore_ascii_case(section.as_bytes())
                    && physical.subsection() == subsection
            })
            .map(|physical| physical.ordinal())
            .collect();
        self.remove_sections(&ordinals)
    }

    fn apply(&mut self, edits: Vec<(Range<usize>, Vec<u8>)>) -> Result<(), ConfigError> {
        // A removal that leaves only whitespace on its line removes the whole line, as
        // `git config --unset` does; otherwise surrounding bytes are preserved.
        let mut edits: Vec<_> = edits
            .into_iter()
            .map(|(range, replacement)| {
                if replacement.is_empty() {
                    (whole_blank_line(&self.bytes, range), replacement)
                } else {
                    (range, replacement)
                }
            })
            .collect();
        edits.sort_by_key(|(range, _)| range.start);
        let mut merged: Vec<(Range<usize>, Vec<u8>)> = Vec::with_capacity(edits.len());
        for (range, replacement) in edits {
            if let Some((last, last_replacement)) = merged.last_mut()
                && range.start < last.end
                && replacement.is_empty()
                && last_replacement.is_empty()
            {
                last.end = last.end.max(range.end);
                continue;
            }
            merged.push((range, replacement));
        }
        let mut bytes = self.bytes.clone();
        for (range, replacement) in merged.into_iter().rev() {
            bytes.splice(range, replacement);
        }
        *self = Self::parse(&bytes)?;
        Ok(())
    }
}

fn header(section: &[u8], subsection: Option<&[u8]>) -> Result<Vec<u8>, ConfigError> {
    if section.is_empty()
        || !section
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
    {
        return Err(invalid("invalid section name"));
    }
    let mut bytes = vec![b'['];
    bytes.extend_from_slice(section);
    if let Some(subsection) = subsection {
        bytes.push(b' ');
        bytes.extend(quote(subsection, true)?);
    }
    bytes.push(b']');
    Ok(bytes)
}
fn validate_variable_name(name: &str) -> Result<(), ConfigError> {
    if !name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(invalid("invalid variable name"));
    }
    Ok(())
}

/// Extends a removed span to its whole physical line when nothing but whitespace would remain.
fn whole_blank_line(bytes: &[u8], range: Range<usize>) -> Range<usize> {
    let line_start = bytes[..range.start]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i + 1);
    let line_end = bytes[range.end..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(bytes.len(), |i| range.end + i + 1);
    let is_blank = |part: &[u8]| part.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'));
    if is_blank(&bytes[line_start..range.start]) && is_blank(&bytes[range.end..line_end]) {
        line_start..line_end
    } else {
        range
    }
}

/// Encodes a value the way Git writes it: bare unless quoting is needed to preserve leading or
/// trailing whitespace or comment characters. Subsections are always quoted.
fn quote(value: &[u8], subsection: bool) -> Result<Vec<u8>, ConfigError> {
    let needs_quotes = subsection
        || value.first().is_some_and(|b| matches!(b, b' ' | b'\t'))
        || value.last().is_some_and(|b| matches!(b, b' ' | b'\t'))
        || value.iter().any(|b| matches!(b, b';' | b'#'));
    let mut bytes = Vec::with_capacity(value.len() + 2);
    if needs_quotes {
        bytes.push(b'"');
    }
    for &byte in value {
        match byte {
            0 => return Err(invalid("NUL in value")),
            b'\n' if subsection => return Err(invalid("newline in subsection")),
            b'"' | b'\\' => {
                bytes.push(b'\\');
                bytes.push(byte);
            }
            b'\n' => bytes.extend_from_slice(b"\\n"),
            b'\t' if !subsection => bytes.extend_from_slice(b"\\t"),
            8 if !subsection => bytes.extend_from_slice(b"\\b"),
            byte => bytes.push(byte),
        }
    }
    if needs_quotes {
        bytes.push(b'"');
    }
    Ok(bytes)
}
fn invalid(reason: &'static str) -> ConfigError {
    ConfigError { line: 1, reason }
}
fn invalid_index() -> ConfigError {
    invalid("invalid occurrence index")
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn physical_sections_include_empty_repeated_and_deprecated_headers() {
        let document = Document::parse(b"[Remote \"x\"]\na=one\nb\n[remote]\n[remote.X]\nc=three\n[remote \"X\"]\n[remote \"x\"]\nd=four\n[last]").unwrap();
        let sections: Vec<_> = document.sections().collect();
        assert_eq!(document.sections().len(), 6);
        assert_eq!(
            sections
                .iter()
                .map(DocumentSection::ordinal)
                .collect::<Vec<_>>(),
            [0, 1, 2, 3, 4, 5]
        );
        assert_eq!(sections[0].name(), b"Remote");
        assert_eq!(sections[1].subsection(), None);
        assert_eq!(sections[2].subsection(), Some(b"x".as_slice()));
        assert_eq!(sections[3].subsection(), Some(b"X".as_slice()));
        assert_eq!(
            sections
                .iter()
                .map(DocumentSection::entry_range)
                .collect::<Vec<_>>(),
            [0..2, 2..2, 2..3, 3..3, 3..4, 4..4]
        );
        let entries = &document.config().entries()[sections[0].entry_range()];
        assert_eq!(entries[0].value.as_deref(), Some(b"one".as_slice()));
        assert_eq!(entries[1].value, None);
    }

    #[test]
    fn selective_removal_preserves_same_named_neighbor_and_all_comments() {
        let original = b"\xef\xbb\xbf[remote \"x\"] #header\r\nurl=\"a#b\" #url\r\nimplicit ;bool\r\nmulti=one\\\r\n two #tail\r\n[other]\r\nx=yes\r\n[remote \"x\"]\r\nurl=second\r\n";
        let mut document = Document::parse(original).unwrap();
        assert_eq!(document.sections().next().unwrap().entry_range(), 0..3);
        document.remove_sections(&[0, 0]).unwrap();
        assert_eq!(document.as_bytes(), b"\xef\xbb\xbf #header\r\n #url\r\n ;bool\r\n #tail\r\n[other]\r\nx=yes\r\n[remote \"x\"]\r\nurl=second\r\n");
        assert_eq!(
            document.config().value("remote", Some(b"x"), "url"),
            Some(Some(b"second".as_slice()))
        );
        assert_eq!(document.sections().nth(1).unwrap().entry_range(), 1..2);
    }

    #[test]
    fn removal_deduplicates_unsorted_ordinals_and_removes_empty_headers() {
        let mut document =
            Document::parse(b"[same]a=one\n[same]#empty\n[keep]x=yes\n[same]a=last").unwrap();
        document.remove_sections(&[3, 0, 1, 3]).unwrap();
        assert_eq!(document.as_bytes(), b"\n#empty\n[keep]x=yes\n");
        assert_eq!(document.sections().len(), 1);
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::comments(b"\xef\xbb\xbf # only\r\n")]
    #[case::sections(b"[x]\r\na=\"two\\nlines\" ;comment\r\n[x]\r\n")]
    fn empty_section_selection_is_exact_noop(#[case] bytes: &[u8]) {
        let mut document = Document::parse(bytes).unwrap();
        document.remove_sections(&[]).unwrap();
        assert_eq!(document.as_bytes(), bytes);
    }

    #[rstest]
    #[case(&[0, 2][..])]
    #[case(&[usize::MAX, 0][..])]
    fn invalid_section_ordinal_is_atomic(#[case] ordinals: &[usize]) {
        let bytes = b"[x]a=one\n[x]a=two\n";
        let mut document = Document::parse(bytes).unwrap();
        assert!(document.remove_sections(ordinals).is_err());
        assert_eq!(document.as_bytes(), bytes);
        assert_eq!(document.sections().len(), 2);
        assert_eq!(document.config().entries().len(), 2);
    }

    #[rstest]
    #[case::comments(
        b"# before\r\n[CoRe] x = old  ; keep\r\n# after\r\n",
        b"# before\r\n[CoRe] x = new  ; keep\r\n# after\r\n"
    )]
    #[case::implicit(b"[core]\nx # keep\n", b"[core]\nx = new # keep\n")]
    #[case::empty(b"[core]\nx=  # keep", b"[core]\nx=  new# keep")]
    #[case::continued(b"[core]\nx=ab\\\n cd\n", b"[core]\nx=new\n")]
    #[case::bom(b"\xef\xbb\xbf[core]\nx=old", b"\xef\xbb\xbf[core]\nx=new")]
    fn replaces_only_value(#[case] input: &[u8], #[case] expected: &[u8]) {
        let mut document = Document::parse(input).unwrap();
        document.set_value(0, b"new").unwrap();
        assert_eq!(document.as_bytes(), expected);
    }

    #[test]
    fn preserves_repeated_sections_and_unrelated_bytes() {
        let bytes = b";top\n[remote.OLD] url=one ;first\n[other]\n  x = yes\n[remote \"old\"]\nurl=two #second\n";
        let mut document = Document::parse(bytes).unwrap();
        document
            .rename_section("remote", Some(b"old"), Some(b"n\"\\\xff"))
            .unwrap();
        assert_eq!(
            document
                .config()
                .values("remote", Some(b"n\"\\\xff"), "url")
                .collect::<Vec<_>>(),
            vec![Some(b"one".as_slice()), Some(b"two".as_slice())]
        );
        document
            .remove_section("remote", Some(b"n\"\\\xff"))
            .unwrap();
        assert_eq!(
            document.as_bytes(),
            b";top\n  ;first\n[other]\n  x = yes\n #second\n"
        );
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::no_newline(b"[core]\nx=yes")]
    #[case::comment(b"#tail")]
    #[case::trailing_continuation(b"[core]\nx=yes\\\n")]
    fn append_escapes_values(#[case] input: &[u8]) {
        let mut document = Document::parse(input).unwrap();
        document
            .append("remote", Some(b"a\"\\\xff"), "url", b" \n\t\x08\"\\#;\xff ")
            .unwrap();
        assert_eq!(
            document.config().value("remote", Some(b"a\"\\\xff"), "url"),
            Some(Some(b" \n\t\x08\"\\#;\xff ".as_slice()))
        );
        assert!(document.as_bytes().starts_with(input));
    }

    #[test]
    fn append_section_preserves_order_and_explicit_empty_values() {
        let before = b"# keep\r\n[remote \"origin\"]\r\nurl=old\r\nimplicit\r\n";
        let mut document = Document::parse(before).unwrap();
        document
            .append_section(
                "remote",
                Some(b"origin"),
                &[
                    ("url", b""),
                    ("url", b"new"),
                    ("fetch", b"+refs/heads/*:refs/remotes/origin/*"),
                ],
            )
            .unwrap();
        assert!(document.as_bytes().starts_with(before));
        assert_eq!(
            document
                .config()
                .values("remote", Some(b"origin"), "url")
                .collect::<Vec<_>>(),
            [
                Some(b"old".as_slice()),
                Some(b"".as_slice()),
                Some(b"new".as_slice())
            ]
        );
        assert_eq!(
            document
                .config()
                .value("remote", Some(b"origin"), "implicit"),
            Some(None)
        );
        assert_eq!(&document.as_bytes()[before.len()..], b"[remote \"origin\"]\n\turl = \n\turl = new\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n");
    }

    #[rstest]
    #[case::key("bad=name", b"value")]
    #[case::value("url", b"bad\0value")]
    fn append_section_invalid_later_entry_retains_bytes(#[case] name: &str, #[case] value: &[u8]) {
        let before = b"[core]\nx=yes\n";
        let mut document = Document::parse(before).unwrap();
        assert!(
            document
                .append_section(
                    "remote",
                    Some(b"origin"),
                    &[("url", b"valid"), (name, value)]
                )
                .is_err()
        );
        assert_eq!(document.as_bytes(), before);
    }

    #[test]
    fn append_empty_section() {
        let mut document = Document::parse(b"").unwrap();
        document
            .append_section("remote", Some(b"origin"), &[])
            .unwrap();
        assert_eq!(document.as_bytes(), b"[remote \"origin\"]\n");
        assert!(document.config().entries().is_empty());
    }

    #[test]
    fn invalid_edits_retain_exact_bytes() {
        let mut document = Document::parse(b"[core]\nx=yes\n").unwrap();
        let before = document.as_bytes().to_vec();
        assert!(document.set_value(0, b"\0").is_err());
        assert!(document.set_value(1, b"x").is_err());
        assert!(document.append("core", None, "x=y\ny", b"x").is_err());
        assert!(
            document
                .rename_section("core", None, Some(b"x\ny"))
                .is_err()
        );
        assert_eq!(document.as_bytes(), before);
    }
}
