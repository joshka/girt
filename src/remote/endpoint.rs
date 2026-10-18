//! Pure destination rewriting and protocol selection for configured remotes.

use std::path::PathBuf;

use super::{Remote, RemoteUrls};
use crate::Config;
use crate::config::boolean;

/// The endpoint a transfer should connect to, after `insteadOf` rewriting and protocol policy.
///
/// Returned by [`Remote::fetch_destination`] and [`Remote::push_destinations`]. Its
/// [`Protocol`] selects the transport. Bytes are preserved, including paths and credentials;
/// Debug output deliberately omits them.
///
/// # Examples
///
/// ```
/// use girt::Config;
/// use girt::remote::{Protocol, ProtocolEnvironment, Remote};
///
/// let config = Config::parse(b"[remote \"origin\"]\nurl = file:///srv/git/project%20one.git\n")?;
/// let origin = Remote::find(&config, b"origin")?.expect("configured");
/// let destination = origin.fetch_destination(&config, &ProtocolEnvironment::default())?;
/// assert_eq!(destination.protocol(), Protocol::File);
/// // `file://` URLs are percent-decoded into a filesystem path.
/// # #[cfg(unix)]
/// assert_eq!(
///     destination.local_path()?,
///     std::path::Path::new("/srv/git/project one.git")
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Destination {
    bytes: Vec<u8>,
    protocol: Protocol,
}

impl std::fmt::Debug for Destination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Destination")
            .field("protocol", &self.protocol)
            .finish_non_exhaustive()
    }
}

impl Destination {
    /// Exact rewritten bytes. Callers must avoid logging or displaying embedded secrets.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// Recognized endpoint family. Recognition does not authorize transport execution.
    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// Converts a local path or `file://` URL to an OS path without shell interpretation.
    ///
    /// File URLs require an absolute path and an empty or `localhost` authority. Percent escapes
    /// are decoded once; NUL and malformed escapes are rejected. Other schemes are refused.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::Syntax`] for a nonlocal protocol or malformed file URL, including
    /// invalid percent escapes or NUL in decoded file-URL bytes. On non-Unix platforms, paths must
    /// also be UTF-8. No filesystem access is performed.
    pub fn local_path(&self) -> Result<PathBuf, EndpointError> {
        let bytes = match self.protocol {
            Protocol::Local => self.bytes.clone(),
            Protocol::File => decode_file_url(&self.bytes)?,
            _ => return Err(EndpointError::Syntax),
        };
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            Ok(std::ffi::OsString::from_vec(bytes).into())
        }
        #[cfg(not(unix))]
        {
            let value = String::from_utf8(bytes).map_err(|_| EndpointError::Syntax)?;
            #[cfg(windows)]
            let value = if self.protocol == Protocol::File
                && value.as_bytes().first() == Some(&b'/')
                && value.as_bytes().get(2) == Some(&b':')
            {
                value[1..].to_owned()
            } else {
                value
            };
            Ok(PathBuf::from(value))
        }
    }

    /// Exact custom scheme for a helper destination; never an executable name or authorization.
    ///
    /// Git would run a program named `git-remote-<scheme>` for such a URL. girt does not run remote
    /// helpers; the scheme is reported so an application can decide what to do, and remote helpers
    /// are refused unless policy allows them.
    ///
    /// # Examples
    ///
    /// ```
    /// use girt::Config;
    /// use girt::remote::{Protocol, ProtocolEnvironment, Remote};
    ///
    /// let config = Config::parse(
    ///     b"[protocol \"hg\"]\nallow = always\n[remote \"origin\"]\nurl = hg::https://example.com/project\n",
    /// )?;
    /// let origin = Remote::find(&config, b"origin")?.expect("configured");
    /// let destination = origin.fetch_destination(&config, &ProtocolEnvironment::default())?;
    /// assert_eq!(destination.protocol(), Protocol::Helper);
    /// assert_eq!(destination.helper_scheme(), Some(&b"hg"[..]));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn helper_scheme(&self) -> Option<&[u8]> {
        (self.protocol == Protocol::Helper)
            .then(|| helper_scheme(&self.bytes))
            .flatten()
    }
}

