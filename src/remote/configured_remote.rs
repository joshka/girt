//! Validated configured remotes, without transport selection or source discovery.

use super::configured_url::{normalize, parse};
use super::user_url::NormalizedUrl;
use super::{
    ConfiguredRefspec, ConfiguredRefspecError, ConfiguredUrlError, ConfiguredUrlParts, Direction,
};
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
/// URL handling uses [`super::normalize_configured_url`], including credentials, IPv6, authority
/// escapes, query/fragment components and custom schemes. Relative local paths are not expanded;
/// normalization never accesses the filesystem. Remaining uncharacterized syntax returns
/// [`ConfiguredRemoteError::UnsupportedUrlSyntax`]. A caller can then use a
/// compatibility implementation for the **whole remote**, without treating unsupported data as
/// invalid. This type neither authorizes a transport nor changes [`super::Remote`] or
/// [`super::RemoteUrls`]' stricter configuration policies.
#[derive(Clone)]
pub struct ConfiguredRemote {
    fetch_url: Option<NormalizedUrl>,
    push_url: Option<NormalizedUrl>,
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
    /// The selected explicit tag option is neither `--tags` nor `--no-tags`.
    #[error("remote tagOpt must be --tags or --no-tags")]
    TagOption,
    /// Retained for compatibility with callers of earlier releases.
    ///
    /// Configuration snapshots now retain physical section membership, so neither
    /// [`ConfiguredRemote::find`] nor [`ConfiguredRemoteRecord::find`] emits this variant.
    #[error("unsupported remote tagOpt inheritance across configuration sections")]
    UnsupportedTagOptionInheritance,
    /// A URL in the supported syntax is malformed.
    #[error("remote {key}, occurrence {occurrence}, rewritten={rewritten}")]
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
    #[error("remote {key}, occurrence {occurrence}")]
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
    /// with an invalid tag option or refspec still fails. The final tag option in each physical
    /// section participates only when explicit; the newest participating section wins. An implicit
    /// final option skips its section, including earlier values there. Explicit empty values fail.
    /// Refspecs retain configuration order and duplicates, leaving selection policy to callers.
    ///
    /// Ordinary `insteadOf` rewrites match canonical URL bytes. The longest prefix wins, with the
    /// first equal-length match retained. Empty and implicit match values match the empty prefix.
    /// Explicit push URLs disable `pushInsteadOf`. Otherwise a malformed supported push-only
    /// rewrite retains that occurrence's original URL, even if an ordinary rewrite also matches.
    /// Unsupported rewritten syntax always requests whole-remote compatibility handling. SSH IPv6
    /// originals also require compatibility when a rule of the applicable rewrite kind exists:
    /// serializers that omit their brackets can otherwise select a different destination.
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
        let Some(record) = ConfiguredRemoteRecord::find(config, name)? else {
            return Ok(None);
        };
        let ConfiguredRemoteRecord {
            fetch,
            push,
            fetch_refspecs,
            push_refspecs,
            ..
        } = record;
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
        self.fetch_url.as_ref().map(|url| url.bytes.as_slice())
    }

    /// First supported serialized push URL, or the first fetch URL with push rewrite policy.
    /// Bytes may contain credentials; do not log them by default.
    pub fn push_url(&self) -> Option<&[u8]> {
        self.push_url.as_ref().map(|url| url.bytes.as_slice())
    }

    /// Interpreted parts of the selected fetch URL, after resets and ordinary rewrites.
    ///
    /// Retains the original parsing result, including distinctions lost during serialization.
    /// Prefer this accessor to parsing [`Self::fetch_url`] again when presenting its host or path.
    pub fn fetch_url_parts(&self) -> Option<&ConfiguredUrlParts> {
        self.fetch_url.as_ref().map(|url| &url.parts)
    }

    /// Interpreted parts of the selected push URL, including fetch fallback and push rewrites.
    ///
    /// Bytes and parts always come from the same selected parsing result. An invalid supported
    /// push-only rewrite retains the original URL's parts as well as its bytes.
    pub fn push_url_parts(&self) -> Option<&ConfiguredUrlParts> {
        self.push_url.as_ref().map(|url| &url.parts)
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

/// A validated remote before URL rewrites, including URL-free configured sections.
///
/// All surviving URL occurrences are retained in configuration order, after implicit and empty
/// resets and supported URL serialization. This is a read-only record, not an edit transaction:
/// callers remain responsible for fresh source loading, locks and publication. Unknown fields
/// remain in the original [`Config`], not in this record. Debug output omits value bytes.
#[derive(Clone)]
pub struct ConfiguredRemoteRecord {
    fetch: Vec<UrlOccurrence>,
    push: Vec<UrlOccurrence>,
    fetch_refspecs: Vec<ConfiguredRefspec>,
    push_refspecs: Vec<ConfiguredRefspec>,
    tag_option: Option<&'static [u8]>,
}

impl std::fmt::Debug for ConfiguredRemoteRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfiguredRemoteRecord")
            .field("fetch_url_count", &self.fetch.len())
            .field("push_url_count", &self.push.len())
            .field("fetch_refspec_count", &self.fetch_refspecs.len())
            .field("push_refspec_count", &self.push_refspecs.len())
            .field("tag_option", &self.tag_option)
            .finish()
    }
}

