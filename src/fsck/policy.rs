//! Configuration namespaces and bounded exception lists shared by scans and transfers.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{MessageId, Severity};
use crate::{Config, ObjectFormat, ObjectId};

/// Independent Git configuration namespace for an integrity operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Scope {
    /// `fsck.<id>` and `fsck.skipList`.
    Repository,
    /// `fetch.fsck.<id>` and `fetch.fsck.skipList`, without repository-policy fallback.
    Fetch,
    /// `receive.fsck.<id>` and `receive.fsck.skipList`, without repository-policy fallback.
    Receive,
}

impl Scope {
    fn key(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Repository => ("fsck", None),
            Self::Fetch => ("fetch", Some("fsck")),
            Self::Receive => ("receive", Some("fsck")),
        }
    }
}

/// Bounds for reading configured exceptions.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct PolicyLimits {
    /// Maximum skip-list bytes, including comments and whitespace.
    pub max_bytes: usize,
    /// Maximum skip-list records, including duplicate identities.
    pub max_entries: usize,
}
impl Default for PolicyLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 << 20,
            max_entries: 1 << 20,
        }
    }
}

/// Message overrides and known historical exceptions. Storage corruption and missing dependencies
/// remain errors even when an object's payload diagnostics are skipped.
///
/// ```
/// use girt::fsck::{MessageId, Policy, Scope, Severity};
/// use girt::{Config, ObjectFormat, ObjectId};
///
/// let config = Config::parse(b"[fsck]\n hasDotgit = ignore\n")?;
/// let policy = Policy::from_config(
///     &config,
///     Scope::Repository,
///     ObjectFormat::Sha1,
///     std::path::Path::new("."),
///     Default::default(),
/// )?;
/// let id = ObjectId::for_blob(ObjectFormat::Sha1, b"historical data");
/// assert_eq!(
///     policy.severity(id, MessageId::HasDotgit, true),
///     Severity::Ignore
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct Policy {
    overrides: BTreeMap<MessageId, Severity>,
    skipped: BTreeSet<ObjectId>,
    unknown: Vec<Vec<u8>>,
    large_pathname: usize,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            overrides: BTreeMap::new(),
            skipped: BTreeSet::new(),
            unknown: Vec::new(),
            large_pathname: 4096,
        }
    }
}