fn decode_file_url(url: &[u8]) -> Result<Vec<u8>, EndpointError> {
    let rest = url.strip_prefix(b"file://").ok_or(EndpointError::Syntax)?;
    let slash = rest
        .iter()
        .position(|byte| *byte == b'/')
        .ok_or(EndpointError::Syntax)?;
    let authority = &rest[..slash];
    if !authority.is_empty() && authority != b"localhost" {
        return Err(EndpointError::Syntax);
    }
    let mut path = Vec::with_capacity(rest.len() - slash);
    let mut input = &rest[slash..];
    while let Some((&first, tail)) = input.split_first() {
        if first == b'%' {
            let [a, b, ..] = tail else {
                return Err(EndpointError::Syntax);
            };
            let high = hex(*a).ok_or(EndpointError::Syntax)?;
            let low = hex(*b).ok_or(EndpointError::Syntax)?;
            path.push((high << 4) | low);
            input = &tail[2..];
        } else {
            path.push(first);
            input = tail;
        }
    }
    if path.contains(&0) {
        return Err(EndpointError::Syntax);
    }
    Ok(path)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn helper_scheme(raw: &[u8]) -> Option<&[u8]> {
    let colon = raw.iter().position(|byte| *byte == b':')?;
    let scheme = &raw[..colon];
    (!scheme.is_empty()
        && scheme
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+.-".contains(byte)))
    .then_some(scheme)
}

/// Recognized Git endpoint family. Helper recognition never authorizes an executable.
///
/// # Examples
///
/// ```
/// use girt::remote::{ParsedUrl, Protocol};
///
/// let protocol = |url: &[u8]| ParsedUrl::parse(url).map(|url| url.protocol());
/// assert_eq!(protocol(b"https://example.com/repo.git")?, Protocol::Https);
/// assert_eq!(protocol(b"git@example.com:repo.git")?, Protocol::Ssh);
/// assert_eq!(protocol(b"/srv/repo.git")?, Protocol::Local);
/// assert_eq!(protocol(b"hg::https://example.com/repo")?, Protocol::Helper);
/// # Ok::<(), girt::remote::UrlError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Protocol {
    /// A local path: absolute, `./` or `../` relative, UNC, drive-letter, or any string without a
    /// recognized scheme or scp-like host.
    Local,
    /// A `file://` URL naming a local path.
    File,
    /// An `ssh://` URL or scp-like `host:path` form.
    Ssh,
    /// An `http://` URL.
    Http,
    /// An `https://` URL.
    Https,
    /// A `git://` URL.
    Git,
    /// A `scheme::address` or unrecognized `scheme://` form handled by a remote helper.
    Helper,
}

impl Protocol {
    fn name(self) -> &'static [u8] {
        match self {
            Self::Local => b"file",
            Self::File => b"file",
            Self::Ssh => b"ssh",
            Self::Http => b"http",
            Self::Https => b"https",
            Self::Git => b"git",
            Self::Helper => b"helper",
        }
    }
}

