//! Construction from already interpreted configuration, without source discovery.

use super::parse::{SectionName, SectionOccurrence};
use super::{Config, ConfigError, Entry, ResolveLimits};

/// One already decoded variable occurrence.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DecodedEntry {
    /// ASCII variable name: starts with a letter, followed by letters, digits or `-`.
    pub name: Vec<u8>,

    /// Uninterpreted decoded bytes, including NUL; `None` denotes an implicit assignment.
    pub value: Option<Vec<u8>>,
}

/// One already decoded section, retaining its position and variable order.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DecodedSection {
    /// Nonempty ASCII section name containing only letters, digits or `-`.
    pub name: Vec<u8>,

    /// Exact decoded bytes, including NUL; absence differs from an empty subsection.
    pub subsection: Option<Vec<u8>>,

    /// Variables in encounter order. Empty sections and duplicate names are retained.
    pub entries: Vec<DecodedEntry>,
}

impl Config {
    /// Constructs a snapshot from ordered decoded sections without parsing or I/O.
    ///
    /// Names retain their spelling; subsections and values retain every supplied byte, including
    /// NUL. Implicit assignments remain distinct from explicit empty values. No includes, trust
    /// decisions, source discovery or value interpretation are performed. Origins are absent and
    /// entry lines are zero because decoded input has no physical source location. Construction
    /// returns either the complete snapshot or an error, without changing another configuration.
    ///
    /// `limits.entries` bounds variable occurrences. `limits.bytes` independently bounds expanded
    /// entry bytes (section, subsection, variable name and value) and section metadata (128 per
    /// header plus its name/subsection lengths, and 8 per variable). These are the same logical
    /// budgets used by resolution, not allocator-size estimates. `limits.depth` and
    /// `limits.match_cells` are unused.
    ///
    /// # Errors
    ///
    /// Rejects invalid section/variable names, arithmetic overflow or exhausted budgets. Error
    /// `line` identifies the one-based input section ordinal, not a physical source line. Decoded
    /// subsections and values are not checked for Git syntax or suitability for a consumer.
    ///
    /// ```
    /// use girt::Config;
    /// use girt::config::{DecodedEntry, DecodedSection, ResolveLimits};
    /// let config = Config::from_decoded_sections(
    ///     vec![DecodedSection {
    ///         name: b"remote".to_vec(),
    ///         subsection: Some(b"origin".to_vec()),
    ///         entries: vec![DecodedEntry {
    ///             name: b"url".to_vec(),
    ///             value: None,
    ///         }],
    ///     }],
    ///     ResolveLimits::default(),
    /// )?;
    /// assert_eq!(config.value("remote", Some(b"origin"), "url"), Some(None));
    /// # Ok::<(), girt::ConfigError>(())
    /// ```
    pub fn from_decoded_sections(
        sections: Vec<DecodedSection>,
        limits: ResolveLimits,
    ) -> Result<Self, ConfigError> {
        let mut config = Self {
            entries: Vec::new(),
            sections: Vec::new(),
            occurrences: Vec::new(),
        };
        let mut metadata_bytes = 0usize;
        let mut expanded_bytes = 0usize;
        for (ordinal, section) in sections.into_iter().enumerate() {
            let error = |reason| ConfigError {
                line: ordinal + 1,
                reason,
            };
            if !valid_name(&section.name) {
                return Err(error("invalid decoded section name"));
            }
            let name = SectionName {
                section: section.name,
                subsection: section.subsection,
            };
            let header_bytes = name
                .section
                .len()
                .checked_add(name.subsection.as_ref().map_or(0, Vec::len))
                .ok_or_else(|| error("section metadata bytes"))?;
            metadata_bytes = metadata_bytes
                .checked_add(128)
                .and_then(|n| n.checked_add(header_bytes))
                .filter(|n| *n <= limits.bytes)
                .ok_or_else(|| error("section metadata bytes"))?;
            let start = config.entries.len();
            for entry in section.entries {
                if !valid_name(&entry.name) || !entry.name[0].is_ascii_alphabetic() {
                    return Err(error("invalid decoded variable name"));
                }
                if config.entries.len() >= limits.entries {
                    return Err(error("entry occurrences"));
                }
                metadata_bytes = metadata_bytes
                    .checked_add(8)
                    .filter(|n| *n <= limits.bytes)
                    .ok_or_else(|| error("section metadata bytes"))?;
                expanded_bytes = expanded_bytes
                    .checked_add(header_bytes)
                    .and_then(|n| n.checked_add(entry.name.len()))
                    .and_then(|n| n.checked_add(entry.value.as_ref().map_or(0, Vec::len)))
                    .filter(|n| *n <= limits.bytes)
                    .ok_or_else(|| error("expanded bytes"))?;
                config.entries.push(Entry {
                    line: 0,
                    origin: None,
                    section: name.section.clone(),
                    subsection: name.subsection.clone(),
                    name: entry.name,
                    value: entry.value,
                });
            }
            config.occurrences.push(SectionOccurrence {
                name: name.clone(),
                start,
                entries: (start..config.entries.len()).collect(),
            });
            config.sections.push(name);
        }
        Ok(config)
    }
}

