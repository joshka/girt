//! User-input URL normalization and filesystem-aware local path expansion.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// Failure while preparing a user-supplied repository location.
///
/// Diagnostics never include the supplied URL, which may contain credentials. Parsing is completed
/// before filesystem resolution starts, allowing callers to distinguish argument and path errors.
#[derive(Debug, thiserror::Error)]
pub enum UserUrlError {
    /// The supplied location is malformed.
    #[error(transparent)]
    Parse(#[from] UserUrlParseError),
    /// A local or file-URL path could not be resolved.
    #[error(transparent)]
    Path(#[from] UserUrlPathError),
}

/// A value-free user-input URL parsing diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum UserUrlParseError {
    /// An empty local path was supplied.
    #[error("local path does not specify a repository")]
    EmptyPath,
    /// A scheme URL is malformed.
    #[error("invalid repository URL")]
    InvalidUrl,
    /// A URL authority has an invalid host or user component.
    #[error("repository URL has an invalid authority")]
    InvalidAuthority,
    /// A network URL requiring an authority has no host.
    #[error("repository URL has no host")]
    MissingHost,
    /// An explicit port is not an integer in the range 1 through 65535.
    #[error("repository URL has an invalid port")]
    InvalidPort,
    /// A percent escape is not followed by two hexadecimal digits.
    #[error("repository URL has an invalid percent escape")]
    InvalidEscape,
    /// A decoded network URL component is not UTF-8.
    #[error("repository URL component is not UTF-8")]
    InvalidEncoding,
    /// A scheme requiring a repository path has none.
    #[error("repository URL has no path")]
    MissingUrlPath,
    /// An scp-like location has a malformed authority.
    #[error("invalid scp-like repository location")]
    InvalidScp,
    /// An scp-like location has no repository path.
    #[error("scp-like repository location has no path")]
    MissingScpPath,
}

/// A value-free local-path expansion diagnostic.
#[derive(Debug, thiserror::Error)]
pub enum UserUrlPathError {
    /// Resolving a relative path requires an absolute base directory.
    #[error("base directory must be absolute")]
    RelativeBase,
    /// A path cannot be represented on the current platform.
    #[error("local path cannot be encoded")]
    Encoding,
    /// Expansion followed more than 32 symbolic links.
    #[error("The maximum allowed number 32 of symlinks in path is exceeded")]
    SymlinkLimit,
    /// Reading a symbolic link failed for a reason other than a missing or non-link component.
    #[error("cannot resolve repository path")]
    Io(#[source] std::io::Error),
}

/// Normalizes a user-supplied Git location and resolves its local filesystem components.
///
/// Local paths and lowercase `file://` URLs become absolute using `base`. Existing symbolic links
/// are expanded before subsequent `..` components, including links to nonexistent targets. Missing
/// suffixes and uninspectable components are permitted; this operation neither requires a
/// repository nor creates anything. At
/// most 32 symbolic links are followed. Resolution is observational, not a filesystem safety check:
/// another process can change the path after this call.
///
/// Ordinary ASCII network hosts become lowercase and ports become decimal. Explicit default ports
/// remain present. Opaque custom authorities are escaped without folding their case. Scheme-URL
/// user information is decoded and escaped for serialization; scp user information and path percent
/// escapes retain their spelling. Lowercase HTTP(S) URLs without a path gain `/`. Other scheme
/// spelling is retained, and custom schemes are accepted without authorizing any transport. File
/// and scp-like paths retain literal percent sequences. Local paths retain arbitrary bytes on Unix;
/// other platforms require UTF-8. Windows local path separators are serialized as `/`;
/// drive-relative paths require a base on the same drive, rather than consulting per-drive process
/// state.
///
/// Returned bytes include credentials and private paths. Do not log them by default. This operation
/// does not apply configuration rewrites or alter [`super::ParsedUrl`]'s lossless parsing contract.
///
/// # Errors
///
/// Returns [`UserUrlError::Parse`] for malformed syntax, escapes or ports, and missing required
/// paths. Returns [`UserUrlError::Path`] for filesystem resolution, encoding and base-directory
/// errors. No files are modified on either success or failure.
///
/// ```
/// use girt::remote::canonicalize_user_url;
/// let url = canonicalize_user_url(
///     b"https://USER:secret@HOST:00443/repo",
///     std::path::Path::new("/"),
/// )?;
/// assert_eq!(url, b"https://USER:secret@host:443/repo");
/// # Ok::<(), girt::remote::UserUrlError>(())
/// ```
pub fn canonicalize_user_url(source: &[u8], base: &Path) -> Result<Vec<u8>, UserUrlError> {
    let form = UrlForm::parse(source)?;
    match form {
        UrlForm::Local => resolve_path(source, base).map_err(Into::into),
        UrlForm::File { path_start } => {
            let path = resolve_path(&source[path_start..], base)?;
            Ok([&source[..path_start], path.as_slice()].concat())
        }
        _ => form.render(source).map_err(Into::into),
    }
}

/// Normalizes presentation without accessing the filesystem or expanding relative paths.
pub(super) fn normalize(source: &[u8]) -> Result<Vec<u8>, UserUrlParseError> {
    UrlForm::parse(source)?.render(source)
}

enum UrlForm {
    Local,
    File { path_start: usize },
    Network { colon: usize },
    Scp { colon: usize },
}

impl UrlForm {
    fn parse(source: &[u8]) -> Result<Self, UserUrlParseError> {
        use UserUrlParseError as Parse;
        if source.is_empty() {
            return Err(Parse::EmptyPath);
        }
        let colon = authority_colon(source);
        let local = source.starts_with(b"/")
            || source.starts_with(b"./")
            || source.starts_with(b"../")
            || source.starts_with(b"\\\\")
            || windows_drive(source)
            || colon.is_none_or(|at| source[..at].contains(&b'/'));
        if local {
            return Ok(Self::Local);
        }
        let colon = colon.expect("nonlocal input contains a colon");
        if source.get(colon + 1..colon + 3) != Some(b"//") {
            return Ok(Self::Scp { colon });
        }
        let scheme = &source[..colon];
        if !scheme.first().is_some_and(u8::is_ascii_alphabetic)
            || !scheme
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(b))
        {
            return Err(Parse::InvalidUrl);
        }
        if scheme == b"file" {
            let slash = source[colon + 3..]
                .iter()
                .position(|b| *b == b'/')
                .ok_or(Parse::MissingUrlPath)?;
            return Ok(Self::File {
                path_start: colon + 3 + slash,
            });
        }
        Ok(Self::Network { colon })
    }

