use super::{Direction, RefspecError, Refspecs};
use crate::Config;

/// An owned snapshot of a named remote's raw URLs and parsed fetch/push refspecs.
///
/// Names and values preserve bytes. Only `url`, `pushurl`, `fetch` and `push` are interpreted;
/// other settings remain available through the original configuration. Resolved snapshots retain
/// inherited values in precedence order. This consumer performs no additional source reads, URL
/// rewriting, remote-name-as-path fallback or implicit refspec selection.
#[derive(Debug, Clone)]
pub struct Remote {
    name: Vec<u8>,
    urls: Vec<Vec<u8>>,
    push_urls: Vec<Vec<u8>>,
    fetch: Refspecs,
    push: Refspecs,
}

/// A named remote contains a missing value or unsupported refspec.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    /// URL/refspec keys require `=` and a value (empty URLs reset the list).
    #[error("remote key {key}, occurrence {occurrence}, requires a value")]
    MissingValue {
        /// Configuration variable name.
        key: &'static str,
        /// One-based occurrence within this remote and key.
        occurrence: usize,
    },
    /// A configured refspec is invalid or outside the supported subset.
    #[error("remote key {key}, occurrence {occurrence}: {source}")]
    Refspec {
        /// `fetch` or `push`.
        key: &'static str,
        /// One-based occurrence within this remote and key.
        occurrence: usize,
        /// Parser diagnostic; raw URL values are never included in errors.
        #[source]
        source: RefspecError,
    },
}

impl Remote {
    /// Lists exact subsection names with at least one entry, once, in first-entry order.
    ///
    /// Bare empty section headers are not retained by [`Config`]. Names are not validated as paths
    /// or reference components; consumers must validate before using them in either context.
    pub fn names(config: &Config) -> Vec<&[u8]> {
        let mut names = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for entry in config.entries() {
            if entry.section.eq_ignore_ascii_case(b"remote")
                && let Some(name) = entry.subsection.as_deref()
                && seen.insert(name)
            {
                names.push(name);
            }
        }
        names
    }

    /// Reads a case-sensitive subsection from one configuration snapshot.
    ///
    /// Returns `None` if there are no entries for this name. Missing URLs/refspecs produce empty
    /// lists, not guessed defaults. Repeated sections and keys accumulate in file order. An empty
    /// URL or pushURL clears that key's preceding list; later values append normally. URL bytes
    /// are opaque and unvalidated, including whitespace or non-UTF-8 data.
    ///
    /// # Errors
    ///
    /// Implicit boolean values for any of the four keys fail. Every refspec is parsed eagerly,
    /// including the other direction's list; empty refspecs fail rather than selecting Git
    /// defaults.
    pub fn find(config: &Config, name: &[u8]) -> Result<Option<Self>, RemoteError> {
        if !Self::names(config).contains(&name) {
            return Ok(None);
        }
        Ok(Some(Self {
            name: name.to_vec(),
            urls: urls(config, name, "url")?,
            push_urls: urls(config, name, "pushurl")?,
            fetch: refspecs(config, name, "fetch", Direction::Fetch)?,
            push: refspecs(config, name, "push", Direction::Push)?,
        }))
    }

    /// Exact subsection bytes; no normalization or path validation.
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// Raw URL list after empty-value resets, in occurrence order, including duplicates.
    pub fn urls(&self) -> &[Vec<u8>] {
        &self.urls
    }

    /// First raw URL, or `None` when absent/reset. Other URLs are not fetch failover candidates.
    pub fn fetch_url(&self) -> Option<&[u8]> {
        self.urls.first().map(Vec::as_slice)
    }

    /// Raw pushURL list after resets, before URL fallback, including duplicates.
    pub fn configured_push_urls(&self) -> &[Vec<u8>] {
        &self.push_urls
    }

    /// All remaining pushURLs, or all URLs when the pushURL list is empty.
    ///
    /// The order and duplicates are retained. This describes configured destinations; it does not
    /// authorize sending, validate endpoints, or perform URL rewriting.
    pub fn push_urls(&self) -> &[Vec<u8>] {
        if self.push_urls.is_empty() {
            &self.urls
        } else {
            &self.push_urls
        }
    }

    /// Ordered fetch specifications, without an implicit HEAD selection or tag following.
    pub fn fetch_refspecs(&self) -> &Refspecs {
        &self.fetch
    }

    /// Ordered push specifications; empty means no configured mappings, not `push.default`.
    pub fn push_refspecs(&self) -> &Refspecs {
        &self.push
    }
}

fn urls(config: &Config, name: &[u8], key: &'static str) -> Result<Vec<Vec<u8>>, RemoteError> {
    let mut urls = Vec::new();
    for (index, value) in config.values("remote", Some(name), key).enumerate() {
        let value = value.ok_or(RemoteError::MissingValue {
            key,
            occurrence: index + 1,
        })?;
        if value.is_empty() {
            urls.clear();
        } else {
            urls.push(value.to_vec());
        }
    }
    Ok(urls)
}

fn refspecs(
    config: &Config,
    name: &[u8],
    key: &'static str,
    direction: Direction,
) -> Result<Refspecs, RemoteError> {
    let values = config
        .values("remote", Some(name), key)
        .enumerate()
        .map(|(index, value)| {
            value.ok_or(RemoteError::MissingValue {
                key,
                occurrence: index + 1,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Refspecs::parse(direction, values).map_err(|error| RemoteError::Refspec {
        key,
        occurrence: error.index + 1,
        source: error.source,
    })
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
