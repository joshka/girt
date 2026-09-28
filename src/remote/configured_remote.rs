//! Validated configured remotes, without transport selection or source discovery.

use super::configured_url::normalize;
use super::{ConfiguredRefspec, ConfiguredRefspecError, ConfiguredUrlError, Direction};
use crate::Config;

/// An active configured remote after validation and URL rewrites.
///
/// This snapshot consumes only caller-supplied configuration. It validates tag options, surviving
/// fetch URLs, surviving push URLs, fetch refspecs, push refspecs, then URL rewrites, in that
/// order. Empty and implicit URL values reset their list; implicit fetch refspecs instead select
/// `HEAD`. All surviving occurrences are validated, including URLs after the first selected
/// destination. Names are exact subsection bytes and are not validated as reference names or
/// filesystem paths.
///
/// URL handling covers ordinary local paths, file URLs, scp-like SSH, SSH URLs and HTTP(S) URLs.
/// ASCII network hosts are lowercased and numeric ports are serialized without leading zeroes.
/// Unknown protocols, IPv6, passwords, percent escapes and other uncharacterized normalization
/// return [`ConfiguredRemoteError::UnsupportedUrlSyntax`]. A caller can then use a compatibility
/// implementation for the **whole remote**, without treating unsupported data as invalid.
/// This type neither authorizes a transport nor changes [`super::Remote`] or
/// [`super::RemoteUrls`]' stricter configuration policies.
#[derive(Clone)]
pub struct ConfiguredRemote {
    fetch_url: Option<Vec<u8>>,
    push_url: Option<Vec<u8>>,
    fetch_refspecs: Vec<ConfiguredRefspec>,
    push_refspecs: Vec<ConfiguredRefspec>,
}

impl std::fmt::Debug for ConfiguredRemote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfiguredRemote")
            .field("has_fetch_url", &self.fetch_url.is_some())
            .field("has_push_url", &self.push_url.is_some())
            .field("fetch_refspec_count", &self.fetch_refspecs.len())
            .field("push_refspec_count", &self.push_refspecs.len())
            .finish()
    }
}

/// A configured remote failed validation, or needs a compatibility URL implementation.
///
/// Occurrences are one-based positions within the original key's values, including values before
/// resets. Errors deliberately omit URL bytes, which may contain credentials. Unsupported syntax
/// is reported only when reached in validation order; preceding malformed values still win.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum ConfiguredRemoteError {
    /// The last explicit tag option is neither `--tags` nor `--no-tags`.
    #[error("remote tagOpt must be --tags or --no-tags")]
    TagOption,
    /// A URL in the supported syntax is malformed.
    #[error("remote {key}, occurrence {occurrence}, rewritten={rewritten}: {source}")]
    Url {
        /// `url` or `pushurl`.
        key: &'static str,
        /// One-based original occurrence of this key.
        occurrence: usize,
        /// Whether validation failed after applying a prefix rewrite.
        rewritten: bool,
        /// Value-free syntax diagnostic.
        #[source]
        source: ConfiguredUrlError,
    },
    /// A configured refspec is malformed.
    #[error("remote {key}, occurrence {occurrence}: {source}")]
    Refspec {
        /// `fetch` or `push`.
        key: &'static str,
        /// One-based occurrence of this key.
        occurrence: usize,
        /// Value-free configured syntax diagnostic.
        #[source]
        source: ConfiguredRefspecError,
    },
    /// The whole remote needs a compatibility implementation for URL syntax or normalization.
    #[error("unsupported remote {key} syntax at occurrence {occurrence}, rewritten={rewritten}")]
    UnsupportedUrlSyntax {
        /// `url` or `pushurl` of the original value.
        key: &'static str,
        /// One-based original occurrence of this key.
        occurrence: usize,
        /// Whether unsupported syntax was produced by a prefix rewrite.
        rewritten: bool,
    },
}

impl ConfiguredRemote {
    /// Resolves one remote without filesystem, environment, or network access.
    ///
    /// Returns `None` for absent or URL-free remotes, **after** validation. Thus a URL-free remote
    /// with an invalid tag option or refspec still fails. Only the last tag option matters; an
    /// implicit last option clears preceding options. Empty explicit tag options are invalid.
    /// Refspecs retain configuration order and duplicates, leaving selection policy to callers.
    ///
    /// Ordinary `insteadOf` rewrites match canonical URL bytes. The longest prefix wins, with the
    /// first equal-length match retained. Empty and implicit match values match the empty prefix.
    /// Explicit push URLs disable `pushInsteadOf`. Otherwise a malformed supported push-only
    /// rewrite retains that occurrence's original URL, even if an ordinary rewrite also matches.
    /// Unsupported rewritten syntax always requests whole-remote compatibility handling.
    ///
    /// # Errors
    ///
    /// Returns the first validation failure in the order documented on [`Self`]. No partial
    /// snapshot is returned. An unsupported error is a compatibility boundary, not transport
    /// permission or a claim that the input is malformed.
    ///
    /// ```
    /// use girt::Config;
    /// use girt::remote::ConfiguredRemote;
    /// let config = Config::parse(b"[remote \"origin\"]\nurl=https://HOST/repo\nfetch\n")?;
    /// let remote = ConfiguredRemote::find(&config, b"origin")?.unwrap();
    /// assert_eq!(remote.fetch_url(), Some(b"https://host/repo".as_slice()));
    /// assert_eq!(remote.fetch_refspecs()[0].to_bytes(), b"HEAD");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn find(config: &Config, name: &[u8]) -> Result<Option<Self>, ConfiguredRemoteError> {
        match config.value("remote", Some(name), "tagopt") {
            None | Some(None | Some(b"--tags" | b"--no-tags")) => {}
            _ => return Err(ConfiguredRemoteError::TagOption),
        }
        let fetch = configured_urls(config, name, "url")?;
        let push = configured_urls(config, name, "pushurl")?;
        let fetch_refspecs = configured_refspecs(config, name, "fetch", Direction::Fetch)?;
        let push_refspecs = configured_refspecs(config, name, "push", Direction::Push)?;
        let rewritten_fetch = rewrite_urls(config, &fetch, "url")?;
        let rewritten_push = if push.is_empty() {
            push_fallback_urls(config, &fetch, &rewritten_fetch)?
        } else {
            rewrite_urls(config, &push, "pushurl")?
        };
        if fetch.is_empty() && push.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            fetch_url: rewritten_fetch.into_iter().next(),
            push_url: rewritten_push.into_iter().next(),
            fetch_refspecs,
            push_refspecs,
        }))
    }

    /// First supported serialized fetch URL after resets and ordinary rewrites, if any.
    /// Bytes may contain credentials; do not log them by default.
    pub fn fetch_url(&self) -> Option<&[u8]> {
        self.fetch_url.as_deref()
    }

    /// First supported serialized push URL, or the first fetch URL with push rewrite policy.
    /// Bytes may contain credentials; do not log them by default.
    pub fn push_url(&self) -> Option<&[u8]> {
        self.push_url.as_deref()
    }

    /// Validated fetch descriptors in configuration order, including duplicates and defaults.
    pub fn fetch_refspecs(&self) -> &[ConfiguredRefspec] {
        &self.fetch_refspecs
    }

    /// Validated push descriptors in configuration order, including duplicates.
    pub fn push_refspecs(&self) -> &[ConfiguredRefspec] {
        &self.push_refspecs
    }
}