    fn render(self, source: &[u8]) -> Result<Vec<u8>, UserUrlParseError> {
        use UserUrlParseError as Parse;
        match self {
            Self::Local | Self::File { .. } => Ok(source.to_vec()),
            Self::Network { colon } => normalize_network(source, colon),
            Self::Scp { colon } => {
                let path = &source[colon + 1..];
                if path.is_empty() {
                    return Err(Parse::MissingScpPath);
                }
                let authority = normalize_authority(&source[..colon], AuthorityKind::Scp)
                    .map_err(|_| Parse::InvalidScp)?;
                Ok([authority.as_slice(), b":", path].concat())
            }
        }
    }
}

fn normalize_network(source: &[u8], colon: usize) -> Result<Vec<u8>, UserUrlParseError> {
    use UserUrlParseError as Error;
    let scheme = &source[..colon];
    let rest = &source[colon + 3..];
    let kind = match scheme {
        b"http" | b"https" => AuthorityKind::Web,
        b"ssh" => AuthorityKind::Ssh,
        _ => AuthorityKind::Other,
    };
    let end = rest
        .iter()
        .position(|b| *b == b'/' || (kind != AuthorityKind::Other && b"?#".contains(b)))
        .unwrap_or(rest.len());
    let authority = normalize_authority(&rest[..end], kind)?;
    let path = &rest[end..];
    if end == 0 && matches!(scheme, b"http" | b"https" | b"ssh" | b"git") {
        return Err(Error::MissingHost);
    }
    if !path.starts_with(b"/") && matches!(scheme, b"ssh" | b"git") {
        return Err(Error::MissingUrlPath);
    }
    let mut path = encode_network_path(path, kind == AuthorityKind::Web)?;
    if path.is_empty() && matches!(scheme, b"http" | b"https") {
        path.push(b'/');
    }
    Ok([&source[..colon + 3], &authority, &path].concat())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthorityKind {
    Web,
    Ssh,
    Scp,
    Other,
}

fn normalize_authority(bytes: &[u8], kind: AuthorityKind) -> Result<Vec<u8>, UserUrlParseError> {
    use UserUrlParseError::InvalidAuthority as Error;
    let port_allowed = kind != AuthorityKind::Scp;
    let (userinfo, host_port) = match bytes.iter().rposition(|b| *b == b'@') {
        Some(at) => (Some(&bytes[..at]), &bytes[at + 1..]),
        None => (None, bytes),
    };
    let (host, port) = if host_port.starts_with(b"[") {
        let close = host_port.iter().position(|b| *b == b']').ok_or(Error)?;
        if kind == AuthorityKind::Web {
            std::str::from_utf8(&host_port[1..close])
                .map_err(|_| Error)?
                .parse::<std::net::Ipv6Addr>()
                .map_err(|_| Error)?;
        }
        let tail = &host_port[close + 1..];
        if !tail.is_empty() && (!port_allowed || !tail.starts_with(b":")) {
            return Err(Error);
        }
        let host = if matches!(kind, AuthorityKind::Ssh | AuthorityKind::Scp)
            && !host_port[1..close].contains(&b':')
        {
            &host_port[1..close]
        } else {
            &host_port[..close + 1]
        };
        (host, tail.strip_prefix(b":"))
    } else {
        let colon = host_port.iter().position(|b| *b == b':');
        match colon {
            Some(at) if port_allowed => (&host_port[..at], Some(&host_port[at + 1..])),
            Some(_) => return Err(Error),
            None => (host_port, None),
        }
    };
    if (kind == AuthorityKind::Web && !host.is_ascii())
        || host
            .iter()
            .any(|b| b.is_ascii_control() || b" /@\\".contains(b))
    {
        return Err(Error);
    }
    if port_allowed {
        decode_escapes(host)?;
    }
    if host.is_empty() && (userinfo.is_some() || !port_allowed) {
        return Err(Error);
    }
    let mut out = Vec::new();
    if let Some(userinfo) = userinfo {
        if port_allowed {
            if userinfo.iter().any(|b| b.is_ascii_control() || *b == b' ') {
                return Err(Error);
            }
            let separator = userinfo.iter().position(|b| *b == b':');
            let (user, password) = match separator {
                Some(at) => (&userinfo[..at], Some(&userinfo[at + 1..])),
                None => (userinfo, None),
            };
            encode_userinfo(&mut out, &decode_escapes(user)?, false);
            if let Some(password) = password.filter(|password| !password.is_empty()) {
                out.push(b':');
                encode_userinfo(&mut out, &decode_escapes(password)?, true);
            }
        } else {
            out.extend_from_slice(userinfo);
        }
        if !out.is_empty() || kind == AuthorityKind::Scp {
            out.push(b'@');
        }
    }
    let escape_host = |byte: u8| {
        (kind != AuthorityKind::Scp && !byte.is_ascii())
            || (kind == AuthorityKind::Other && b"[]:?#".contains(&byte))
    };
    if host.contains(&b'%') || host.iter().copied().any(escape_host) {
        for &byte in host {
            if escape_host(byte) {
                push_escape(&mut out, byte);
            } else {
                out.push(byte);
            }
        }
    } else {
        out.extend(host.iter().map(u8::to_ascii_lowercase));
    }
    if let Some(port) = port {
        out.push(b':');
        if !port.is_empty() {
            if !port.iter().all(u8::is_ascii_digit) {
                return Err(UserUrlParseError::InvalidPort);
            }
            let port = std::str::from_utf8(port)
                .map_err(|_| UserUrlParseError::InvalidPort)?
                .parse::<u16>()
                .map_err(|_| UserUrlParseError::InvalidPort)?;
            if port == 0 {
                return Err(UserUrlParseError::InvalidPort);
            }
            out.extend_from_slice(port.to_string().as_bytes());
        }
    }
    Ok(out)
}

fn encode_userinfo(out: &mut Vec<u8>, bytes: &[u8], password: bool) {
    for &byte in bytes {
        if byte.is_ascii_alphanumeric()
            || b"-._~!$&'()*+,;=".contains(&byte)
            || (password && byte == b':')
        {
            out.push(byte);
        } else {
            push_escape(out, byte);
        }
    }
}

fn encode_network_path(bytes: &[u8], web: bool) -> Result<Vec<u8>, UserUrlParseError> {
    if bytes.iter().any(|b| b.is_ascii_control() || *b == b' ') {
        return Err(UserUrlParseError::InvalidUrl);
    }
    decode_escapes(bytes)?;
    let mut out = Vec::new();
    for &byte in bytes {
        if web && (!byte.is_ascii() || b"\"<>`{}".contains(&byte)) {
            push_escape(&mut out, byte);
        } else {
            out.push(byte);
        }
    }
    Ok(out)
}

fn decode_escapes(mut bytes: &[u8]) -> Result<Vec<u8>, UserUrlParseError> {
    let mut out = Vec::new();
    while let Some((&byte, rest)) = bytes.split_first() {
        if byte == b'%' {
            let pair = rest.get(..2).ok_or(UserUrlParseError::InvalidEscape)?;
            let high = (pair[0] as char)
                .to_digit(16)
                .ok_or(UserUrlParseError::InvalidEscape)?;
            let low = (pair[1] as char)
                .to_digit(16)
                .ok_or(UserUrlParseError::InvalidEscape)?;
            out.push((high * 16 + low) as u8);
            bytes = &rest[2..];
        } else {
            out.push(byte);
            bytes = rest;
        }
    }
    std::str::from_utf8(&out).map_err(|_| UserUrlParseError::InvalidEncoding)?;
    Ok(out)
}

fn push_escape(out: &mut Vec<u8>, byte: u8) {
    const HEX: &[u8] = b"0123456789ABCDEF";
    out.extend_from_slice(&[b'%', HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]]);
}

fn authority_colon(bytes: &[u8]) -> Option<usize> {
    let mut bracketed = false;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'[' if (index == 0 || bytes[index - 1] == b'@') && bytes[index..].contains(&b']') => {
                bracketed = true
            }
            b']' => bracketed = false,
            b':' if !bracketed => return Some(index),
            _ => {}
        }
    }
    None
}