fn valid_name(name: &[u8]) -> bool {
    !name.is_empty() && name.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn section(name: &[u8], entries: Vec<DecodedEntry>) -> DecodedSection {
        DecodedSection {
            name: name.to_vec(),
            subsection: None,
            entries,
        }
    }
    fn entry(name: &[u8], value: Option<&[u8]>) -> DecodedEntry {
        DecodedEntry {
            name: name.to_vec(),
            value: value.map(Vec::from),
        }
    }

    #[test]
    fn preserves_decoded_bytes_membership_and_implicit_values() {
        let mut first = section(
            b"ReMoTe",
            vec![
                entry(b"URL", None),
                entry(b"url", Some(b"")),
                entry(b"url", Some(b" \xff\0\n ")),
            ],
        );
        first.subsection = Some(b"Old\0\xff".to_vec());
        let config = Config::from_decoded_sections(
            vec![
                first,
                section(b"remote", vec![]),
                section(b"remote", vec![entry(b"url", Some(b"last"))]),
            ],
            ResolveLimits::default(),
        )
        .unwrap();
        assert_eq!(
            config
                .values("remote", Some(b"Old\0\xff"), "url")
                .collect::<Vec<_>>(),
            vec![None, Some(b"".as_slice()), Some(b" \xff\0\n ".as_slice())]
        );
        let sections: Vec<_> = config.section_occurrences().collect();
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].name(), b"ReMoTe");
        assert_eq!(sections[0].entry_indices(), &[0, 1, 2]);
        assert!(sections[1].entry_indices().is_empty());
        assert_eq!(sections[2].entry_indices(), &[3]);
        assert!(
            config
                .entries()
                .iter()
                .all(|e| e.origin.is_none() && e.line == 0)
        );
    }

    #[rstest]
    #[case(b"", b"x")]
    #[case(b"a.b", b"x")]
    #[case(b"a\0", b"x")]
    #[case(b"a", b"")]
    #[case(b"a", b"1x")]
    #[case(b"a", b"x_y")]
    fn rejects_invalid_names(#[case] name: &[u8], #[case] key: &[u8]) {
        let result = Config::from_decoded_sections(
            vec![
                section(b"valid", vec![]),
                section(name, vec![entry(key, None)]),
            ],
            ResolveLimits::default(),
        );
        assert_eq!(result.unwrap_err().line, 2);
    }

    #[rstest]
    #[case::metadata_exact(137, 1, 0, true)]
    #[case::metadata_short(136, 1, 0, false)]
    #[case::entries_short(137, 0, 0, false)]
    #[case::expanded_exact(137, 1, 135, true)]
    #[case::expanded_short(137, 1, 136, false)]
    fn enforces_independent_budgets(
        #[case] bytes: usize,
        #[case] entries: usize,
        #[case] value_len: usize,
        #[case] accepted: bool,
    ) {
        let limits = ResolveLimits {
            bytes,
            entries,
            depth: 0,
            ..ResolveLimits::default()
        };
        let result = Config::from_decoded_sections(
            vec![section(
                b"a",
                vec![entry(b"b", Some(&vec![b'x'; value_len]))],
            )],
            limits,
        );
        assert_eq!(result.is_ok(), accepted);
    }

    #[test]
    fn empty_headers_consume_metadata_without_entries() {
        let limits = ResolveLimits {
            bytes: 257,
            entries: 0,
            depth: 0,
            ..ResolveLimits::default()
        };
        let error = Config::from_decoded_sections(
            vec![section(b"a", vec![]), section(b"a", vec![])],
            limits,
        )
        .unwrap_err();
        assert_eq!(error.line, 2);
        assert_eq!(error.reason, "section metadata bytes");
    }

    #[test]
    fn decoded_include_remains_inert() {
        let config = Config::from_decoded_sections(
            vec![section(
                b"include",
                vec![entry(b"path", Some(b"/missing/should-not-be-read"))],
            )],
            ResolveLimits::default(),
        )
        .unwrap();
        assert_eq!(config.entries().len(), 1);
        assert_eq!(
            config.value("include", None, "path"),
            Some(Some(b"/missing/should-not-be-read".as_slice()))
        );
    }

    #[test]
    fn preserves_empty_subsection_and_accepts_empty_input_at_zero_budget() {
        let empty = Config::from_decoded_sections(
            vec![],
            ResolveLimits {
                bytes: 0,
                entries: 0,
                ..ResolveLimits::default()
            },
        )
        .unwrap();
        assert_eq!(empty.section_occurrences().len(), 0);
        let config = Config::from_decoded_sections(
            vec![
                DecodedSection {
                    name: b"remote".to_vec(),
                    subsection: Some(vec![]),
                    entries: vec![],
                },
                section(b"remote", vec![]),
            ],
            ResolveLimits::default(),
        )
        .unwrap();
        assert_eq!(config.subsection_names("remote"), vec![b"".as_slice()]);
        assert_eq!(
            config.section_occurrences().next().unwrap().subsection(),
            Some(b"".as_slice())
        );
    }
}
