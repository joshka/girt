//! Lossless parsing and local-path expansion for remote URL presentation.

use std::ops::Range;
use std::path::Path;

use super::Protocol;

/// A borrowed remote URL or local path, without transport authorization.
///
/// Parsing retains the original bytes, including credentials and percent escapes. It recognizes
/// local paths, scp-like SSH locations, and `scheme://` URLs. A successful parse does not validate
/// transport-specific paths or authorize a protocol. The caller must avoid logging [`Self::bytes`]
/// when credentials might be present.
///
/// ```
/// use girt::remote::{ParsedUrl, Protocol};
///
/// let url = ParsedUrl::parse(b"git@example.org:team/repo.git")?;
/// assert_eq!(url.protocol(), Protocol::Ssh);
/// assert_eq!(url.host(), Some(b"example.org".as_slice()));
/// assert_eq!(url.path(), b"team/repo.git");
/// assert_eq!(url.bytes(), b"git@example.org:team/repo.git");
/// # Ok::<(), girt::remote::UrlError>(())
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct ParsedUrl<'a> {
    bytes: &'a [u8],
    protocol: Protocol,
    host: Option<Range<usize>>,
    path: Range<usize>,
}

impl std::fmt::Debug for ParsedUrl<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedUrl")
            .field("protocol", &self.protocol)
            .finish_non_exhaustive()
    }
}

/// A value-free parsing or local-path expansion error.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UrlError {
    /// Input is empty, contains NUL, or has an incomplete URL authority.
    #[error("invalid remote URL syntax")]
    Syntax,
    /// A relative path was supplied with a non-absolute base directory.
    #[error("base directory must be absolute")]
    RelativeBase,
    /// The local OS path cannot be represented in remote configuration bytes on this platform.
    #[error("local path cannot be encoded")]
    PathEncoding,
}

impl<'a> ParsedUrl<'a> {
    /// Classifies URL or path syntax without reading the filesystem or applying protocol policy.
    ///
    /// The input may be arbitrary bytes. Scheme-based URLs with an authority must name a host,
    /// except `file://` URLs. Custom schemes remain opaque and are classified as helpers. Parsing
    /// does not percent-decode or normalize the input. Drive-letter paths are local on Windows;
    /// on Unix, `C:/repo` has scp-like syntax.
    ///
    /// # Errors
    ///
    /// Returns [`UrlError::Syntax`] for empty input, NUL, an invalid scheme, or a missing or
    /// malformed URL authority.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, UrlError> {
        if bytes.is_empty() || bytes.contains(&0) {
            return Err(UrlError::Syntax);
        }
        if local_prefix(bytes) {
            return Ok(Self::local(bytes));
        }
        if let Some(colon) = top_level_colon(bytes) {
            let prefix = &bytes[..colon];
            if bytes.get(colon + 1..colon + 3) == Some(b"//") {
                if !valid_scheme(prefix) {
                    return Err(UrlError::Syntax);
                }
                let protocol = match prefix {
                    b"file" => Protocol::File,
                    b"ssh" => Protocol::Ssh,
                    b"http" => Protocol::Http,
                    b"https" => Protocol::Https,
                    b"git" => Protocol::Git,
                    _ => Protocol::Helper,
                };
                let authority_start = colon + 3;
                let authority_end = bytes[authority_start..]
                    .iter()
                    .position(|byte| matches!(byte, b'/' | b'?' | b'#'))
                    .map_or(bytes.len(), |index| authority_start + index);
                let host = parse_host(bytes, authority_start..authority_end)?;
                if host.is_none() && protocol != Protocol::File {
                    return Err(UrlError::Syntax);
                }
                return Ok(Self {
                    bytes,
                    protocol,
                    host,
                    path: authority_end..bytes.len(),
                });
            }
            if bytes.get(colon + 1) == Some(&b':') && valid_scheme(prefix) {
                return Ok(Self {
                    bytes,
                    protocol: Protocol::Helper,
                    host: None,
                    path: colon + 2..bytes.len(),
                });
            }
            if !prefix.contains(&b'/') && !prefix.contains(&b'\\') && !prefix.is_empty() {
                let host = parse_host(bytes, 0..colon)?.ok_or(UrlError::Syntax)?;
                return Ok(Self {
                    bytes,
                    protocol: Protocol::Ssh,
                    host: Some(host),
                    path: colon + 1..bytes.len(),
                });
            }
        }
        Ok(Self::local(bytes))
    }

    fn local(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            protocol: Protocol::Local,
            host: None,
            path: 0..bytes.len(),
        }
    }

    /// Returns the original bytes, including credentials and percent escapes.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Returns the recognized family; this does not authorize a transport.
    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// Returns the host bytes without user information or port, if this syntax has a host.
    ///
    /// Brackets around an IPv6 literal are retained so the value can be placed in a URL authority.
    pub fn host(&self) -> Option<&'a [u8]> {
        self.host.as_ref().map(|range| &self.bytes[range.clone()])
    }

    /// Returns the syntactic path bytes, without percent decoding.
    ///
    /// For scheme URLs this includes the leading slash, if any, and any query or fragment suffix
    /// as opaque bytes. For scp-like syntax it starts after the colon. For local paths it is the
    /// complete input. This is a Git remote presentation component, not an HTTP URL path parser.
    pub fn path(&self) -> &'a [u8] {
        &self.bytes[self.path.clone()]
    }

    /// Returns a local path in absolute, slash-separated form, or the unchanged remote URL.
    ///
    /// Relative local paths are joined to an absolute `base`; `.` and `..` components are removed
    /// lexically. No filesystem access, symlink resolution, or percent decoding occurs, and the
    /// target need not exist. File URLs remain unchanged. On Windows, local separators in the
    /// resulting path become `/` for use in Git remote configuration.
    ///
    /// # Errors
    ///
    /// Returns [`UrlError::RelativeBase`] for a relative local path and non-absolute base, or
    /// [`UrlError::PathEncoding`] if an OS path cannot be encoded on this platform.
    pub fn canonicalize_local(&self, base: &Path) -> Result<Vec<u8>, UrlError> {
        if self.protocol != Protocol::Local {
            return Ok(self.bytes.to_vec());
        }
        let local = os_path(self.bytes)?;
        let absolute = if local.is_absolute() {
            local
        } else {
            if !base.is_absolute() {
                return Err(UrlError::RelativeBase);
            }
            base.join(local)
        };
        let mut clean = std::path::PathBuf::new();
        for component in absolute.components() {
            match component {
                std::path::Component::ParentDir => {
                    clean.pop();
                }
                std::path::Component::CurDir => {}
                _ => clean.push(component.as_os_str()),
            }
        }
        let bytes = path_bytes(&clean)?;
        #[cfg(windows)]
        let bytes = bytes
            .into_iter()
            .map(|byte| if byte == b'\\' { b'/' } else { byte })
            .collect();
        Ok(bytes)
    }
}