impl ConfiguredRemoteRecord {
    /// Finds a configured section without applying `insteadOf` or `pushInsteadOf`.
    ///
    /// Returns `None` only when the section is absent. An empty or URL-free section returns a
    /// record after validation. No fetch-to-push URL fallback is applied. Names are exact
    /// subsection bytes, without reference-name validation.
    ///
    /// # Errors
    ///
    /// Validates the selected tag option, surviving fetch URLs, surviving push URLs, all fetch
    /// refspecs, then all push refspecs. Uses the supported syntax and whole-record compatibility
    /// boundary of [`ConfiguredRemote`], including its physical-section tag-option selection.
    /// Rewrites are neither read nor validated.
    ///
    /// ```
    /// use girt::Config;
    /// use girt::remote::ConfiguredRemoteRecord;
    /// let config = Config::parse(b"[remote \"origin\"]\n")?;
    /// let record = ConfiguredRemoteRecord::find(&config, b"origin")?.unwrap();
    /// assert_eq!(record.fetch_urls().len(), 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn find(config: &Config, name: &[u8]) -> Result<Option<Self>, ConfiguredRemoteError> {
        if !config.contains_section("remote", Some(name)) {
            return Ok(None);
        }
        let selected_tag_option = config
            .section_occurrences()
            .filter(|section| {
                section.name().eq_ignore_ascii_case(b"remote") && section.subsection() == Some(name)
            })
            .filter_map(|section| {
                section
                    .entry_indices()
                    .iter()
                    .rev()
                    .map(|&index| &config.entries()[index])
                    .find(|entry| entry.name.eq_ignore_ascii_case(b"tagopt"))
                    .and_then(|entry| entry.value.as_deref())
            })
            .last();
        let tag_option = match selected_tag_option {
            None => None,
            Some(b"--tags") => Some(b"--tags".as_slice()),
            Some(b"--no-tags") => Some(b"--no-tags".as_slice()),
            _ => return Err(ConfiguredRemoteError::TagOption),
        };
        let fetch = configured_urls(config, name, "url")?;
        let push = configured_urls(config, name, "pushurl")?;
        let fetch_refspecs = configured_refspecs(config, name, "fetch", Direction::Fetch)?;
        let push_refspecs = configured_refspecs(config, name, "push", Direction::Push)?;
        Ok(Some(Self {
            fetch,
            push,
            fetch_refspecs,
            push_refspecs,
            tag_option,
        }))
    }

    /// Every surviving supported serialized fetch URL, without rewrites. May contain secrets.
    pub fn fetch_urls(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.fetch.iter().map(|url| url.url.bytes.as_slice())
    }

    /// Every surviving explicit push URL, without rewrites or fallback. May contain secrets.
    pub fn push_urls(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.push.iter().map(|url| url.url.bytes.as_slice())
    }

    /// Fetch descriptors in configuration order, including duplicates and defaults.
    pub fn fetch_refspecs(&self) -> &[ConfiguredRefspec] {
        &self.fetch_refspecs
    }

    /// Push descriptors in configuration order, including duplicates.
    pub fn push_refspecs(&self) -> &[ConfiguredRefspec] {
        &self.push_refspecs
    }

    /// Selected `--tags` or `--no-tags`, or `None` if every section lacks an explicit final value.
    pub fn tag_option(&self) -> Option<&[u8]> {
        self.tag_option
    }
}

