//! Bounded URL validation for configured remote snapshots, not transport admission.

/// A value-free configured URL validation diagnostic.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum ConfiguredUrlError {
    /// The supported URL form requires a repository path.
    #[error("remote URL has no repository path")]
    MissingPath,
    /// A scheme URL cannot be decoded as UTF-8.
    #[error("remote network URL is not UTF-8")]
    Encoding,
    /// The supported URL form has an empty or malformed host or user component.
    #[error("invalid remote URL authority")]
    Authority,
    /// An explicit numeric port must fit a nonzero u16.
    #[error("invalid remote URL port")]
    Port,
    /// A network path has malformed percent escapes or decodes to invalid UTF-8.
    #[error("invalid remote URL path escape")]
    PathEscape,
    /// URL syntax or normalization is outside the characterized subset.
    /// [`super::ConfiguredRemote`] reports this as its distinct unsupported error variant.
    #[error("unsupported configured URL syntax")]
    Unsupported,
}

/// Validates and serializes one configured destination, without applying URL rewrites.
///
/// Supports ordinary local byte paths, file URLs, scp-like SSH, and scheme-based network URLs.
/// Ordinary ASCII hosts are lowercased; percent-encoded host spelling is preserved. Numeric ports
/// lose leading zeroes; explicit default
/// ports and ordinary dot path segments are retained. File-host case is preserved. An HTTP(S)
/// URL without a path gains `/`. Local paths can contain non-UTF-8 bytes and spaces.
/// File URL paths also retain literal spaces, without encoding or trimming them.
/// Percent spelling and case in paths are retained. HTTP(S)/SSH path escapes must be complete hex
/// pairs whose decoded bytes form UTF-8; file/scp path percent sequences remain uninterpreted.
///
/// Unlike an empty value in [`super::ConfiguredRemoteRecord`], an empty destination is an error,
/// not a list reset. This function does not read configuration, apply fetch-to-push fallback,
/// authorize a transport, access the network or check whether a repository exists.
/// Returned bytes may contain private paths or credentials; do not log them by default.
///
/// Accepts IPv6, credentials, authority escapes, query/fragment components, custom schemes and
/// retained scheme case using the same pure normalization as [`super::canonicalize_user_url`].
/// Relative local paths and file paths remain unchanged: no filesystem access or canonicalization
/// occurs. Parsing a custom scheme does not authorize its transport.
///
/// # Errors
///
/// Returns a value-free diagnostic for malformed supported syntax. Helper syntax, control bytes,
/// Windows drive/UNC paths, bracketed scp hosts with an explicit user, and uncharacterized file
/// authorities return
/// [`ConfiguredUrlError::Unsupported`]. That result requests compatibility handling; it does not
/// establish that the destination is malformed.
///
/// ```
/// use girt::remote::{ConfiguredUrlError, normalize_configured_url};
/// assert_eq!(
///     normalize_configured_url(b"https://HOST:00443/repo")?,
///     b"https://host:443/repo"
/// );
/// assert_eq!(
///     normalize_configured_url(b"https://HOST/a%2fb")?,
///     b"https://host/a%2fb"
/// );
/// assert_eq!(
///     normalize_configured_url(b"file:///repo name ")?,
///     b"file:///repo name "
/// );
/// assert_eq!(
///     normalize_configured_url(b""),
///     Err(ConfiguredUrlError::MissingPath)
/// );
/// # Ok::<(), ConfiguredUrlError>(())
/// ```
pub fn normalize_configured_url(bytes: &[u8]) -> Result<Vec<u8>, ConfiguredUrlError> {
    normalize(bytes)
}