fn local_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(b"/")
        || bytes.starts_with(b"./")
        || bytes.starts_with(b"../")
        || bytes.starts_with(b"\\\\")
        || (cfg!(windows)
            && bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

fn valid_scheme(bytes: &[u8]) -> bool {
    bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+.-".contains(byte))
}

fn top_level_colon(bytes: &[u8]) -> Option<usize> {
    let mut bracketed = false;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'[' => bracketed = true,
            b']' => bracketed = false,
            b':' if !bracketed => return Some(index),
            _ => {}
        }
    }
    None
}

fn parse_host(bytes: &[u8], authority: Range<usize>) -> Result<Option<Range<usize>>, UrlError> {
    let start = bytes[authority.clone()]
        .iter()
        .rposition(|byte| *byte == b'@')
        .map_or(authority.start, |index| authority.start + index + 1);
    if start == authority.end {
        return Ok(None);
    }
    if bytes[start] == b'[' {
        let close = bytes[start..authority.end]
            .iter()
            .position(|byte| *byte == b']')
            .map(|index| start + index)
            .ok_or(UrlError::Syntax)?;
        if close == start + 1 || (close + 1 < authority.end && bytes[close + 1] != b':') {
            return Err(UrlError::Syntax);
        }
        return Ok(Some(start..close + 1));
    }
    let end = bytes[start..authority.end]
        .iter()
        .position(|byte| *byte == b':')
        .map_or(authority.end, |index| start + index);
    (start != end)
        .then_some(start..end)
        .ok_or(UrlError::Syntax)
        .map(Some)
}

#[cfg(unix)]
fn os_path(bytes: &[u8]) -> Result<std::path::PathBuf, UrlError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::ffi::OsString::from_vec(bytes.to_vec()).into())
}