/// Sanitized configuration failure. No URL, path, or environment value appears in Display/Debug.
///
/// - [`Missing`](Self::Missing): the remote has no URL for this direction. Add one with
///   [`RemoteConfig`](super::RemoteConfig).
/// - [`Refused`](Self::Refused): `GIT_ALLOW_PROTOCOL`, `protocol.allow` or `protocol.<name>.allow`
///   forbids the URL's protocol. This is policy, not a broken URL; tell the user which setting
///   applies rather than retrying.
/// - [`Syntax`](Self::Syntax): after `insteadOf` rewriting, the URL is not a recognizable local
///   path, `file://` URL or network URL.
/// - [`Configuration`](Self::Configuration): an `insteadOf` rule or protocol policy value is
///   malformed, such as `protocol.allow = sometimes`.
///
/// # Examples
///
/// ```
/// use girt::Config;
/// use girt::remote::{EndpointError, ProtocolEnvironment, Remote};
///
/// let config = Config::parse(b"[remote \"origin\"]\nurl = https://example.com/repo.git\n")?;
/// let origin = Remote::find(&config, b"origin")?.expect("configured");
/// // As with Git, a `GIT_ALLOW_PROTOCOL` list replaces the configured policy.
/// let ssh_only = ProtocolEnvironment::default().with_allow_protocol(Some(b"ssh".to_vec()));
/// assert_eq!(
///     origin.fetch_destination(&config, &ssh_only),
///     Err(EndpointError::Refused)
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum EndpointError {
    /// Remote has no destination for this direction.
    #[error("remote has no destination")]
    Missing,
    /// A rewrite or protocol setting is missing or malformed.
    #[error("invalid transport configuration: {0}")]
    Configuration(&'static str),
    /// Endpoint syntax does not identify a supported family.
    #[error("invalid endpoint syntax")]
    Syntax,
    /// The effective policy refuses the recognized protocol.
    #[error("transport protocol is refused by policy")]
    Refused,
}

/// Whether protocol use was initiated by the user or by an automatic operation.
///
/// Git treats some protocols, by default local paths and `file://`, as safe only when the user asked
/// for them directly. A tool doing work automatically, such as a submodule update triggered by a
/// clone, sets `GIT_PROTOCOL_FROM_USER=0`, and those protocols are refused.
///
/// # Examples
///
/// ```
/// use girt::Config;
/// use girt::remote::{EndpointError, ProtocolEnvironment, Remote};
///
/// let config = Config::parse(b"[remote \"origin\"]\nurl = file:///srv/git/project.git\n")?;
/// let origin = Remote::find(&config, b"origin")?.expect("configured");
///
/// // Started by the user: allowed.
/// assert!(
///     origin
///         .fetch_destination(&config, &ProtocolEnvironment::default())
///         .is_ok()
/// );
///
/// // Automatic: refused, as Git refuses it.
/// let automatic = ProtocolEnvironment::from_environment(|name| {
///     (name == "GIT_PROTOCOL_FROM_USER").then(|| b"0".to_vec())
/// })?;
/// assert_eq!(
///     origin.fetch_destination(&config, &automatic),
///     Err(EndpointError::Refused)
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum ProtocolOrigin {
    /// Allow protocols configured with the `user` policy.
    User,
    /// Refuse protocols requiring user initiation.
    Automatic,
}

/// The environment variables that restrict which protocols a transfer may use.
///
/// `GIT_ALLOW_PROTOCOL` is a colon-separated list of allowed protocol names; when present it
/// replaces `protocol.allow` and `protocol.<name>.allow` entirely. `GIT_PROTOCOL_FROM_USER=0`
/// marks the operation as automatic, which refuses protocols whose policy is `user` (by default
/// `file` and local paths). The default value of this type applies configuration only.
///
/// # Examples
///
/// ```
/// use girt::remote::{ProtocolEnvironment, ProtocolOrigin};
///
/// let environment = ProtocolEnvironment::from_environment(|name| match name {
///     "GIT_ALLOW_PROTOCOL" => Some(b"https:ssh".to_vec()),
///     "GIT_PROTOCOL_FROM_USER" => Some(b"0".to_vec()),
///     _ => None,
/// })?;
/// assert_eq!(
///     environment.allow_protocol.as_deref(),
///     Some(&b"https:ssh"[..])
/// );
/// assert_eq!(environment.origin, Some(ProtocolOrigin::Automatic));
/// # Ok::<(), girt::remote::EndpointError>(())
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ProtocolEnvironment {
    /// `GIT_ALLOW_PROTOCOL`, split on `:`; an empty list allows no protocols.
    pub allow_protocol: Option<Vec<u8>>,
    /// `GIT_PROTOCOL_FROM_USER`; automatic use refuses protocols with `user` policy.
    pub origin: Option<ProtocolOrigin>,
}

impl ProtocolEnvironment {
    /// Sets [`Self::allow_protocol`].
    #[must_use]
    pub fn with_allow_protocol(mut self, allow_protocol: Option<Vec<u8>>) -> Self {
        self.allow_protocol = allow_protocol;
        self
    }

    /// Sets [`Self::origin`].
    #[must_use]
    pub fn with_origin(mut self, origin: Option<ProtocolOrigin>) -> Self {
        self.origin = origin;
        self
    }
}

impl ProtocolEnvironment {
    /// Decodes supplied transport environment bytes without reading process-global state.
    ///
    /// A present empty `GIT_ALLOW_PROTOCOL` allows no protocol. Invalid boolean bytes fail with
    /// a value-free diagnostic. Credential and transport-specific inputs are supplied to their
    /// respective adapters after protocol selection.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::Configuration`] if the allow-list contains NUL or
    /// `GIT_PROTOCOL_FROM_USER` is not a recognized boolean. No transport is started.
    pub fn from_environment(
        mut get: impl FnMut(&str) -> Option<Vec<u8>>,
    ) -> Result<Self, EndpointError> {
        let allow_protocol = get("GIT_ALLOW_PROTOCOL");
        if allow_protocol
            .as_ref()
            .is_some_and(|value| value.contains(&0))
        {
            return Err(EndpointError::Configuration("protocol allow list"));
        }
        let origin = get("GIT_PROTOCOL_FROM_USER")
            .map(|value| {
                boolean(Some(&value))
                    .map(|user| {
                        if user {
                            ProtocolOrigin::User
                        } else {
                            ProtocolOrigin::Automatic
                        }
                    })
                    .ok_or(EndpointError::Configuration("protocol user policy"))
            })
            .transpose()?;
        Ok(Self {
            allow_protocol,
            origin,
        })
    }
}

impl Remote {
    /// Resolves the first fetch URL, applying `insteadOf` and protocol policy once.
    ///
    /// A push-only rewrite is never applied to fetch. No filesystem or process environment is read.
    /// This method does not perform transport I/O or validate transport-specific components.
    ///
    /// Without configuration, local paths and `file://` are allowed unless the
    /// [`ProtocolEnvironment`] marks the operation as automatic, remote helpers are refused, and
    /// `http`, `https`, `ssh` and `git` are allowed.
    ///
    /// # Examples
    ///
    /// ```
    /// use girt::Config;
    /// use girt::remote::{Protocol, ProtocolEnvironment, Remote};
    ///
    /// let config = Config::parse(
    ///     b"[url \"https://github.com/\"]\ninsteadOf = gh:\n\
    ///       [remote \"origin\"]\nurl = gh:joshka/girt\n",
    /// )?;
    /// let origin = Remote::find(&config, b"origin")?.expect("configured");
    /// let destination = origin.fetch_destination(&config, &ProtocolEnvironment::default())?;
    /// assert_eq!(destination.bytes(), b"https://github.com/joshka/girt");
    /// assert_eq!(destination.protocol(), Protocol::Https);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::Missing`] when no fetch URL is configured, configuration or syntax
    /// errors for invalid rewrites/endpoints, or [`EndpointError::Refused`] when policy denies the
    /// recognized protocol. No transfer is attempted.
    pub fn fetch_destination(
        &self,
        config: &Config,
        env: &ProtocolEnvironment,
    ) -> Result<Destination, EndpointError> {
        let raw = self.fetch_url().ok_or(EndpointError::Missing)?;
        resolve(raw, config, env, false, true)
    }

    /// Resolves every push destination in order, including duplicates.
    ///
    /// Configured `pushurl` entries override `url` and receive only `insteadOf` rewrites. When
    /// falling back to `url`, the longest `pushInsteadOf` match wins over `insteadOf`.
    ///
    /// # Examples
    ///
    /// Fetch anonymously over HTTPS but push over SSH, a common `pushInsteadOf` setup:
    ///
    /// ```
    /// use girt::Config;
    /// use girt::remote::{Protocol, ProtocolEnvironment, Remote};
    ///
    /// let config = Config::parse(
    ///     b"[url \"git@example.com:\"]\npushInsteadOf = https://example.com/\n\
    ///       [remote \"origin\"]\nurl = https://example.com/project.git\n",
    /// )?;
    ///
    /// let origin = Remote::find(&config, b"origin")?.expect("configured");
    /// let environment = ProtocolEnvironment::default();
    /// assert_eq!(
    ///     origin.fetch_destination(&config, &environment)?.protocol(),
    ///     Protocol::Https
    /// );
    ///
    /// let push = origin.push_destinations(&config, &environment)?;
    /// assert_eq!(push[0].bytes(), b"git@example.com:project.git");
    /// assert_eq!(push[0].protocol(), Protocol::Ssh);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns configuration or syntax errors for invalid rewrites/endpoints, or
    /// [`EndpointError::Refused`] when policy denies a destination's protocol. No transfer is
    /// attempted, including when an earlier destination resolved successfully.
    pub fn push_destinations(
        &self,
        config: &Config,
        env: &ProtocolEnvironment,
    ) -> Result<Vec<Destination>, EndpointError> {
        self.push_urls()
            .iter()
            .map(|raw| {
                resolve(
                    raw,
                    config,
                    env,
                    self.configured_push_urls().is_empty(),
                    true,
                )
            })
            .collect()
    }
}

impl RemoteUrls {
    /// Returns the first fetch URL after `insteadOf` rewriting, for display.
    ///
    /// The result preserves bytes and may contain credentials. It does not classify the URL,
    /// apply protocol policy, validate transport syntax or perform I/O. `None` means no URL is
    /// configured after resets. The supplied configuration provides rewrite rules and should be
    /// the same snapshot used by [`Self::find`].
    ///
    /// # Errors
    ///
    /// Malformed rewrite settings produce [`EndpointError::Configuration`].
    pub fn fetch_display_url(&self, config: &Config) -> Result<Option<Vec<u8>>, EndpointError> {
        self.fetch_url()
            .map(|raw| rewrite_url(raw, config, false))
            .transpose()
    }

    /// Returns the first push URL after Git-style rewriting, for display.
    ///
    /// An explicit `pushurl` wins over `url` and receives only `insteadOf` rewriting. When
    /// falling back to `url`, a matching `pushInsteadOf` takes priority over `insteadOf`. The
    /// result preserves bytes and may contain credentials; it performs no protocol validation,
    /// authorization or I/O. `None` means neither URL key has a value after resets.
    ///
    /// # Errors
    ///
    /// Malformed rewrite settings produce [`EndpointError::Configuration`].
    ///
    /// # Examples
    ///
    /// Show where `git remote -v` would say a remote fetches from and pushes to:
    ///
    /// ```
    /// use girt::Config;
    /// use girt::remote::RemoteUrls;
    ///
    /// let config = Config::parse(
    ///     b"[url \"git@example.com:\"]\npushInsteadOf = https://example.com/\n\
    ///       [remote \"origin\"]\nurl = https://example.com/project.git\n",
    /// )?;
    ///
    /// let urls = RemoteUrls::find(&config, b"origin")?.expect("configured");
    /// assert_eq!(
    ///     urls.fetch_display_url(&config)?,
    ///     Some(b"https://example.com/project.git".to_vec())
    /// );
    /// assert_eq!(
    ///     urls.push_display_url(&config)?,
    ///     Some(b"git@example.com:project.git".to_vec())
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn push_display_url(&self, config: &Config) -> Result<Option<Vec<u8>>, EndpointError> {
        self.push_urls()
            .first()
            .map(|raw| rewrite_url(raw, config, self.configured_push_urls().is_empty()))
            .transpose()
    }
}

fn resolve(
    raw: &[u8],
    config: &Config,
    env: &ProtocolEnvironment,
    push_rewrite: bool,
    user: bool,
) -> Result<Destination, EndpointError> {
    let bytes = rewrite_url(raw, config, push_rewrite)?;
    let protocol = classify(&bytes)?;
    if !allowed(protocol, &bytes, config, env, user)? {
        return Err(EndpointError::Refused);
    }
    Ok(Destination { bytes, protocol })
}

fn rewrite_url(raw: &[u8], config: &Config, push_rewrite: bool) -> Result<Vec<u8>, EndpointError> {
    let replacement = if push_rewrite {
        rewrite(raw, config, "pushinsteadof")?
    } else {
        None
    };
    Ok(match replacement.or(rewrite(raw, config, "insteadof")?) {
        Some((prefix, matched)) => [prefix, &raw[matched..]].concat(),
        None => raw.to_vec(),
    })
}

fn rewrite<'a>(
    raw: &[u8],
    config: &'a Config,
    key: &str,
) -> Result<Option<(&'a [u8], usize)>, EndpointError> {
    let mut best = None;
    for entry in config.entries() {
        if !entry.section.eq_ignore_ascii_case(b"url")
            || !entry.name.eq_ignore_ascii_case(key.as_bytes())
        {
            continue;
        }
        let prefix = entry
            .subsection
            .as_deref()
            .ok_or(EndpointError::Configuration("rewrite prefix"))?;
        let value = entry
            .value
            .as_deref()
            .ok_or(EndpointError::Configuration("rewrite match"))?;
        if value.is_empty() {
            return Err(EndpointError::Configuration("empty rewrite match"));
        }
        if raw.starts_with(value) && best.is_none_or(|(_, n)| value.len() > n) {
            best = Some((prefix, value.len()));
        }
    }
    Ok(best)
}

