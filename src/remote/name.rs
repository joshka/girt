use std::fmt;

/// Validates a remote name for use within a fetch tracking-reference namespace.
///
/// A name must permit a full reference such as `refs/remotes/<name>/branch`. Slashes and
/// non-UTF-8 bytes are accepted; empty names, invalid reference components and wildcard patterns
/// are rejected. A trailing dot is allowed because the name is followed by another component.
/// No bytes are normalized. This does not check config-section representability, filesystem
/// restrictions, existing remotes, or application-reserved names.
///
/// # Errors
///
/// Returns the original bytes and a structured reason. Diagnostics follow an embedded fetch
/// mapping: a colon splits that mapping before reference validation; multiple wildcards in the
/// first checked side precede other errors. Otherwise reference errors follow byte order, before
/// a remaining single wildcard. Diagnostics escape invalid UTF-8 without losing original bytes.
///
/// ```
/// use girt::remote::validate_name;
/// validate_name(b"upstream")?;
/// validate_name(b"team/fork.")?;
/// assert!(validate_name(b"my remote").is_err());
/// # Ok::<(), girt::remote::InvalidRemoteName>(())
/// ```
pub fn validate_name(name: &[u8]) -> Result<(), InvalidRemoteName> {
    validate(name).map_err(|source| InvalidRemoteName {
        name: name.to_vec(),
        source,
    })
}

/// A remote name cannot be used within a fetch tracking-reference namespace.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("remote names must be valid within refspecs for fetching: {}", Quoted(.name))]
pub struct InvalidRemoteName {
    /// Original name bytes, without normalization.
    pub name: Vec<u8>,
    /// The syntax failure, before any application-specific policy is applied.
    #[source]
    pub source: RemoteNameError,
}

/// Syntax preventing a remote name from being embedded in a tracking reference.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RemoteNameError {
    /// A colon leaves the source side of the embedded fetch mapping ending in a slash.
    #[error("Reference name cannot end with a slash")]
    EndsWithSlash,
    /// An empty component, including an empty name or an edge slash.
    #[error("Reference name cannot contain repeated slashes")]
    RepeatedSlash,
    /// A component starts with a dot.
    #[error("Reference name cannot start with a dot")]
    StartsWithDot,
    /// A component ends with `.lock`.
    #[error("Reference name cannot end with '.lock'")]
    LockFileSuffix,
    /// Adjacent dots occur within a component.
    #[error("Reference name cannot contain repeated dots")]
    RepeatedDot,
    /// A reflog selector occurs within the name.
    #[error("Reference name cannot contain '@{{'")]
    ReflogPortion,
    /// A byte forbidden in reference names.
    #[error("Reference name contains invalid byte: {}", Quoted(&[*byte]))]
    InvalidByte {
        /// The first forbidden byte.
        byte: u8,
    },
    /// One wildcard would introduce an unmatched fetch mapping pattern.
    #[error("Both sides of a two-sided specification need a pattern, like 'a/*:b/*'")]
    PatternUnbalanced,
    /// More than one wildcard occurs in the tracking-reference pattern.
    #[error("refspec patterns may only contain a single '*' character, found {}", Quoted(.pattern))]
    PatternUnsupported {
        /// The invalid tracking-reference pattern used to diagnose the name.
        pattern: Vec<u8>,
    },
}

fn validate(name: &[u8]) -> Result<(), RemoteNameError> {
    // Public validator probes establish this diagnostic precedence for an embedded mapping
    // `refs/heads/test:refs/remotes/<name>/test`. A colon is always invalid, but the last colon
    // changes the first checked side; preserve that ordering without parsing general refspecs.
    if let Some(colon) = name.iter().rposition(|&byte| byte == b':') {
        let prefix = &name[..colon];
        if prefix.iter().filter(|&&byte| byte == b'*').count() > 1 {
            return Err(RemoteNameError::PatternUnsupported {
                pattern: [b"refs/heads/test:refs/remotes/", prefix].concat(),
            });
        }
        if prefix.is_empty() || prefix.ends_with(b"/") {
            return Err(RemoteNameError::EndsWithSlash);
        }
        return Err(RemoteNameError::InvalidByte { byte: b':' });
    }
    let stars = name.iter().filter(|&&byte| byte == b'*').count();
    if stars > 1 {
        return Err(RemoteNameError::PatternUnsupported {
            pattern: [b"refs/remotes/", name, b"/test"].concat(),
        });
    }
    for component in name.split(|&byte| byte == b'/') {
        if component.is_empty() {
            return Err(RemoteNameError::RepeatedSlash);
        }
        if component.starts_with(b".") {
            return Err(RemoteNameError::StartsWithDot);
        }
        for (index, &byte) in component.iter().enumerate() {
            if byte <= b' ' || byte == 127 || b"~^:?[\\".contains(&byte) {
                return Err(RemoteNameError::InvalidByte { byte });
            }
            if component[index..].starts_with(b"..") {
                return Err(RemoteNameError::RepeatedDot);
            }
            if component[index..].starts_with(b"@{") {
                return Err(RemoteNameError::ReflogPortion);
            }
        }
        if component.ends_with(b".lock") {
            return Err(RemoteNameError::LockFileSuffix);
        }
    }
    if stars == 1 {
        return Err(RemoteNameError::PatternUnbalanced);
    }
    Ok(())
}

// Keep original bytes recognizable in diagnostics, including malformed UTF-8 and control bytes.
struct Quoted<'a>(&'a [u8]);