#[cfg(not(unix))]
fn os_path(bytes: &[u8]) -> Result<std::path::PathBuf, UrlError> {
    Ok(std::path::PathBuf::from(
        std::str::from_utf8(bytes).map_err(|_| UrlError::PathEncoding)?,
    ))
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Result<Vec<u8>, UrlError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(path.as_os_str().as_bytes().to_vec())
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Result<Vec<u8>, UrlError> {
    Ok(path
        .to_str()
        .ok_or(UrlError::PathEncoding)?
        .as_bytes()
        .to_vec())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::scp(
        b"git@example.org:team/repo.git",
        Protocol::Ssh,
        Some(&b"example.org"[..]),
        b"team/repo.git"
    )]
    #[case::ssh(
        b"ssh://git@example.org/team/repo",
        Protocol::Ssh,
        Some(&b"example.org"[..]),
        b"/team/repo"
    )]
    #[case::scp_ipv6(
        b"git@[::1]:repo.git",
        Protocol::Ssh,
        Some(&b"[::1]"[..]),
        b"repo.git"
    )]
    #[case::https(
        b"https://name:secret@example.org:443/%20%25",
        Protocol::Https,
        Some(&b"example.org"[..]),
        b"/%20%25"
    )]
    #[case::git(
        b"git://example.org/repo.git",
        Protocol::Git,
        Some(&b"example.org"[..]),
        b"/repo.git"
    )]
    #[case::custom(
        b"custom://example.org/repo",
        Protocol::Helper,
        Some(&b"example.org"[..]),
        b"/repo"
    )]
    #[case::helper(b"custom::opaque", Protocol::Helper, None, b"opaque")]
    #[case::file(b"file:///%20%25", Protocol::File, None, b"/%20%25")]
    #[case::local(b"../repo", Protocol::Local, None, b"../repo")]
    fn parses_and_preserves_bytes(
        #[case] raw: &[u8],
        #[case] protocol: Protocol,
        #[case] host: Option<&[u8]>,
        #[case] path: &[u8],
    ) {
        let parsed = ParsedUrl::parse(raw).unwrap();
        assert_eq!(parsed.protocol(), protocol);
        assert_eq!(parsed.host(), host);
        assert_eq!(parsed.path(), path);
        assert_eq!(parsed.bytes(), raw);
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_path_is_local() {
        let parsed = ParsedUrl::parse(br"C:\repo").unwrap();
        assert_eq!(parsed.protocol(), Protocol::Local);
        assert_eq!(parsed.path(), br"C:\repo");
    }

    #[cfg(unix)]
    #[rstest]
    #[case::backslash(br"C:\repo", br"\repo")]
    #[case::slash(b"C:/repo", b"/repo")]
    fn unix_drive_looking_path_is_scp_like(#[case] raw: &[u8], #[case] path: &[u8]) {
        let parsed = ParsedUrl::parse(raw).unwrap();
        assert_eq!(parsed.protocol(), Protocol::Ssh);
        assert_eq!(parsed.host(), Some(b"C".as_slice()));
        assert_eq!(parsed.path(), path);
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::nul(b"https://host/repo\0secret")]
    #[case::no_host(b"https:///repo")]
    #[case::empty_scp_host(b"user@:repo")]
    #[case::broken_ipv6(b"ssh://[::1/repo")]
    #[case::invalid_scheme(b"1bad://host/repo")]
    fn rejects_malformed_input(#[case] raw: &[u8]) {
        assert_eq!(ParsedUrl::parse(raw).err(), Some(UrlError::Syntax));
    }

    #[test]
    fn debug_does_not_expose_credentials() {
        let url = ParsedUrl::parse(b"https://name:secret@example.org/repo").unwrap();
        let debug = format!("{url:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("name"));
    }

    #[test]
    fn local_canonicalization_needs_no_target_on_disk() {
        let url = ParsedUrl::parse(b"missing/repo").unwrap();
        #[cfg(unix)]
        assert_eq!(
            url.canonicalize_local(Path::new("/tmp/girt-uncreated"))
                .unwrap(),
            b"/tmp/girt-uncreated/missing/repo"
        );
        #[cfg(windows)]
        assert_eq!(
            url.canonicalize_local(Path::new(r"C:\girt-uncreated"))
                .unwrap(),
            b"C:/girt-uncreated/missing/repo"
        );
    }

    #[cfg(unix)]
    #[rstest]
    #[case::parent(b"foo/../target")]
    #[case::current_and_parent(b"./foo/../target")]
    fn local_canonicalization_removes_dot_components(#[case] raw: &[u8]) {
        let url = ParsedUrl::parse(raw).unwrap();
        assert_eq!(
            url.canonicalize_local(Path::new("/tmp/base")).unwrap(),
            b"/tmp/base/target"
        );
    }

    #[test]
    fn remote_canonicalization_preserves_secret_and_escapes() {
        let raw = b"https://name:secret@example.org/%20%25";
        let url = ParsedUrl::parse(raw).unwrap();
        assert_eq!(url.canonicalize_local(Path::new("relative")).unwrap(), raw);
    }

    #[test]
    fn local_canonicalization_requires_absolute_base() {
        let url = ParsedUrl::parse(b"repo").unwrap();
        assert_eq!(
            url.canonicalize_local(Path::new("relative")),
            Err(UrlError::RelativeBase)
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_local_path_preserves_non_utf8_bytes() {
        let url = ParsedUrl::parse(b"repo-\xff").unwrap();
        assert_eq!(
            url.canonicalize_local(Path::new("/tmp")).unwrap(),
            b"/tmp/repo-\xff"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_local_path_normalizes_separators() {
        let url = ParsedUrl::parse(br"sub\repo").unwrap();
        assert_eq!(
            url.canonicalize_local(Path::new(r"C:\base")).unwrap(),
            b"C:/base/sub/repo"
        );
    }
}