fn windows_drive(bytes: &[u8]) -> bool {
    cfg!(windows) && bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn resolve_path(bytes: &[u8], base: &Path) -> Result<Vec<u8>, UserUrlPathError> {
    let path = os_path(bytes)?;
    let absolute = if path.is_absolute() {
        path
    } else {
        if !base.is_absolute() {
            return Err(UserUrlPathError::RelativeBase);
        }
        join_relative(base, &path)?
    };
    let mut pending = components(&absolute);
    let mut resolved = PathBuf::new();
    let mut links = 0;
    while let Some(component) = pending.pop_front() {
        if component == ".." {
            resolved.pop();
            continue;
        }
        if component == "." {
            continue;
        }
        resolved.push(&component);
        match symlink_target(&resolved) {
            Ok(Some(target)) => {
                links += 1;
                if links > 32 {
                    return Err(UserUrlPathError::SymlinkLimit);
                }
                resolved.pop();
                let mut expanded = components(&target);
                expanded.append(&mut pending);
                pending = expanded;
            }
            Ok(None) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::NotADirectory
                        | std::io::ErrorKind::InvalidInput
                ) => {}
            Err(error) => return Err(UserUrlPathError::Io(error)),
        }
    }
    path_bytes(&resolved)
}

#[cfg(not(windows))]
fn join_relative(base: &Path, path: &Path) -> Result<PathBuf, UserUrlPathError> {
    Ok(base.join(path))
}

