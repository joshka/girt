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
        let mut addition = header(section.as_bytes(), subsection)?;
        addition.extend_from_slice(b"\n\t");
        addition.extend_from_slice(name.as_bytes());
        addition.extend_from_slice(b" = ");
        addition.extend(quote(value, false)?);
        addition.push(b'\n');
        // Parse the fragment independently so a name cannot inject additional syntax.
        let parsed = Config::parse(&addition)?;
        if parsed.entries().len() != 1 || parsed.entries()[0].name != name.as_bytes() {
            return Err(invalid("invalid variable name"));
        }
        let mut bytes = self.bytes.clone();
        // A blank separator also terminates a trailing backslash-newline continuation.
        if !bytes.is_empty() {
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
        let mut edits: Vec<_> = self
            .layout
            .sections
            .iter()
            .filter(|s| {
                s.section.eq_ignore_ascii_case(section.as_bytes())
                    && s.subsection.as_deref() == subsection
            })
            .map(|s| (s.range.clone(), Vec::new()))
            .collect();
        edits.extend(
            self.config
                .entries()
                .iter()
                .zip(&self.layout.entries)
                .filter(|(e, _)| {
                    e.section.eq_ignore_ascii_case(section.as_bytes())
                        && e.subsection.as_deref() == subsection
                })
                .map(|(_, s)| (s.range.clone(), Vec::new())),
        );
        self.apply(edits)
    }

    fn apply(&mut self, mut edits: Vec<(Range<usize>, Vec<u8>)>) -> Result<(), ConfigError> {
        edits.sort_by_key(|(range, _)| range.start);
        let mut bytes = self.bytes.clone();
        for (range, replacement) in edits.into_iter().rev() {
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
fn quote(value: &[u8], subsection: bool) -> Result<Vec<u8>, ConfigError> {
    let mut bytes = vec![b'"'];
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
    bytes.push(b'"');
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

    #[rstest]
    #[case::comments(
        b"# before\r\n[CoRe] x = old  ; keep\r\n# after\r\n",
        b"# before\r\n[CoRe] x = \"new\"  ; keep\r\n# after\r\n"
    )]
    #[case::implicit(b"[core]\nx # keep\n", b"[core]\nx = \"new\" # keep\n")]
    #[case::empty(b"[core]\nx=  # keep", b"[core]\nx=  \"new\"# keep")]
    #[case::continued(b"[core]\nx=ab\\\n cd\n", b"[core]\nx=\"new\"\n")]
    #[case::bom(b"\xef\xbb\xbf[core]\nx=old", b"\xef\xbb\xbf[core]\nx=\"new\"")]
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
            b";top\n  ;first\n[other]\n  x = yes\n\n #second\n"
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
