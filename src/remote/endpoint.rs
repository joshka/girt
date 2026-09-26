//! Pure destination rewriting and protocol selection for configured remotes.

use std::path::PathBuf;

use super::Remote;
use crate::Config;
use crate::config::boolean;

/// One configured destination after a single Git-style prefix rewrite.
///
/// Bytes are preserved, including paths and credentials. Debug output deliberately omits them.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Local,
    File,
    Ssh,
    Http,
    Https,
    Git,
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
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
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

/// Explicit environment policy for one resolution call. A present allow-list overrides config.
#[derive(Debug, Clone, Default)]
pub struct ProtocolEnvironment {
    /// `GIT_ALLOW_PROTOCOL`, split on `:`; an empty list allows no protocols.
    pub allow_protocol: Option<Vec<u8>>,
    /// `GIT_PROTOCOL_FROM_USER`; false refuses protocols with `user` policy.
    pub from_user: Option<bool>,
}

impl ProtocolEnvironment {
    /// Decodes supplied transport environment bytes without reading process-global state.
    ///
    /// A present empty `GIT_ALLOW_PROTOCOL` allows no protocol. Invalid boolean bytes fail with
    /// a value-free diagnostic. Other transport environment belongs to R22–R25.
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
        let from_user = get("GIT_PROTOCOL_FROM_USER")
            .map(|value| {
                boolean(Some(&value)).ok_or(EndpointError::Configuration("protocol user policy"))
            })
            .transpose()?;
        Ok(Self {
            allow_protocol,
            from_user,
        })
    }
}

impl Remote {
    /// Resolves the first fetch URL, applying `insteadOf` and protocol policy once.
    ///
    /// A push-only rewrite is never applied to fetch. No filesystem or process environment is read.
    /// This method does not perform transport I/O or validate transport-specific components.
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

fn resolve(
    raw: &[u8],
    config: &Config,
    env: &ProtocolEnvironment,
    push_rewrite: bool,
    user: bool,
) -> Result<Destination, EndpointError> {
    let replacement = if push_rewrite {
        rewrite(raw, config, "pushinsteadof")?
    } else {
        None
    };
    let bytes = match replacement.or(rewrite(raw, config, "insteadof")?) {
        Some((prefix, matched)) => [prefix, &raw[matched..]].concat(),
        None => raw.to_vec(),
    };
    let protocol = classify(&bytes)?;
    if !allowed(protocol, &bytes, config, env, user)? {
        return Err(EndpointError::Refused);
    }
    Ok(Destination { bytes, protocol })
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
        b"user" => Ok(user && env.from_user.unwrap_or(true)),
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
                    from_user: None,
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
                    from_user: Some(false)
                }
            ),
            Err(EndpointError::Refused)
        );
        assert_eq!(
            remote.fetch_destination(
                &config,
                &ProtocolEnvironment {
                    allow_protocol: Some(b"ssh:https".to_vec()),
                    from_user: None
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
                        from_user: Some(false)
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