#[cfg(windows)]
fn join_relative(base: &Path, path: &Path) -> Result<PathBuf, UserUrlPathError> {
    let mut parts = path.components();
    if let Some(Component::Prefix(prefix)) = parts.next() {
        // An explicit base cannot supply another drive's per-drive current directory.
        if base.components().next() != Some(Component::Prefix(prefix)) {
            return Err(UserUrlPathError::RelativeBase);
        }
        return Ok(base.join(parts.as_path()));
    }
    Ok(base.join(path))
}

fn symlink_target(path: &Path) -> std::io::Result<Option<PathBuf>> {
    // Normalization permits inaccessible and otherwise uninspectable components, just as it
    // permits missing suffixes. It must not become a repository accessibility check.
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if metadata.file_type().is_symlink() {
        std::fs::read_link(path).map(Some)
    } else {
        Ok(None)
    }
}

fn components(path: &Path) -> VecDeque<OsString> {
    path.components()
        .map(|component| match component {
            Component::CurDir => OsString::from("."),
            other => other.as_os_str().to_owned(),
        })
        .collect()
}

#[cfg(unix)]
fn os_path(bytes: &[u8]) -> Result<PathBuf, UserUrlPathError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes.to_vec()).into())
}

#[cfg(not(unix))]
fn os_path(bytes: &[u8]) -> Result<PathBuf, UserUrlPathError> {
    Ok(PathBuf::from(
        std::str::from_utf8(bytes).map_err(|_| UserUrlPathError::Encoding)?,
    ))
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Result<Vec<u8>, UserUrlPathError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(path.as_os_str().as_bytes().to_vec())
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Result<Vec<u8>, UserUrlPathError> {
    Ok(path
        .to_str()
        .ok_or(UserUrlPathError::Encoding)?
        .replace('\\', "/")
        .into_bytes())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    // Original cases characterized through the public gix 0.87.1 URL API on macOS. These are
    // serialization observations, not claims about network transport acceptance or authorization.
    #[rstest]
    #[case::host_port("https://HOST:00443", "https://host:443/")]
    #[case::custom("custom://HOST:00022/repo", "custom://host:22/repo")]
    #[case::custom_without_path("custom://HOST", "custom://host")]
    #[case::empty_custom_authority("custom:///x", "custom:///x")]
    #[case::password("https://user:pass@HOST/repo", "https://user:pass@host/repo")]
    #[case::userinfo_decode("https://u%41:p%61@HOST/x", "https://uA:pa@host/x")]
    #[case::userinfo_delimiter("https://u%40:p%3a@h/r", "https://u%40:p:@h/r")]
    #[case::empty_user("https://@x/x", "https://x/x")]
    #[case::path_spelling("https://host/%2Egit", "https://host/%2Egit")]
    #[case::path_utf8("https://host/é", "https://host/%C3%A9")]
    #[case::path_escape("https://host/a`b{c}", "https://host/a%60b%7Bc%7D")]
    #[case::path_dots("https://host/a/../b", "https://host/a/../b")]
    #[case::scheme_case("HTTPS://HOST/repo", "HTTPS://host/repo")]
    #[case::upper_http_empty("HTTP://HOST", "HTTP://host")]
    #[case::query("https://host/repo?x=1#frag", "https://host/repo?x=1#frag")]
    #[case::query_only("https://host?x", "https://host?x")]
    #[case::ipv6("ssh://[2001:0DB8::1]:22/repo", "ssh://[2001:0db8::1]:22/repo")]
    #[case::scp_empty_user("@HOST:repo", "@host:repo")]
    #[case::ssh_bracketed_host("ssh://[host]/r", "ssh://host/r")]
    #[case::ssh_future_host("ssh://[v1.ab]/r", "ssh://v1.ab/r")]
    #[case::custom_bracketed_host("custom://[host]/r", "custom://%5Bhost%5D/r")]
    #[case::custom_query_authority("custom://HOST?x/r", "custom://HOST%3Fx/r")]
    #[case::custom_fragment_authority("custom://host#x", "custom://host%23x")]
    #[case::custom_unicode_path("custom://host/é", "custom://host/é")]
    #[case::ssh_unicode_host("ssh://münich/r", "ssh://m%C3%BCnich/r")]
    #[case::scp_bracket_host("[host]:repo", "host:repo")]
    #[case::scp_unmatched_bracket("a[b:repo", "a[b:repo")]
    #[case::encoded_host_case("https://HOST%2Eexample/r", "https://HOST%2Eexample/r")]
    #[case::encoded_host("https://ho%73t/x", "https://ho%73t/x")]
    // The public reference API drops brackets for fully expanded IPv6. Retaining them is an
    // intentional correction: the resulting authority stays unambiguous and parseable.
    #[case::expanded_ipv6(
        "ssh://[2001:0DB8:0000:0000:0000:0000:0000:0001]/r",
        "ssh://[2001:0db8:0000:0000:0000:0000:0000:0001]/r"
    )]
    #[case::empty_port("https://HOST:/repo", "https://host:/repo")]
    #[case::scp("git@HOST:team/repo", "git@host:team/repo")]
    #[case::scp_ipv6("[::1]:repo", "[::1]:repo")]
    #[case::scp_user_literal("u%ZZ@HOST:repo", "u%ZZ@host:repo")]
    #[case::scp_unicode_host("münich:repo", "münich:repo")]
    #[case::userinfo_empty_password("https://user:@HOST/repo", "https://user@host/repo")]
    #[case::userinfo_escaped_colon("https://u%3a:p@h/r", "https://u%3A:p@h/r")]
    #[case::scp_literal("host:a %ZZ", "host:a %ZZ")]
    #[case::double_colon("foo::repo", "foo::repo")]
    fn normalizes_network_location(#[case] source: &str, #[case] expected: &str) {
        assert_eq!(
            canonicalize_user_url(source.as_bytes(), Path::new(".")).unwrap(),
            expected.as_bytes()
        );
    }

    #[rstest]
    #[case::empty("", UserUrlParseError::EmptyPath)]
    #[case::authority("https://", UserUrlParseError::MissingHost)]
    #[case::percent("https://host/%ZZ", UserUrlParseError::InvalidEscape)]
    #[case::decoded_non_utf8("https://host/%ff", UserUrlParseError::InvalidEncoding)]
    #[case::port("https://host:bogus/repo", UserUrlParseError::InvalidPort)]
    #[case::port_zero("https://host:0/repo", UserUrlParseError::InvalidPort)]
    #[case::port_overflow("https://host:65536/repo", UserUrlParseError::InvalidPort)]
    #[case::missing_ssh_path("ssh://host", UserUrlParseError::MissingUrlPath)]
    #[case::missing_ssh_query_path("ssh://host?x", UserUrlParseError::MissingUrlPath)]
    #[case::missing_git_fragment_path("git://host#x", UserUrlParseError::MissingUrlPath)]
    #[case::missing_git_path("git://host", UserUrlParseError::MissingUrlPath)]
    #[case::missing_file_path("file://host", UserUrlParseError::MissingUrlPath)]
    #[case::missing_scp_path("host:", UserUrlParseError::MissingScpPath)]
    #[case::missing_scp_host(":repo", UserUrlParseError::InvalidScp)]
    #[case::space("https://host/a b", UserUrlParseError::InvalidUrl)]
    #[case::unicode_host("https://münich.example/repo", UserUrlParseError::InvalidAuthority)]
    fn rejects_invalid_locations(#[case] source: &str, #[case] expected: UserUrlParseError) {
        let error = canonicalize_user_url(source.as_bytes(), Path::new(".")).unwrap_err();
        assert!(matches!(error, UserUrlError::Parse(actual) if actual == expected));
    }

    #[rstest]
    #[case::missing("missing/a/../b", "missing/b")]
    #[case::colon("./a:b", "a:b")]
    #[case::space("some path ", "some path ")]
    fn resolves_missing_paths(#[case] source: &str, #[case] expected: &str) {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            canonicalize_user_url(source.as_bytes(), &base).unwrap(),
            path_bytes(&base.join(expected)).unwrap()
        );
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0);
    }

    #[test]
    fn dot_resolves_base_without_trailing_separator() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            canonicalize_user_url(b".", &base).unwrap(),
            path_bytes(&base).unwrap()
        );
    }

    #[test]
    fn reports_relative_base_separately_from_parse_errors() {
        assert!(matches!(
            canonicalize_user_url(b"repo", Path::new("relative")),
            Err(UserUrlError::Path(UserUrlPathError::RelativeBase))
        ));
    }

    #[test]
    fn errors_do_not_disclose_credentials() {
        let error = canonicalize_user_url(b"https://user:secret@host:bad/repo", Path::new("."))
            .unwrap_err();
        assert!(!format!("{error:?}: {error}").contains("secret"));
    }

    #[cfg(unix)]
    #[rstest]
    #[case::link_parent("link/../repo", "real/repo")]
    #[case::dangling_parent("dangling/../repo", "missing/repo")]
    #[case::regular_file_prefix("file/child", "file/child")]
    fn resolves_existing_components(#[case] source: &str, #[case] expected: &str) {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(base.join("real/nested")).unwrap();
        std::fs::write(base.join("file"), b"content").unwrap();
        std::os::unix::fs::symlink(base.join("real/nested"), base.join("link")).unwrap();
        std::os::unix::fs::symlink("missing/target", base.join("dangling")).unwrap();
        assert_eq!(
            canonicalize_user_url(source.as_bytes(), &base).unwrap(),
            path_bytes(&base.join(expected)).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn loop_is_a_bounded_path_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("loop", dir.path().join("loop")).unwrap();
        assert!(matches!(
            canonicalize_user_url(b"loop/x", dir.path()),
            Err(UserUrlError::Path(UserUrlPathError::SymlinkLimit))
        ));
        assert_eq!(
            std::fs::read_link(dir.path().join("loop")).unwrap(),
            Path::new("loop")
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_url_resolves_links_and_retains_literal_percent_and_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        std::os::unix::fs::symlink("target", base.join("link")).unwrap();
        let source = format!("file://HOST{}/link/%ZZ  ", base.display());
        let expected = format!("file://HOST{}/target/%ZZ  ", base.display());
        assert_eq!(
            canonicalize_user_url(source.as_bytes(), &base).unwrap(),
            expected.as_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_base_and_symlink_target() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let target = std::ffi::OsStr::from_bytes(b"target\xff");
        std::os::unix::fs::symlink(target, base.join("link")).unwrap();
        assert_eq!(
            canonicalize_user_url(b"link/repo", &base).unwrap(),
            base.join(target).join("repo").as_os_str().as_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn uninspectable_path_is_preserved_without_access_validation() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let source = vec![b'a'; 4096];
        assert_eq!(
            canonicalize_user_url(&source, &base).unwrap(),
            [path_bytes(&base).unwrap(), b"/".to_vec(), source].concat()
        );
    }

    #[cfg(windows)]
    #[rstest]
    #[case::drive(br"C:\repos\missing\..\repo", b"C:/repos/repo")]
    #[case::drive_relative(br"C:missing\..\repo", b"C:/base/repo")]
    #[case::root_relative(br"\missing\..\repo", b"C:/repo")]
    #[case::unc(br"\\server\share\missing\..\repo", b"//server/share/repo")]
    fn windows_paths(#[case] source: &[u8], #[case] expected: &[u8]) {
        assert_eq!(
            canonicalize_user_url(source, Path::new(r"C:\base")).unwrap(),
            expected
        );
    }
}
