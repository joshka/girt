use super::Definition;
use crate::remote::{ParsedUrl, Protocol};

impl Definition {
    /// Resolves a relative submodule URL against an explicitly selected superproject remote.
    ///
    /// `./child` appends to the remote repository path; `../child` selects its sibling. This is
    /// Git's repository-relative rule, rather than web URL resolution. Only leading `./` and `../`
    /// components are processed: later path bytes, percent escapes and credentials are preserved.
    /// Absolute/non-relative URLs are returned unchanged without consulting `superproject_url`.
    /// No network access, URL rewriting or protocol authorization occurs. Avoid logging returned
    /// URLs because they can contain credentials.
    ///
    /// # Errors
    ///
    /// Returns [`UrlError`] for missing/unsafe URL bytes, malformed relative bases, remote-helper
    /// bases, query/fragment bases, or traversal beyond the remote path's root. Git can produce
    /// unusual results by traversing an authority or scp host; this API explicitly refuses those
    /// cases instead of changing the endpoint. Relative local bases retain their relativity.
    ///
    /// ```
    /// use girt::submodule::Modules;
    /// let modules = Modules::parse(b"[submodule \"math\"]\nurl = ../math.git\n")?;
    /// let url = modules
    ///     .get(b"math")
    ///     .unwrap()
    ///     .resolve_url(b"https://example.com/team/parent.git")?;
    /// assert_eq!(url, b"https://example.com/team/math.git");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn resolve_url(&self, superproject_url: &[u8]) -> Result<Vec<u8>, UrlError> {
        let url = self
            .url()
            .filter(|url| safe_url(url))
            .ok_or(UrlError::Invalid)?;
        if !relative(url) {
            return Ok(url.to_vec());
        }
        if !safe_url(superproject_url) {
            return Err(UrlError::Invalid);
        }
        let parsed = ParsedUrl::parse(superproject_url).map_err(|_| UrlError::Invalid)?;
        if parsed.protocol() == Protocol::Helper
            || superproject_url
                .iter()
                .any(|byte| matches!(byte, b'?' | b'#'))
        {
            return Err(UrlError::UnsupportedBase);
        }
        let prefix_len = superproject_url.len() - parsed.path().len();
        let prefix = &superproject_url[..prefix_len];
        let mut base = parsed.path();
        while let Some(rest) = base.strip_prefix(b"./") {
            base = rest;
        }
        let rooted = base.starts_with(b"/");
        let mut path = base.to_vec();
        while path.ends_with(b"/") {
            path.pop();
        }
        let mut remaining = url;
        while relative(remaining) {
            if let Some(rest) = remaining.strip_prefix(b"./") {
                remaining = rest;
            } else if let Some(rest) = remaining.strip_prefix(b"../") {
                let component = path.rsplit(|byte| *byte == b'/').next().unwrap_or_default();
                if path.is_empty() || component == b".." {
                    return Err(UrlError::AboveRoot);
                }
                match path.iter().rposition(|byte| *byte == b'/') {
                    Some(end) => path.truncate(end),
                    None => path.clear(),
                }
                remaining = rest;
            }
        }
        let mut resolved = prefix.to_vec();
        if rooted && !path.starts_with(b"/") {
            resolved.push(b'/');
        }
        resolved.extend_from_slice(&path);
        if !path.is_empty() || (!prefix.is_empty() && !rooted && !prefix.ends_with(b":")) {
            resolved.push(b'/');
        }
        resolved.extend_from_slice(remaining);
        Ok(resolved)
    }
}

fn safe_url(url: &[u8]) -> bool {
    !url.is_empty() && !url.starts_with(b"-") && !url.iter().any(u8::is_ascii_control)
}

fn relative(url: &[u8]) -> bool {
    url.starts_with(b"./") || url.starts_with(b"../")
}

/// A relative submodule URL could not be resolved without guessing or changing its endpoint.
///
/// Error messages omit URL values because they can contain credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum UrlError {
    /// A required URL is missing, empty, malformed, option-like or contains control bytes.
    #[error("invalid submodule URL or superproject remote")]
    Invalid,
    /// Relative resolution is undefined here for helper URLs or query/fragment bases.
    #[error("unsupported superproject remote for relative submodule URL")]
    UnsupportedBase,
    /// Parent traversal would consume a root, authority, host or unresolved parent component.
    #[error("relative submodule URL traverses beyond the remote path")]
    AboveRoot,
}