/// Invalid integrity policy; no partial policy is returned.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PolicyError {
    /// Repository fsck refuses unknown message IDs. Transfers retain them as warnings instead.
    #[error("unknown fsck message ID: {0:?}")]
    UnknownMessage(Vec<u8>),
    /// Only `error`, `warn` and `ignore` are accepted.
    #[error("invalid fsck severity: {0:?}")]
    Severity(Vec<u8>),
    /// Fatal structural diagnostics cannot be demoted to warn or ignore.
    #[error("cannot override fatal fsck message {0:?}")]
    Fatal(MessageId),
    /// A skip-list record does not contain a full object identity.
    #[error("invalid fsck skip-list record at line {0}")]
    SkipRecord(usize),
    /// Reading a skip list failed.
    #[error("reading fsck skip list {path}")]
    Io {
        /// Configured path.
        path: PathBuf,
        /// Original I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Resource bound exceeded.
    #[error("fsck policy limit exceeded: {0}")]
    Limit(&'static str),
    /// Path interpolation requires caller-owned environment context.
    #[error("unsupported fsck skip-list path")]
    Path,
}

impl Policy {
    /// Reads one independent policy namespace and its optional skip list.
    ///
    /// Relative skip-list paths resolve against explicit `base`, never the process directory.
    /// Native path bytes are preserved on Unix; other platforms require UTF-8. Tilde expansion
    /// is explicitly refused because this API does not read the process environment.
    /// Unknown transfer IDs are retained in [`Self::unknown_messages`], matching Git's warning
    /// policy. Repository fsck refuses them. Later values override earlier ones.
    ///
    /// # Errors
    ///
    /// Returns invalid severity, fatal override, path, I/O, skip-record or resource errors.
    pub fn from_config(
        config: &Config,
        scope: Scope,
        format: ObjectFormat,
        base: &Path,
        limits: PolicyLimits,
    ) -> Result<Self, PolicyError> {
        let mut result = Self::default();
        let (section, subsection) = scope.key();
        for entry in config.entries().iter().filter(|entry| {
            entry.section.eq_ignore_ascii_case(section.as_bytes())
                && entry.subsection.as_deref() == subsection.map(str::as_bytes)
        }) {
            if entry.name.eq_ignore_ascii_case(b"skiplist") {
                continue;
            }
            let Some(id) = MessageId::from_config_name(&entry.name) else {
                if scope == Scope::Repository {
                    return Err(PolicyError::UnknownMessage(entry.name.clone()));
                }
                result.unknown.push(entry.name.clone());
                continue;
            };
            let raw_value = entry.value.as_deref().unwrap_or_default();
            let value = if id == MessageId::LargePathname {
                if let Some((severity, maximum)) = crate::bytes::split_once(raw_value, b':') {
                    result.large_pathname = std::str::from_utf8(maximum)
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .ok_or_else(|| PolicyError::Severity(raw_value.to_vec()))?;
                    severity
                } else {
                    raw_value
                }
            } else {
                raw_value
            };
            let severity = if value.eq_ignore_ascii_case(b"error") {
                Severity::Error
            } else if value.eq_ignore_ascii_case(b"warn") {
                Severity::Warning
            } else if value.eq_ignore_ascii_case(b"ignore") {
                Severity::Ignore
            } else {
                return Err(PolicyError::Severity(value.to_vec()));
            };
            if id.default_severity() == Severity::Fatal {
                if severity != Severity::Error {
                    return Err(PolicyError::Fatal(id));
                }
                continue;
            }
            result.overrides.insert(id, severity);
        }
        if let Some(raw) = config.value(section, subsection.map(str::as_bytes), "skiplist") {
            let raw = raw.ok_or(PolicyError::Path)?;
            if raw.is_empty() || raw.starts_with(b"~") || raw.contains(&0) {
                return Err(PolicyError::Path);
            }
            let path = native_path(raw)?;
            let path = if path.is_absolute() {
                path
            } else {
                base.join(path)
            };
            let file = std::fs::File::open(&path).map_err(|source| PolicyError::Io {
                path: path.clone(),
                source,
            })?;
            let mut bytes = Vec::new();
            file.take(limits.max_bytes.saturating_add(1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|source| PolicyError::Io { path, source })?;
            result.add_skip_list(format, &bytes, limits)?;
        }
        Ok(result)
    }

    /// Adds full-format identities from a bounded skip list. Comments after `#`, blank lines and
    /// ASCII whitespace are ignored. Duplicate and unsorted records are accepted.
    ///
    /// # Errors
    ///
    /// Returns malformed records or byte/record exhaustion; `self` is unchanged on failure.
    pub fn add_skip_list(
        &mut self,
        format: ObjectFormat,
        bytes: &[u8],
        limits: PolicyLimits,
    ) -> Result<(), PolicyError> {
        if bytes.len() > limits.max_bytes {
            return Err(PolicyError::Limit("skip-list bytes"));
        }
        let mut ids = BTreeSet::new();
        let mut records = 0usize;
        for (line, value) in bytes.split(|&byte| byte == b'\n').enumerate() {
            let value = value
                .split(|&byte| byte == b'#')
                .next()
                .unwrap_or_default()
                .trim_ascii();
            if value.is_empty() {
                continue;
            }
            records += 1;
            if records > limits.max_entries {
                return Err(PolicyError::Limit("skip-list records"));
            }
            let id = std::str::from_utf8(value)
                .ok()
                .and_then(|value| ObjectId::from_hex(format, value).ok())
                .ok_or(PolicyError::SkipRecord(line + 1))?;
            ids.insert(id);
        }
        self.skipped.extend(ids);
        Ok(())
    }

    /// Computes configured severity for one payload diagnostic. Fatal diagnostics are never
    /// skipped.
    pub fn severity(&self, id: ObjectId, message: MessageId, strict: bool) -> Severity {
        if message.default_severity() == Severity::Fatal {
            return Severity::Fatal;
        }
        if self.skipped.contains(&id) {
            return Severity::Ignore;
        }
        self.overrides
            .get(&message)
            .copied()
            .unwrap_or_else(|| message.severity(strict))
    }

    /// Checks one payload with this policy's path-length threshold, severity overrides and skip
    /// list. Storage identity and dependency checks remain the caller's responsibility.
    pub fn check_object(
        &self,
        id: ObjectId,
        kind: crate::ObjectKind,
        data: &[u8],
        strict: bool,
    ) -> Vec<super::Finding> {
        super::check_payload(id.format(), kind, data, self.large_pathname, strict)
            .into_iter()
            .filter_map(|mut finding| {
                finding.severity = self.severity(id, finding.message_id, strict);
                (finding.severity != Severity::Ignore).then_some(finding)
            })
            .collect()
    }

    /// Unknown transfer-policy IDs to report as configuration warnings.
    pub fn unknown_messages(&self) -> &[Vec<u8>] {
        &self.unknown
    }
}

fn native_path(bytes: &[u8]) -> Result<PathBuf, PolicyError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| PolicyError::Path)
    }
}