pub(super) fn normalize(bytes: &[u8]) -> Result<Vec<u8>, ConfiguredUrlError> {
    use ConfiguredUrlError as Error;
    if bytes.is_empty() {
        return Err(Error::MissingPath);
    }
    // Control-byte handling and Windows drive/UNC interpretation have separate contracts.
    if bytes.iter().any(|b| b.is_ascii_control()) || bytes.starts_with(b"\\") {
        return Err(Error::Unsupported);
    }
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        return Err(Error::Unsupported);
    }
    let colon = bytes.iter().position(|b| *b == b':');
    let slash = bytes.iter().position(|b| *b == b'/');
    if let Some(colon) = colon.filter(|colon| slash.is_none_or(|slash| *colon < slash)) {
        // Only an explicit user in the scp authority triggers this compatibility boundary;
        // the same bytes in a local path or scp repository path remain literal.
        if bytes.get(colon + 1..colon + 3) != Some(b"//")
            && bytes[..colon].windows(2).any(|part| part == b"@[")
        {
            return Err(Error::Unsupported);
        }
        if bytes.get(colon + 1) == Some(&b':')
            && bytes[..colon]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(b))
        {
            return Err(Error::Unsupported);
        }
        if bytes.starts_with(b"file://") {
            return file_url(bytes, 7);
        }
        if bytes.get(colon + 1..colon + 3) == Some(b"//") {
            std::str::from_utf8(bytes).map_err(|_| Error::Encoding)?;
        }
    }
    super::user_url::normalize(bytes).map_err(|error| {
        use super::UserUrlParseError as Parse;
        match error {
            Parse::EmptyPath | Parse::MissingUrlPath | Parse::MissingScpPath => Error::MissingPath,
            Parse::MissingHost | Parse::InvalidAuthority | Parse::InvalidScp => Error::Authority,
            Parse::InvalidPort => Error::Port,
            Parse::InvalidEscape | Parse::InvalidEncoding => Error::PathEscape,
            Parse::InvalidUrl => Error::Unsupported,
        }
    })
}

fn file_url(bytes: &[u8], start: usize) -> Result<Vec<u8>, ConfiguredUrlError> {
    let rest = &bytes[start..];
    if rest.is_empty() {
        return Err(ConfiguredUrlError::MissingPath);
    }
    let Some(slash) = rest.iter().position(|b| *b == b'/') else {
        return Err(ConfiguredUrlError::Unsupported);
    };
    let host = &rest[..slash];
    if !host
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return Err(ConfiguredUrlError::Unsupported);
    }
    // Literal file-path spaces are preserved; all other path checks remain shared.
    for part in rest[slash..].split(|byte| *byte == b' ') {
        plain_path(part)?;
    }
    // File authorities are preserved, unlike the network host case normalization.
    Ok(bytes.to_vec())
}