fn classify(raw: &[u8]) -> Result<Protocol, EndpointError> {
    if raw.is_empty() || raw.contains(&0) {
        return Err(EndpointError::Syntax);
    }
    if raw.starts_with(b"/")
        || raw.starts_with(b"./")
        || raw.starts_with(b"../")
        || raw.starts_with(b"\\\\")
        || (raw.len() >= 3
            && raw[0].is_ascii_alphabetic()
            && raw[1] == b':'
            && matches!(raw[2], b'/' | b'\\'))
    {
        return Ok(Protocol::Local);
    }
    if let Some(colon) = raw.iter().position(|b| *b == b':') {
        let scheme = &raw[..colon];
        if raw.get(colon + 1) == Some(&b':') && helper_scheme(raw).is_some() {
            return Ok(Protocol::Helper);
        }
        if raw.get(colon + 1..colon + 3) == Some(b"//") {
            return match scheme {
                b"file" => Ok(Protocol::File),
                b"ssh" => Ok(Protocol::Ssh),
                b"http" => Ok(Protocol::Http),
                b"https" => Ok(Protocol::Https),
                b"git" => Ok(Protocol::Git),
                _ if !scheme.is_empty() => Ok(Protocol::Helper),
                _ => Err(EndpointError::Syntax),
            };
        }
        if !scheme.contains(&b'/') && !scheme.is_empty() {
            return Ok(Protocol::Ssh);
        }
    }
    Ok(Protocol::Local)
}