impl fmt::Display for Quoted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"")?;
        for chunk in self.0.utf8_chunks() {
            for character in chunk.valid().chars() {
                match character {
                    '\0' => f.write_str("\\0")?,
                    '\t' => f.write_str("\\t")?,
                    '\r' => f.write_str("\\r")?,
                    '\n' => f.write_str("\\n")?,
                    '\x01'..='\x1f' | '\x7f' => write!(f, "\\x{:02x}", u32::from(character))?,
                    character => write!(f, "{}", character.escape_debug())?,
                }
            }
            for byte in chunk.invalid() {
                write!(f, "\\x{byte:02x}")?;
            }
        }
        f.write_str("\"")
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    // Original fixtures from installed Git remote-add observations and public API probes.
    #[rstest]
    #[case::ordinary(b"origin")]
    #[case::application_reserved(b"git")]
    #[case::slash(b"team/fork")]
    #[case::trailing_dot(b"fork.")]
    #[case::dot_before_slash(b"a./b")]
    #[case::at(b"@")]
    #[case::leading_dash(b"-fork")]
    #[case::double_quote(b"a\"b")]
    #[case::apostrophe(b"a'b")]
    #[case::closing_bracket(b"a]b")]
    #[case::non_utf8(b"a\xffb")]
    #[case::unicode("é".as_bytes())]
    #[case::unicode_control("a\u{85}b".as_bytes())]
    fn accepts_names(#[case] name: &[u8]) {
        assert_eq!(validate_name(name), Ok(()));
    }

    #[rstest]
    #[case::colon(b":", RemoteNameError::EndsWithSlash)]
    #[case::colon_after_slash(b"a/:x", RemoteNameError::EndsWithSlash)]
    #[case::colon_before_stars(b"a:x**", RemoteNameError::InvalidByte { byte: b':' })]
    #[case::colon_before_space(b"a :x", RemoteNameError::InvalidByte { byte: b':' })]
    #[case::last_colon(b"a/:x:", RemoteNameError::InvalidByte { byte: b':' })]
    #[case::empty(b"", RemoteNameError::RepeatedSlash)]
    #[case::leading_slash(b"/a", RemoteNameError::RepeatedSlash)]
    #[case::trailing_slash(b"a/", RemoteNameError::RepeatedSlash)]
    #[case::repeated_slash(b"a//b", RemoteNameError::RepeatedSlash)]
    #[case::leading_dot(b".a", RemoteNameError::StartsWithDot)]
    #[case::nested_leading_dot(b"a/.b", RemoteNameError::StartsWithDot)]
    #[case::nested_lock_suffix(b"a.lock/b", RemoteNameError::LockFileSuffix)]
    #[case::lock_suffix(b"a.lock", RemoteNameError::LockFileSuffix)]
    #[case::repeated_dot(b"a..b", RemoteNameError::RepeatedDot)]
    #[case::reflog(b"a@{b", RemoteNameError::ReflogPortion)]
    #[case::wildcard(b"a*b", RemoteNameError::PatternUnbalanced)]
    #[case::space_before_wildcard_error(b"a*b c", RemoteNameError::InvalidByte { byte: b' ' })]
    #[case::lock_before_wildcard_error(b"a*b.lock", RemoteNameError::LockFileSuffix)]
    #[case::lock_before_space(b"a.lock/ ", RemoteNameError::LockFileSuffix)]
    #[case::dot_before_space(b". a", RemoteNameError::StartsWithDot)]
    #[case::first_invalid_byte(b"a \n", RemoteNameError::InvalidByte { byte: b' ' })]
    #[case::dots_before_space(b"a.. b", RemoteNameError::RepeatedDot)]
    #[case::reflog_before_dots(b"a@{..", RemoteNameError::ReflogPortion)]
    fn rejects_names_in_diagnostic_order(#[case] name: &[u8], #[case] source: RemoteNameError) {
        assert_eq!(
            validate_name(name),
            Err(InvalidRemoteName {
                name: name.to_vec(),
                source
            })
        );
    }

    #[test]
    fn colon_changes_the_first_checked_pattern() {
        assert_eq!(
            validate_name(b"a**:x").unwrap_err().source,
            RemoteNameError::PatternUnsupported {
                pattern: b"refs/heads/test:refs/remotes/a**".to_vec(),
            }
        );
    }

    #[test]
    fn multiple_wildcards_precede_reference_errors() {
        assert_eq!(
            validate_name(b". ** ").unwrap_err().source,
            RemoteNameError::PatternUnsupported {
                pattern: b"refs/remotes/. ** /test".to_vec()
            }
        );
    }

    #[rstest]
    #[case::nul(0)]
    #[case::control_start(1)]
    #[case::control_end(31)]
    #[case::space(b' ')]
    #[case::delete(127)]
    #[case::tilde(b'~')]
    #[case::caret(b'^')]
    #[case::colon(b':')]
    #[case::question(b'?')]
    #[case::bracket(b'[')]
    #[case::backslash(b'\\')]
    fn rejects_forbidden_bytes(#[case] byte: u8) {
        assert_eq!(
            validate_name(&[b'a', byte, b'b']).unwrap_err().source,
            RemoteNameError::InvalidByte { byte }
        );
    }

    #[test]
    fn wildcard_diagnostic_preserves_non_utf8() {
        let error = validate_name(b"a**\xff").unwrap_err();
        assert_eq!(
            error.source.to_string(),
            "refspec patterns may only contain a single '*' character, found \"refs/remotes/a**\\xff/test\""
        );
    }

    #[test]
    fn diagnostic_chain_preserves_bytes() {
        use std::error::Error as _;
        let error = validate_name(b"a\xff\0").unwrap_err();
        assert_eq!(
            error.to_string(),
            "remote names must be valid within refspecs for fetching: \"a\\xff\\0\""
        );
        assert_eq!(
            error.source().unwrap().to_string(),
            "Reference name contains invalid byte: \"\\0\""
        );
    }
}