#[derive(Clone)]
struct UrlOccurrence {
    index: usize,
    url: NormalizedUrl,
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
            parse(bytes)
                .map(|url| UrlOccurrence { index, url })
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

/// Applies one ordinary `insteadOf` rewrite to a validated configured destination.
///
/// Normalizes the supplied URL before matching prefixes. The longest matching prefix wins;
/// the first occurrence wins equal-length ties. An implicit or empty prefix matches every URL.
/// The replacement is normalized and validated once, without recursively applying rewrites.
/// `pushInsteadOf` entries are ignored: this function has no push fallback policy. SSH IPv6
/// originals require compatibility when any ordinary rewrite rule exists, since serializers
/// that omit their brackets can otherwise select a different destination.
///
/// Reads only the supplied snapshot, without filesystem, environment or network access. It does
/// not authorize a transport or check repository existence. Returned bytes may contain private
/// paths or credentials. To persist the original destination after validating its rewrite, retain
/// the separate result of [`super::normalize_configured_url`].
///
/// # Errors
///
/// Uses the bounded syntax of [`super::normalize_configured_url`]. Invalid original URLs fail
/// before prefix matching, so a rewrite cannot repair them. Invalid replacement URLs also fail.
/// [`ConfiguredUrlError::Unsupported`] requests compatibility handling, not rejection as malformed.
/// Error values omit URL contents.
///
/// ```
/// use girt::Config;
/// use girt::remote::rewrite_configured_url;
/// let config = Config::parse(b"[url \"ssh://HOST/\"]\ninsteadOf=https://host/\n")?;
/// assert_eq!(
///     rewrite_configured_url(&config, b"https://HOST/repo")?,
///     b"ssh://host/repo"
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn rewrite_configured_url(
    config: &Config,
    bytes: &[u8],
) -> Result<Vec<u8>, ConfiguredUrlError> {
    let original = parse(bytes)?;
    match rewrite(config, &original, "insteadof")? {
        Some(rewritten) => normalize(&rewritten),
        None => Ok(original.bytes),
    }
}

fn rewrite_urls(
    config: &Config,
    urls: &[UrlOccurrence],
    key: &'static str,
) -> Result<Vec<NormalizedUrl>, ConfiguredRemoteError> {
    urls.iter()
        .map(|url| {
            let rewritten = rewrite(config, &url.url, "insteadof")
                .map_err(|err| url_error(err, key, url.index, true))?;
            match rewritten {
                Some(bytes) => parse(&bytes).map_err(|err| url_error(err, key, url.index, true)),
                None => Ok(url.url.clone()),
            }
        })
        .collect()
}

fn push_fallback_urls(
    config: &Config,
    originals: &[UrlOccurrence],
    ordinary: &[NormalizedUrl],
) -> Result<Vec<NormalizedUrl>, ConfiguredRemoteError> {
    originals
        .iter()
        .zip(ordinary)
        .map(|(url, ordinary)| {
            let rewritten = rewrite(config, &url.url, "pushinsteadof")
                .map_err(|err| url_error(err, "url", url.index, true))?;
            let Some(bytes) = rewritten else {
                return Ok(ordinary.clone());
            };
            match parse(&bytes) {
                Ok(url) => Ok(url),
                Err(ConfiguredUrlError::Unsupported) => Err(url_error(
                    ConfiguredUrlError::Unsupported,
                    "url",
                    url.index,
                    true,
                )),
                Err(_) => Ok(url.url.clone()),
            }
        })
        .collect()
}

fn rewrite(
    config: &Config,
    url: &NormalizedUrl,
    key: &str,
) -> Result<Option<Vec<u8>>, ConfiguredUrlError> {
    // Compatibility serializers can omit SSH IPv6 brackets. Their canonical-prefix matching
    // then selects different destinations. Decline even nonmatching rules rather than guess
    // which serialization a caller's existing configuration expects.
    let ssh_ipv6 = url.bytes.starts_with(b"ssh://")
        && url.parts.host().is_some_and(|host| {
            std::str::from_utf8(host)
                .ok()
                .and_then(|host| host.parse::<std::net::Ipv6Addr>().ok())
                .is_some()
        });
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
        if ssh_ipv6 {
            return Err(ConfiguredUrlError::Unsupported);
        }
        let prefix = entry.value.as_deref().unwrap_or_default();
        if url.bytes.starts_with(prefix) && selected.is_none_or(|(_, old)| prefix.len() > old.len())
        {
            selected = Some((base, prefix));
        }
    }
    Ok(selected.map(|(base, prefix)| [base, &url.bytes[prefix.len()..]].concat()))
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