struct UrlOccurrence {
    index: usize,
    bytes: Vec<u8>,
}

fn configured_urls(
    config: &Config,
    name: &[u8],
    key: &'static str,
) -> Result<Vec<UrlOccurrence>, ConfiguredRemoteError> {
    let mut surviving = Vec::new();
    for (index, value) in config.values("remote", Some(name), key).enumerate() {
        match value {
            None | Some(b"") => surviving.clear(),
            Some(bytes) => surviving.push((index + 1, bytes)),
        }
    }
    surviving
        .into_iter()
        .map(|(index, bytes)| {
            normalize(bytes)
                .map(|bytes| UrlOccurrence { index, bytes })
                .map_err(|err| url_error(err, key, index, false))
        })
        .collect()
}

fn configured_refspecs(
    config: &Config,
    name: &[u8],
    key: &'static str,
    direction: Direction,
) -> Result<Vec<ConfiguredRefspec>, ConfiguredRemoteError> {
    config
        .values("remote", Some(name), key)
        .enumerate()
        .map(|(index, value)| {
            ConfiguredRefspec::parse(direction, value.unwrap_or_default()).map_err(|source| {
                ConfiguredRemoteError::Refspec {
                    key,
                    occurrence: index + 1,
                    source,
                }
            })
        })
        .collect()
}

fn rewrite_urls(
    config: &Config,
    urls: &[UrlOccurrence],
    key: &'static str,
) -> Result<Vec<Vec<u8>>, ConfiguredRemoteError> {
    urls.iter()
        .map(|url| match rewrite(config, &url.bytes, "insteadof") {
            Some(bytes) => normalize(&bytes).map_err(|err| url_error(err, key, url.index, true)),
            None => Ok(url.bytes.clone()),
        })
        .collect()
}

fn push_fallback_urls(
    config: &Config,
    originals: &[UrlOccurrence],
    ordinary: &[Vec<u8>],
) -> Result<Vec<Vec<u8>>, ConfiguredRemoteError> {
    originals
        .iter()
        .zip(ordinary)
        .map(|(url, ordinary)| {
            let Some(bytes) = rewrite(config, &url.bytes, "pushinsteadof") else {
                return Ok(ordinary.clone());
            };
            match normalize(&bytes) {
                Ok(bytes) => Ok(bytes),
                Err(ConfiguredUrlError::Unsupported) => Err(url_error(
                    ConfiguredUrlError::Unsupported,
                    "url",
                    url.index,
                    true,
                )),
                Err(_) => Ok(url.bytes.clone()),
            }
        })
        .collect()
}

fn rewrite(config: &Config, url: &[u8], key: &str) -> Option<Vec<u8>> {
    let mut selected: Option<(&[u8], &[u8])> = None;
    for entry in config.entries() {
        if !entry.section.eq_ignore_ascii_case(b"url")
            || !entry.name.eq_ignore_ascii_case(key.as_bytes())
        {
            continue;
        }
        let Some(base) = entry.subsection.as_deref() else {
            continue;
        };
        let prefix = entry.value.as_deref().unwrap_or_default();
        if url.starts_with(prefix) && selected.is_none_or(|(_, old)| prefix.len() > old.len()) {
            selected = Some((base, prefix));
        }
    }
    selected.map(|(base, prefix)| [base, &url[prefix.len()..]].concat())
}

fn url_error(
    source: ConfiguredUrlError,
    key: &'static str,
    occurrence: usize,
    rewritten: bool,
) -> ConfiguredRemoteError {
    if source == ConfiguredUrlError::Unsupported {
        ConfiguredRemoteError::UnsupportedUrlSyntax {
            key,
            occurrence,
            rewritten,
        }
    } else {
        ConfiguredRemoteError::Url {
            key,
            occurrence,
            rewritten,
            source,
        }
    }
}

#[cfg(test)]
#[path = "configured_remote_tests.rs"]
mod tests;
