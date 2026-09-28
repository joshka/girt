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
    /// URL syntax or normalization is outside the characterized subset.
    /// [`super::ConfiguredRemote`] reports this as its distinct unsupported error variant.
    #[error("unsupported configured URL syntax")]
    Unsupported,
}

/// Validates and serializes one configured destination, without applying URL rewrites.
///
/// Supports ordinary local byte paths, file URLs, scp-like SSH, SSH URLs and HTTP(S) URLs.
/// ASCII network hosts are lowercased and numeric ports lose leading zeroes; explicit default
/// ports and ordinary dot path segments are retained. File-host case is preserved. An HTTP(S)
/// URL without a path gains `/`. Local paths can contain non-UTF-8 bytes and spaces.
///
/// Unlike an empty value in [`super::ConfiguredRemoteRecord`], an empty destination is an error,
/// not a list reset. This function does not read configuration, apply fetch-to-push fallback,
/// authorize a transport, access the network or check whether a repository exists.
/// Returned bytes may contain private paths or credentials; do not log them by default.
///
/// # Errors
///
/// Returns a value-free diagnostic for malformed supported syntax. Unknown protocols, helpers,
/// IPv6, passwords, percent escapes, query/fragment handling, Unicode normalization, uppercase
/// schemes and Windows drive/UNC syntax return [`ConfiguredUrlError::Unsupported`]. That result
/// requests compatibility handling; it does not establish that the destination is malformed.
///
/// ```
/// use girt::remote::{ConfiguredUrlError, normalize_configured_url};
/// assert_eq!(
///     normalize_configured_url(b"https://HOST:00443/repo")?,
///     b"https://host:443/repo"
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
    let Some(colon) = colon.filter(|colon| slash.is_none_or(|slash| *colon < slash)) else {
        return Ok(bytes.to_vec());
    };
    if bytes.get(colon + 1) == Some(&b':') {
        return Err(Error::Unsupported);
    }
    if bytes.get(colon + 1..colon + 3) != Some(b"//") {
        return scp(bytes, colon);
    }
    let scheme = &bytes[..colon];
    if scheme == b"file" {
        return file_url(bytes, colon + 3);
    }
    if !matches!(scheme, b"ssh" | b"http" | b"https") {
        return Err(Error::Unsupported);
    }
    std::str::from_utf8(bytes).map_err(|_| Error::Encoding)?;
    let rest = &bytes[colon + 3..];
    if rest.iter().any(|b| matches!(b, b'?' | b'#' | b'%' | b'\\')) {
        return Err(Error::Unsupported);
    }
    let (authority, path) = match rest.iter().position(|b| *b == b'/') {
        Some(slash) => (&rest[..slash], &rest[slash..]),
        None => (rest, b"".as_slice()),
    };
    let authority = network_authority(authority, true)?;
    let path = if path.is_empty() {
        if scheme == b"ssh" {
            return Err(Error::MissingPath);
        }
        b"/".as_slice()
    } else {
        plain_path(path)?;
        path
    };
    Ok([scheme, b"://", &authority, path].concat())
}

fn scp(bytes: &[u8], colon: usize) -> Result<Vec<u8>, ConfiguredUrlError> {
    let path = &bytes[colon + 1..];
    // Byte paths and escaping beyond ordinary ASCII need separate URL normalization evidence.
    if bytes.contains(&b'\\') || !bytes.is_ascii() {
        return Err(ConfiguredUrlError::Unsupported);
    }
    let authority = network_authority(&bytes[..colon], false)?;
    if path.is_empty() {
        return Err(ConfiguredUrlError::MissingPath);
    }
    plain_path(path)?;
    Ok([authority.as_slice(), b":", path].concat())
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
    plain_path(&rest[slash..])?;
    // File authorities are preserved, unlike the network host case normalization.
    Ok(bytes.to_vec())
}

fn network_authority(bytes: &[u8], port_allowed: bool) -> Result<Vec<u8>, ConfiguredUrlError> {
    use ConfiguredUrlError as Error;
    if bytes
        .iter()
        .any(|b| matches!(b, b'[' | b']' | b'%' | b'\\'))
        || !bytes.is_ascii()
    {
        return Err(Error::Unsupported);
    }
    let (user, host_port) = match bytes.iter().position(|b| *b == b'@') {
        Some(at) => {
            let user = &bytes[..at];
            if user.is_empty()
                || !user
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~'))
            {
                return Err(Error::Unsupported);
            }
            (Some(user), &bytes[at + 1..])
        }
        None => (None, bytes),
    };
    let (host, port) = match host_port.iter().position(|b| *b == b':') {
        Some(colon) if port_allowed => (&host_port[..colon], Some(&host_port[colon + 1..])),
        Some(_) => return Err(Error::Unsupported),
        None => (host_port, None),
    };
    if host.is_empty() || host.contains(&b' ') || host.contains(&b'@') {
        return Err(Error::Authority);
    }
    if !host
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
    {
        return Err(Error::Unsupported);
    }
    let mut normalized = Vec::new();
    if let Some(user) = user {
        normalized.extend_from_slice(user);
        normalized.push(b'@');
    }
    normalized.extend(host.iter().map(u8::to_ascii_lowercase));
    if let Some(port) = port {
        normalized.push(b':');
        if !port.is_empty() {
            if !port.iter().all(u8::is_ascii_digit) {
                return Err(Error::Port);
            }
            let number = std::str::from_utf8(port)
                .ok()
                .and_then(|p| p.parse::<u16>().ok())
                .filter(|number| *number != 0)
                .ok_or(Error::Port)?;
            normalized.extend_from_slice(number.to_string().as_bytes());
        }
    }
    Ok(normalized)
}

fn plain_path(path: &[u8]) -> Result<(), ConfiguredUrlError> {
    if path.contains(&b' ') {
        return Err(ConfiguredUrlError::Unsupported);
    }
    if !path
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'~' | b'+'))
    {
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
    #[case::empty_host(b"http:///repo", ConfiguredUrlError::Authority)]
    #[case::network_bytes(b"http://host/\xff", ConfiguredUrlError::Encoding)]
    #[case::helper(b"foo::repo", ConfiguredUrlError::Unsupported)]
    #[case::scheme(b"foo://host/repo", ConfiguredUrlError::Unsupported)]
    #[case::ipv6(b"ssh://[::1]/repo", ConfiguredUrlError::Unsupported)]
    #[case::password(b"https://user:secret@host/repo", ConfiguredUrlError::Unsupported)]
    #[case::escape(b"https://host/repo%20name", ConfiguredUrlError::Unsupported)]
    fn failures(#[case] input: &[u8], #[case] error: ConfiguredUrlError) {
        assert_eq!(
            crate::remote::normalize_configured_url(input).unwrap_err(),
            error
        );
    }

    #[test]
    fn single_url_error_does_not_disclose_input() {
        let error =
            normalize_configured_url(b"https://user:private-password@host/repo").unwrap_err();
        assert_eq!(error, ConfiguredUrlError::Unsupported);
        assert!(!format!("{error:?}: {error}").contains("private-password"));
    }
}