fn plain_path(path: &[u8]) -> Result<(), ConfiguredUrlError> {
    if path.contains(&b' ') {
        return Err(ConfiguredUrlError::Unsupported);
    }
    if !path.iter().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'~' | b'+' | b'%')
    }) {
        return Err(ConfiguredUrlError::Unsupported);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    // Original observations of the gix 0.87.1 public URL API on macOS; no upstream sources.
    #[rstest]
    #[case::local(b"../repo with spaces", b"../repo with spaces")]
    #[case::local_bytes(b"/repo/\xff", b"/repo/\xff")]
    #[case::local_colon(b"./repo:part", b"./repo:part")]
    #[case::file(b"file:///tmp/repo", b"file:///tmp/repo")]
    #[case::file_host(b"file://HOST/repo", b"file://HOST/repo")]
    #[case::scp(b"user@HOST:repo", b"user@host:repo")]
    #[case::ssh(b"ssh://user@HOST:00022/repo", b"ssh://user@host:22/repo")]
    #[case::default_port_retained(b"https://HOST:443/repo", b"https://host:443/repo")]
    #[case::http_path(b"https://HOST", b"https://host/")]
    #[case::numeric_host(b"https://127.1/repo", b"https://127.1/repo")]
    #[case::dot_segments(b"https://host/a/../repo", b"https://host/a/../repo")]
    #[case::empty_port(b"https://host:/repo", b"https://host:/repo")]
    fn canonical_values(#[case] input: &[u8], #[case] expected: &[u8]) {
        assert_eq!(
            crate::remote::normalize_configured_url(input).unwrap(),
            expected
        );
    }

    #[rstest]
    #[case::empty(b"", ConfiguredUrlError::MissingPath)]
    #[case::scp_empty(b"host:", ConfiguredUrlError::MissingPath)]
    #[case::file_empty(b"file://", ConfiguredUrlError::MissingPath)]
    #[case::ssh_empty(b"ssh://host", ConfiguredUrlError::MissingPath)]
    #[case::bad_port(b"http://host:bad/repo", ConfiguredUrlError::Port)]
    #[case::zero_port(b"http://host:0/repo", ConfiguredUrlError::Port)]
    #[case::large_port(b"http://host:65536/repo", ConfiguredUrlError::Port)]
    #[case::host_space(b"http://bad host/repo", ConfiguredUrlError::Authority)]
    #[case::user_without_host(b"http://user@/repo", ConfiguredUrlError::Authority)]
    #[case::empty_host(b"http:///repo", ConfiguredUrlError::Authority)]
    #[case::network_bytes(b"http://host/\xff", ConfiguredUrlError::Encoding)]
    #[case::helper(b"foo::repo", ConfiguredUrlError::Unsupported)]
    #[case::scp_bracket_user(b"user@[::1]:repo", ConfiguredUrlError::Unsupported)]
    fn failures(#[case] input: &[u8], #[case] error: ConfiguredUrlError) {
        assert_eq!(
            crate::remote::normalize_configured_url(input).unwrap_err(),
            error
        );
    }

    #[test]
    fn single_url_error_does_not_disclose_input() {
        let error =
            normalize_configured_url(b"https://user:private-password@host:bad/repo").unwrap_err();
        assert_eq!(error, ConfiguredUrlError::Port);
        assert!(!format!("{error:?}: {error}").contains("private-password"));
    }
    // Original public-gix 0.87.1 percent-path fixtures; serialized bytes remain encoded.
    #[rstest]
    #[case::space(b"https://HOST/a%20b", b"https://host/a%20b")]
    #[case::slash_lower(b"https://HOST/a%2fb", b"https://host/a%2fb")]
    #[case::slash_upper(b"https://HOST/a%2Fb", b"https://host/a%2Fb")]
    #[case::nul(b"https://HOST/a%00b", b"https://host/a%00b")]
    #[case::dot_segments(b"https://HOST/%2e%2e/repo", b"https://host/%2e%2e/repo")]
    #[case::question(b"https://HOST/a%3Fb", b"https://host/a%3Fb")]
    #[case::hash(b"https://HOST/a%23b", b"https://host/a%23b")]
    #[case::percent(b"http://HOST/%25/repo", b"http://host/%25/repo")]
    #[case::ssh_space(b"ssh://HOST/a%20b", b"ssh://host/a%20b")]
    #[case::ssh_slash(b"ssh://HOST/%2Frepo", b"ssh://host/%2Frepo")]
    #[case::file_space(b"file:///tmp/a%20b", b"file:///tmp/a%20b")]
    #[case::file_host(b"file://HOST/a%2Fb", b"file://HOST/a%2Fb")]
    #[case::file_unfinished(b"file:///tmp/a%", b"file:///tmp/a%")]
    #[case::scp_space(b"HOST:a%20b", b"host:a%20b")]
    #[case::scp_user(b"user@HOST:a%2fb", b"user@host:a%2fb")]
    #[case::scp_nonhex(b"HOST:a%GGb", b"host:a%GGb")]
    #[case::local(b"./local%20path", b"./local%20path")]
    #[case::local_invalid_utf8(b"/local/%FF", b"/local/%FF")]
    #[case::unicode(b"https://HOST/%C3%A9", b"https://host/%C3%A9")]
    #[case::newline(b"https://HOST/%0A", b"https://host/%0A")]
    #[case::delete(b"https://HOST/%7F", b"https://host/%7F")]
    #[case::backslash(b"https://HOST/%5C", b"https://host/%5C")]
    #[case::double_percent(b"https://HOST/%2520", b"https://host/%2520")]
    #[case::at(b"https://HOST/%40", b"https://host/%40")]
    #[case::euro(b"https://HOST/%E2%82%AC", b"https://host/%E2%82%AC")]
    fn percent_path_serialization(#[case] input: &[u8], #[case] expected: &[u8]) {
        assert_eq!(
            crate::remote::normalize_configured_url(input).unwrap(),
            expected
        );
    }

    #[rstest]
    #[case::invalid_utf8(b"https://HOST/a%FFb")]
    #[case::one_digit(b"https://HOST/a%b")]
    #[case::missing_pair(b"https://HOST/a%")]
    #[case::missing_digit(b"https://HOST/a%2")]
    #[case::nonhex(b"https://HOST/a%GGb")]
    #[case::ssh_nonhex(b"ssh://HOST/a%GGb")]
    #[case::overlong(b"https://HOST/%C0%AF")]
    #[case::surrogate(b"https://HOST/%ED%A0%80")]
    #[case::truncated_utf8(b"https://HOST/%C3")]
    fn invalid_network_path_escape_is_value_free(#[case] input: &[u8]) {
        let error = crate::remote::normalize_configured_url(input).unwrap_err();
        assert_eq!(error, ConfiguredUrlError::PathEscape);
        assert_eq!(error.to_string(), "invalid remote URL path escape");
        assert_eq!(format!("{error:?}"), "PathEscape");
    }

    #[rstest]
    #[case::host(b"https://HOST%2Eexample/repo", b"https://HOST%2Eexample/repo")]
    #[case::user(b"https://user%20name@HOST/repo", b"https://user%20name@host/repo")]
    #[case::query(b"https://HOST/a%20b?query", b"https://host/a%20b?query")]
    #[case::fragment(b"https://HOST/a%20b#fragment", b"https://host/a%20b#fragment")]
    #[case::credentials(b"https://user:secret@HOST/a%20b", b"https://user:secret@host/a%20b")]
    #[case::ipv6(b"https://[::1]/a%20b", b"https://[::1]/a%20b")]
    #[case::scheme(b"HTTPS://HOST/a%20b", b"HTTPS://host/a%20b")]
    #[case::custom(b"foo://HOST/repo", b"foo://host/repo")]
    #[case::ssh_ipv6(b"ssh://[::1]/repo", b"ssh://[::1]/repo")]
    #[case::authority_escape(b"https://host%20name/repo", b"https://host%20name/repo")]
    #[case::scp_space(b"HOST:repo name", b"host:repo name")]
    fn additional_presentation_syntax(#[case] input: &[u8], #[case] expected: &[u8]) {
        assert_eq!(normalize_configured_url(input).unwrap(), expected);
    }
    // Original gix 0.87.1 public URL oracle: file path spaces serialize byte-for-byte.
    #[rstest]
    #[case::ordinary(b"file:///tmp/repo name")]
    #[case::authority_case(b"file://HOST/repo name")]
    #[case::leading(b"file:/// repo")]
    #[case::trailing(b"file:///repo ")]
    #[case::repeated(b"file:///repo  name")]
    #[case::only_space(b"file:/// ")]
    #[case::components(b"file:///a b/c d")]
    #[case::percent(b"file:///a%20b c")]
    #[case::literal_percent(b"file:///a%GG b")]
    fn file_path_spaces_remain_literal(#[case] input: &[u8]) {
        assert_eq!(
            crate::remote::normalize_configured_url(input).unwrap(),
            input
        );
    }

    #[rstest]
    #[case::authority(b"file://bad host/repo")]
    #[case::tab(b"file:///a\tb")]
    #[case::newline(b"file:///a\nb")]
    #[case::nul(b"file:///a\0b")]
    #[case::encoding(b"file:///a\xff b")]
    #[case::ssh(b"ssh://host/repo name")]
    #[case::http(b"https://host/repo name")]
    fn file_spaces_do_not_change_other_boundaries(#[case] input: &[u8]) {
        assert_eq!(
            crate::remote::normalize_configured_url(input),
            Err(ConfiguredUrlError::Unsupported)
        );
    }
    #[rstest]
    #[case::credentials(
        b"https://user:secret@HOST:00443/repo",
        b"https://user:secret@host:443/repo"
    )]
    #[case::escaped_user(b"https://u%41:p%3a@HOST/repo", b"https://uA:p:@host/repo")]
    #[case::query_fragment(b"https://HOST/repo?token=x#part", b"https://host/repo?token=x#part")]
    #[case::opaque_authority(b"custom://HOST?x/repo", b"custom://HOST%3Fx/repo")]
    #[case::uppercase(b"HTTPS://HOST/repo", b"HTTPS://host/repo")]
    #[case::custom_utf8(b"custom://HOST/\xc3\xa9", b"custom://host/\xc3\xa9")]
    #[case::local_bracket(b"./repo@[backup", b"./repo@[backup")]
    #[case::absolute_bracket(b"/tmp/repo@[backup", b"/tmp/repo@[backup")]
    #[case::scp_path_bracket(b"HOST:repo@[backup", b"host:repo@[backup")]
    #[case::local_bracket_colon(b"./repo@[backup:old", b"./repo@[backup:old")]
    #[case::relative_parent(b"../missing/./repo", b"../missing/./repo")]
    #[case::relative_byte(b"./missing/\xff", b"./missing/\xff")]
    #[case::file_dots(b"file:///missing/a/../repo  ", b"file:///missing/a/../repo  ")]
    #[case::expanded_ipv6(
        b"ssh://[2001:db8:0:0:0:0:0:1]/repo",
        b"ssh://[2001:db8:0:0:0:0:0:1]/repo"
    )]
    fn pure_normalization_retains_config_meaning(#[case] input: &[u8], #[case] expected: &[u8]) {
        assert_eq!(normalize_configured_url(input).unwrap(), expected);
    }

    #[cfg(unix)]
    #[test]
    fn configured_paths_do_not_follow_symlinks_or_resolve_parent_components() {
        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("loop");
        std::os::unix::fs::symlink("loop", &link).unwrap();
        let input = format!("file://{}/loop/../repo name ", directory.path().display());
        assert_eq!(
            normalize_configured_url(input.as_bytes()).unwrap(),
            input.as_bytes()
        );
        assert_eq!(
            std::fs::read_link(link).unwrap(),
            std::path::Path::new("loop")
        );
    }

    #[cfg(feature = "http")]
    #[rstest]
    #[case::credentials(b"https://user:secret@HOST/repo")]
    #[case::query(b"https://HOST/repo?secret=value")]
    #[case::fragment(b"https://HOST/repo#secret")]
    #[case::custom(b"custom://HOST/repo")]
    fn config_acceptance_does_not_authorize_http(#[case] input: &[u8]) {
        let normalized = normalize_configured_url(input).unwrap();
        let url = std::str::from_utf8(&normalized).unwrap();
        let error = crate::transport::http::HttpRemote::new(url, &[], &[]).unwrap_err();
        assert!(!format!("{error:?}: {error}").contains("secret"));
    }

    #[cfg(all(feature = "ssh", unix))]
    #[rstest]
    #[case::credentials(b"ssh://user:secret@HOST/repo")]
    #[case::query(b"ssh://HOST/repo?secret=value")]
    #[case::fragment(b"ssh://HOST/repo#secret")]
    #[case::escape(b"ssh://HOST/repo%20name")]
    fn config_acceptance_does_not_authorize_ssh(#[case] input: &[u8]) {
        use crate::remote::{ProtocolEnvironment, Remote};
        use crate::transport::ssh::{OpenSshOptions, SshRemote};
        let normalized = normalize_configured_url(input).unwrap();
        let mut document = crate::config::Document::parse(b"").unwrap();
        document
            .append("remote", Some(b"r"), "url", &normalized)
            .unwrap();
        let config = document.config();
        let destination = Remote::find(config, b"r")
            .unwrap()
            .unwrap()
            .fetch_destination(config, &ProtocolEnvironment::default())
            .unwrap();
        let error = SshRemote::openssh(
            config,
            &destination,
            OpenSshOptions {
                default_executable: "/usr/bin/ssh".into(),
                ..OpenSshOptions::default()
            },
        )
        .unwrap_err();
        assert!(!format!("{error:?}: {error}").contains("secret"));
    }
}