fn allowed(
    protocol: Protocol,
    raw: &[u8],
    config: &Config,
    env: &ProtocolEnvironment,
    user: bool,
) -> Result<bool, EndpointError> {
    let name = if protocol == Protocol::Helper {
        helper_scheme(raw).ok_or(EndpointError::Syntax)?
    } else {
        protocol.name()
    };
    if let Some(list) = &env.allow_protocol {
        return Ok(list.split(|b| *b == b':').any(|entry| entry == name));
    }
    let section = name;
    let policy = config
        .value("protocol", Some(section), "allow")
        .or_else(|| config.value("protocol", None, "allow"));
    let policy = match policy {
        None => match protocol {
            Protocol::Helper => b"never".as_slice(),
            Protocol::Local | Protocol::File => b"user",
            _ => b"always",
        },
        Some(Some(value)) => value,
        Some(None) => return Err(EndpointError::Configuration("protocol policy")),
    };
    match policy {
        b"always" => Ok(true),
        b"never" => Ok(false),
        b"user" => Ok(user && env.origin.unwrap_or(ProtocolOrigin::User) == ProtocolOrigin::User),
        _ => Err(EndpointError::Configuration("protocol policy")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_rewrite_and_push_precedence() {
        let config = Config::parse(b"[remote \"r\"]\nurl = short:long/repo\npushurl = https://push/repo\n[url \"ssh://short/\"]\ninsteadOf = short:\n[url \"ssh://long/\"]\ninsteadOf = short:long\n[url \"ssh://push/\"]\npushInsteadOf = short:\n").unwrap();
        let remote = Remote::find(&config, b"r").unwrap().unwrap();
        assert_eq!(
            remote
                .fetch_destination(&config, &ProtocolEnvironment::default())
                .unwrap()
                .bytes(),
            b"ssh://long//repo"
        );
        assert_eq!(
            remote
                .push_destinations(&config, &ProtocolEnvironment::default())
                .unwrap()[0]
                .bytes(),
            b"https://push/repo"
        );
    }

    #[test]
    fn push_rewrite_wins_only_for_url_fallback() {
        let config = Config::parse(b"[remote \"r\"]\nurl = short:repo\n[url \"ssh://ordinary/\"]\ninsteadOf = short:\n[url \"ssh://push/\"]\npushInsteadOf = short:\n").unwrap();
        let remote = Remote::find(&config, b"r").unwrap().unwrap();
        assert_eq!(
            remote
                .fetch_destination(&config, &ProtocolEnvironment::default())
                .unwrap()
                .bytes(),
            b"ssh://ordinary/repo"
        );
        assert_eq!(
            remote
                .push_destinations(&config, &ProtocolEnvironment::default())
                .unwrap()[0]
                .bytes(),
            b"ssh://push/repo"
        );
    }

    #[test]
    fn display_urls_use_longest_rewrites_and_push_precedence() {
        let config = Config::parse(b"[remote \"r\"]\nurl=short:long/repo\nurl=short:other\n[url \"ssh://short/\"]\ninsteadOf=short:\n[url \"ssh://long/\"]\ninsteadOf=short:long\n[url \"ssh://push/\"]\npushInsteadOf=short:\n").unwrap();
        let remote = RemoteUrls::find(&config, b"r").unwrap().unwrap();
        assert_eq!(remote.urls().len(), 2);
        assert_eq!(
            remote.fetch_display_url(&config).unwrap(),
            Some(b"ssh://long//repo".to_vec())
        );
        assert_eq!(
            remote.push_display_url(&config).unwrap(),
            Some(b"ssh://push/long/repo".to_vec())
        );
    }

    #[test]
    fn explicit_pushurl_receives_only_ordinary_rewrite() {
        let config = Config::parse(b"[remote \"r\"]\nurl=short:fetch\npushurl=short:push\npushurl=short:later\n[url \"ssh://ordinary/\"]\ninsteadOf=short:\n[url \"ssh://push/\"]\npushInsteadOf=short:\n").unwrap();
        let remote = RemoteUrls::find(&config, b"r").unwrap().unwrap();
        assert_eq!(remote.configured_push_urls().len(), 2);
        assert_eq!(
            remote.push_display_url(&config).unwrap(),
            Some(b"ssh://ordinary/push".to_vec())
        );
    }

    #[test]
    fn display_urls_ignore_protocol_policy_and_endpoint_syntax() {
        let config = Config::parse(
            b"[remote \"r\"]\nurl=custom://opaque\n[protocol \"custom\"]\nallow=never\n",
        )
        .unwrap();
        let remote = RemoteUrls::find(&config, b"r").unwrap().unwrap();
        assert_eq!(
            remote.fetch_display_url(&config).unwrap(),
            Some(b"custom://opaque".to_vec())
        );
        assert_eq!(
            remote.push_display_url(&config).unwrap(),
            Some(b"custom://opaque".to_vec())
        );
        assert_eq!(
            Remote::find(&config, b"r")
                .unwrap()
                .unwrap()
                .fetch_destination(&config, &ProtocolEnvironment::default()),
            Err(EndpointError::Refused)
        );
    }

    #[test]
    fn policy_refuses_before_execution_and_redacts_debug() {
        let config = Config::parse(b"[remote \"r\"]\nurl = https://secret:token@example.test/path\n[protocol \"https\"]\nallow = never\n").unwrap();
        let remote = Remote::find(&config, b"r").unwrap().unwrap();
        assert_eq!(
            remote.fetch_destination(&config, &ProtocolEnvironment::default()),
            Err(EndpointError::Refused)
        );
        let allowed = remote
            .fetch_destination(
                &config,
                &ProtocolEnvironment {
                    allow_protocol: Some(b"https".to_vec()),
                    origin: None,
                },
            )
            .unwrap();
        assert!(!format!("{allowed:?}").contains("secret"));
        assert_eq!(allowed.bytes(), b"https://secret:token@example.test/path");
    }

    #[test]
    fn file_policy_respects_user_context_and_allow_list() {
        let config = Config::parse(b"[remote \"r\"]\nurl = ./relative\n").unwrap();
        let remote = Remote::find(&config, b"r").unwrap().unwrap();
        assert_eq!(
            remote.fetch_destination(
                &config,
                &ProtocolEnvironment {
                    allow_protocol: None,
                    origin: Some(ProtocolOrigin::Automatic)
                }
            ),
            Err(EndpointError::Refused)
        );
        assert_eq!(
            remote.fetch_destination(
                &config,
                &ProtocolEnvironment {
                    allow_protocol: Some(b"ssh:https".to_vec()),
                    origin: None
                }
            ),
            Err(EndpointError::Refused)
        );
        assert_eq!(
            remote
                .fetch_destination(
                    &config,
                    &ProtocolEnvironment {
                        allow_protocol: Some(b"file".to_vec()),
                        origin: Some(ProtocolOrigin::Automatic)
                    }
                )
                .unwrap()
                .protocol(),
            Protocol::Local
        );
    }

    #[test]
    fn environment_decoding_preserves_empty_allow_list_and_rejects_secret_value() {
        let env = ProtocolEnvironment::from_environment(|name| {
            (name == "GIT_ALLOW_PROTOCOL").then(Vec::new)
        })
        .unwrap();
        assert_eq!(env.allow_protocol, Some(Vec::new()));
        let error = ProtocolEnvironment::from_environment(|name| {
            (name == "GIT_PROTOCOL_FROM_USER").then(|| b"secret".to_vec())
        })
        .unwrap_err();
        assert_eq!(error, EndpointError::Configuration("protocol user policy"));
        assert!(!format!("{error:?}").contains("secret"));
    }

    #[test]
    fn local_paths_and_file_urls_preserve_paths_without_remote_authorities() {
        let config =
            Config::parse(b"[remote \"r\"]\nurl = file://localhost/tmp/a%20b.git\n").unwrap();
        let remote = Remote::find(&config, b"r").unwrap().unwrap();
        let destination = remote
            .fetch_destination(&config, &ProtocolEnvironment::default())
            .unwrap();
        assert_eq!(
            destination.local_path().unwrap(),
            std::path::Path::new("/tmp/a b.git")
        );
        assert_eq!(
            decode_file_url(b"file://host/tmp/repo"),
            Err(EndpointError::Syntax)
        );
        assert_eq!(
            decode_file_url(b"file:///tmp/%00"),
            Err(EndpointError::Syntax)
        );
        assert_eq!(
            decode_file_url(b"file:///tmp/%G0"),
            Err(EndpointError::Syntax)
        );
    }

    #[test]
    fn explicit_helper_syntax_requires_scheme_policy() {
        let config = Config::parse(
            b"[remote \"r\"]\nurl = test::opaque-address\n[protocol \"test\"]\nallow = always\n",
        )
        .unwrap();
        let remote = Remote::find(&config, b"r").unwrap().unwrap();
        let destination = remote
            .fetch_destination(&config, &ProtocolEnvironment::default())
            .unwrap();
        assert_eq!(destination.protocol(), Protocol::Helper);
        assert_eq!(destination.helper_scheme(), Some(b"test".as_slice()));
        assert_eq!(destination.local_path(), Err(EndpointError::Syntax));
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_paths_are_local_and_file_urls_decode_to_drive_paths() {
        let config = Config::parse(b"[remote \"r\"]\nurl = C:/repos/source.git\n").unwrap();
        let remote = Remote::find(&config, b"r").unwrap().unwrap();
        let destination = remote
            .fetch_destination(&config, &ProtocolEnvironment::default())
            .unwrap();
        assert_eq!(destination.protocol(), Protocol::Local);
        assert_eq!(
            destination.local_path().unwrap(),
            std::path::Path::new(r"C:\repos\source.git")
        );
        let config = Config::parse(b"[remote \"r\"]\nurl = file:///C:/repos/source.git\n").unwrap();
        let remote = Remote::find(&config, b"r").unwrap().unwrap();
        let destination = remote
            .fetch_destination(&config, &ProtocolEnvironment::default())
            .unwrap();
        assert_eq!(
            destination.local_path().unwrap(),
            std::path::Path::new(r"C:\repos\source.git")
        );
    }
}
