use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{
    Config, ConfigError, ConfigInputs, ConfigScope, Entry, Origin, SourceLocation, wildmatch,
};

/// Contextual resolution failure. Source locations may contain private paths; tracing omits them.
#[derive(Debug, thiserror::Error)]
#[error("configuration resolution failed: {source}")]
pub struct ResolveError {
    /// Location being loaded or interpreted.
    pub location: SourceLocation,
    /// Outermost-first include directive ancestry.
    pub included_from: Vec<SourceLocation>,
    /// Machine-inspectable cause.
    #[source]
    pub source: ResolveFailure,
}

/// Failure category without raw configuration values.
#[derive(Debug, thiserror::Error)]
pub enum ResolveFailure {
    /// Source could not be read.
    #[error("source I/O failure")]
    Io(#[source] std::io::Error),
    /// Invalid source bytes.
    #[error("invalid source syntax: {0}")]
    Parse(#[source] ConfigError),
    /// A finite work budget was exhausted.
    #[error("configuration limit: {0}")]
    Limit(&'static str),
    /// An include reaches an ancestor file.
    #[error("configuration include cycle")]
    Cycle,
    /// Required expansion context or valid directive is missing.
    #[error("invalid configuration input: {0}")]
    Input(&'static str),
    /// A hasconfig include declares a remote URL, possibly through another include.
    #[error("remote URL in hasconfig include")]
    ConditionalRemote,
}

impl Config {
    /// Resolves explicit sources and returns an owned effective snapshot with per-entry provenance.
    ///
    /// Includes expand in place and inherit their root scope. Unknown conditional keywords do not
    /// match, as in Git. `gitdir`, `gitdir/i`, `onbranch`, and `hasconfig:remote.*.url` are
    /// supported. A preliminary scan validates all hasconfig include descendants and collects
    /// other remote URL occurrences, including later layers. File bytes are cached within this
    /// call, then discarded. No environment is read and no file is written. See
    /// [`ConfigInputs`] for refresh semantics.
    ///
    /// # Errors
    ///
    /// Reports source I/O, syntax, expansion, cycle, and budget failures with include ancestry.
    /// Missing optional roots and include files are ignored. Runtime relative includes require a
    /// file origin and fail; use absolute paths. Unix paths preserve bytes; other platforms require
    /// valid UTF-8 in path values. `~user` and prefix expansion require explicit context mappings.
    pub fn resolve(inputs: &ConfigInputs) -> Result<Self, ResolveError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "config.resolve",
            outcome = "incomplete",
            failure_class = tracing::field::Empty
        );
        let operation = || Resolver::new(inputs).resolve();
        #[cfg(feature = "tracing")]
        {
            let result = span.in_scope(operation);
            crate::trace::finish(&span, &result, |error| match error.source {
                ResolveFailure::Io(_) => "io",
                ResolveFailure::Parse(_) => "corrupt",
                ResolveFailure::Limit(_) => "limit",
                ResolveFailure::Cycle => "cycle",
                ResolveFailure::Input(_) | ResolveFailure::ConditionalRemote => "invalid_input",
            });
            result
        }
        #[cfg(not(feature = "tracing"))]
        operation()
    }

    /// Decodes explicitly supplied `GIT_CONFIG_COUNT/KEY_n/VALUE_n` environment pairs.
    ///
    /// The closure supplies bytes; this method never reads or mutates process environment. Empty
    /// count means zero. Dotted keys split at the first and last dot, preserving subsection bytes.
    /// Values are literal bytes, without config-file escape processing. Caller `command` entries
    /// take precedence over this snapshot when resolving [`ConfigInputs`]. File-selection variables
    /// belong to the caller's `files` policy, not this runtime-pair decoder.
    ///
    /// # Errors
    ///
    /// Rejects invalid/missing counts, keys, values, NULs and counts exceeding `max_pairs`.
    pub fn from_environment(
        mut get: impl FnMut(&str) -> Option<Vec<u8>>,
        max_pairs: usize,
    ) -> Result<Self, ConfigError> {
        let count = get("GIT_CONFIG_COUNT").unwrap_or_default();
        let count = if count.is_empty() {
            0
        } else {
            std::str::from_utf8(&count)
                .ok()
                .map(str::trim_ascii_start)
                .and_then(|s| {
                    if s.starts_with('-') && s[1..].bytes().all(|b| b == b'0') && s.len() > 1 {
                        Some(0)
                    } else {
                        s.parse::<usize>().ok()
                    }
                })
                .ok_or(ConfigError {
                    line: 1,
                    reason: "invalid environment count",
                })?
        };
        if count > max_pairs {
            return Err(ConfigError {
                line: 1,
                reason: "environment pair limit",
            });
        }
        let mut entries = Vec::new();
        for index in 0..count {
            let line = index + 1;
            let error = |reason| ConfigError { line, reason };
            let key = get(&format!("GIT_CONFIG_KEY_{index}"))
                .ok_or_else(|| error("missing environment key"))?;
            let value = get(&format!("GIT_CONFIG_VALUE_{index}"))
                .ok_or_else(|| error("missing environment value"))?;
            let first = key
                .iter()
                .position(|b| *b == b'.')
                .ok_or_else(|| error("invalid environment key"))?;
            let last = key.iter().rposition(|b| *b == b'.').unwrap();
            let section = &key[..first];
            let name = &key[last + 1..];
            if section.is_empty()
                || !section
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
                || !name.first().is_some_and(u8::is_ascii_alphabetic)
                || !name.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-')
                || key.contains(&0)
                || value.contains(&0)
            {
                return Err(error("invalid environment pair"));
            }
            entries.push(Entry {
                section: section.to_vec(),
                subsection: (first != last).then(|| key[first + 1..last].to_vec()),
                name: name.to_vec(),
                value: Some(value),
                line,
                origin: None,
            });
        }
        Ok(Self { entries })
    }
}

struct Resolver<'a> {
    inputs: &'a ConfigInputs,
    cache: HashMap<PathBuf, Config>,
    bytes: usize,
    visited: usize,
    expanded_bytes: usize,
    stack: Vec<PathBuf>,
    ancestry: Vec<SourceLocation>,
    urls: Vec<Vec<u8>>,
    output: Vec<Entry>,
    scanning: bool,
}

impl<'a> Resolver<'a> {
    fn new(inputs: &'a ConfigInputs) -> Self {
        Self {
            inputs,
            cache: HashMap::new(),
            bytes: 0,
            visited: 0,
            expanded_bytes: 0,
            stack: Vec::new(),
            ancestry: Vec::new(),
            urls: Vec::new(),
            output: Vec::new(),
            scanning: true,
        }
    }

    fn resolve(mut self) -> Result<Config, ResolveError> {
        self.roots()?;
        self.scanning = false;
        self.visited = 0;
        self.expanded_bytes = 0;
        self.roots()?;
        Ok(Config {
            entries: self.output,
        })
    }

    fn roots(&mut self) -> Result<(), ResolveError> {
        let mut files: Vec<_> = self.inputs.files.iter().collect();
        files.sort_by_key(|file| file.scope);
        for file in files {
            if file.scope > ConfigScope::Worktree {
                return Err(self.error(
                    &SourceLocation {
                        path: Some(file.path.clone()),
                        line: 1,
                    },
                    ResolveFailure::Input("file scope must be system, global, local or worktree"),
                ));
            }
            self.file(&file.path, file.scope, file.optional, false)?;
        }
        for (config, scope) in [
            (&self.inputs.environment, ConfigScope::Environment),
            (&self.inputs.command, ConfigScope::Command),
        ] {
            if let Some(config) = config {
                self.entries(config, None, scope, false)?;
            }
        }
        Ok(())
    }

    fn error(&self, location: &SourceLocation, source: ResolveFailure) -> ResolveError {
        ResolveError {
            location: location.clone(),
            included_from: self.ancestry.clone(),
            source,
        }
    }

    fn file(
        &mut self,
        path: &Path,
        scope: ConfigScope,
        optional: bool,
        prohibited: bool,
    ) -> Result<(), ResolveError> {
        let location = SourceLocation {
            path: Some(path.into()),
            line: 1,
        };
        if self.ancestry.len() > self.inputs.limits.depth {
            return Err(self.error(&location, ResolveFailure::Limit("include depth")));
        }
        // Canonical identities catch aliases in cycles; source provenance retains supplied
        // spelling.
        let identity = match std::fs::canonicalize(path) {
            Ok(path) => path,
            Err(e) if optional && e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(self.error(&location, ResolveFailure::Io(e))),
        };
        if self.stack.contains(&identity) {
            return Err(self.error(&location, ResolveFailure::Cycle));
        }
        let config = if let Some(config) = self.cache.get(path) {
            config.clone()
        } else {
            let remaining = self.inputs.limits.bytes.saturating_sub(self.bytes);
            let mut bytes = Vec::new();
            let mut file = std::fs::File::open(path)
                .map_err(|e| self.error(&location, ResolveFailure::Io(e)))?
                .take(remaining.saturating_add(1) as u64);
            file.read_to_end(&mut bytes)
                .map_err(|e| self.error(&location, ResolveFailure::Io(e)))?;
            if bytes.len() > remaining {
                return Err(self.error(&location, ResolveFailure::Limit("source bytes")));
            }
            self.bytes += bytes.len();
            let config = Config::parse(&bytes).map_err(|e| {
                let mut at = location.clone();
                at.line = e.line;
                self.error(&at, ResolveFailure::Parse(e))
            })?;
            self.cache.insert(path.into(), config.clone());
            config
        };
        self.stack.push(identity);
        let result = self.entries(&config, Some(path), scope, prohibited);
        self.stack.pop();
        result
    }

    fn entries(
        &mut self,
        config: &Config,
        path: Option<&Path>,
        scope: ConfigScope,
        prohibited: bool,
    ) -> Result<(), ResolveError> {
        for entry in config.entries() {
            let location = SourceLocation {
                path: path.map(Path::to_path_buf),
                line: entry.line,
            };
            self.visited += 1;
            self.expanded_bytes = self
                .expanded_bytes
                .saturating_add(entry.section.len())
                .saturating_add(entry.name.len())
                .saturating_add(entry.subsection.as_ref().map_or(0, Vec::len))
                .saturating_add(entry.value.as_ref().map_or(0, Vec::len));
            if self.expanded_bytes > self.inputs.limits.bytes {
                return Err(self.error(&location, ResolveFailure::Limit("expanded bytes")));
            }
            if self.visited > self.inputs.limits.entries {
                return Err(self.error(&location, ResolveFailure::Limit("entry occurrences")));
            }
            if entry.section.eq_ignore_ascii_case(b"remote")
                && entry.subsection.is_some()
                && entry.name.eq_ignore_ascii_case(b"url")
            {
                if prohibited {
                    return Err(self.error(&location, ResolveFailure::ConditionalRemote));
                }
                if self.scanning
                    && let Some(url) = &entry.value
                {
                    self.urls.push(url.clone());
                }
            }
            if !self.scanning {
                let mut entry = entry.clone();
                entry.origin = Some(Origin {
                    scope,
                    location: location.clone(),
                    included_from: self.ancestry.clone(),
                });
                self.output.push(entry);
            }
            if !entry.name.eq_ignore_ascii_case(b"path") {
                continue;
            }
            let mut hasconfig = false;
            let include =
                if entry.section.eq_ignore_ascii_case(b"include") && entry.subsection.is_none() {
                    true
                } else if entry.section.eq_ignore_ascii_case(b"includeif") {
                    if let Some(condition) = &entry.subsection {
                        hasconfig = condition.starts_with(b"hasconfig:remote.*.url:");
                        if hasconfig && self.scanning {
                            true
                        } else {
                            self.condition(condition, &location)?
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };
            if include {
                let value = entry.value.as_deref().ok_or_else(|| {
                    self.error(&location, ResolveFailure::Input("include requires a value"))
                })?;
                if value.is_empty() {
                    continue;
                }
                let target = self.expand(value, &location)?;
                let target = if target.is_absolute() {
                    target
                } else {
                    let parent = path.and_then(Path::parent).ok_or_else(|| {
                        self.error(
                            &location,
                            ResolveFailure::Input("relative include without file origin"),
                        )
                    })?;
                    parent.join(target)
                };
                self.ancestry.push(location);
                let result = self.file(&target, scope, true, prohibited || hasconfig);
                self.ancestry.pop();
                result?;
            }
        }
        Ok(())
    }

    fn expand(&self, bytes: &[u8], location: &SourceLocation) -> Result<PathBuf, ResolveError> {
        let context = &self.inputs.context;
        let expanded = if let Some(rest) = bytes.strip_prefix(b"~/") {
            context.home.as_ref().map(|home| (home, rest))
        } else if let Some(rest) = bytes.strip_prefix(b"%(prefix)/") {
            context.prefix.as_ref().map(|prefix| (prefix, rest))
        } else if bytes.starts_with(b"~") {
            bytes.iter().position(|b| *b == b'/').and_then(|slash| {
                context
                    .user_homes
                    .iter()
                    .find(|(name, _)| name == &bytes[1..slash])
                    .map(|(_, home)| (home, &bytes[slash + 1..]))
            })
        } else {
            return path_bytes(bytes).map_err(|e| self.error(location, e));
        };
        let (base, rest) = expanded.ok_or_else(|| {
            self.error(
                location,
                ResolveFailure::Input("missing path expansion context"),
            )
        })?;
        Ok(base.join(path_bytes(rest).map_err(|e| self.error(location, e))?))
    }

    fn glob(
        &self,
        pattern: &[u8],
        text: &[u8],
        fold: bool,
        location: &SourceLocation,
    ) -> Result<bool, ResolveError> {
        wildmatch::matches(pattern, text, fold, self.inputs.limits.match_cells)
            .ok_or_else(|| self.error(location, ResolveFailure::Limit("wildcard work")))
    }

    fn condition(&self, condition: &[u8], location: &SourceLocation) -> Result<bool, ResolveError> {
        if let Some(pattern) = condition.strip_prefix(b"hasconfig:remote.*.url:") {
            for url in &self.urls {
                if self.glob(pattern, url, false, location)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        if let Some(pattern) = condition.strip_prefix(b"onbranch:") {
            let pattern = trailing_glob(pattern);
            return match &self.inputs.context.branch {
                Some(branch) => self.glob(&pattern, branch, false, location),
                None => Ok(false),
            };
        }
        let (pattern, fold) = if let Some(pattern) = condition.strip_prefix(b"gitdir:") {
            (pattern, false)
        } else if let Some(pattern) = condition.strip_prefix(b"gitdir/i:") {
            (pattern, true)
        } else {
            return Ok(false);
        };
        let mut pattern = if let Some(rest) = pattern.strip_prefix(b"./") {
            let parent = location
                .path
                .as_deref()
                .and_then(Path::parent)
                .ok_or_else(|| {
                    self.error(
                        location,
                        ResolveFailure::Input("relative condition without file origin"),
                    )
                })?;
            let absolute = std::fs::canonicalize(parent)
                .map_err(|e| self.error(location, ResolveFailure::Io(e)))?;
            let mut bytes = os_bytes(&absolute);
            bytes.push(b'/');
            bytes.extend_from_slice(rest);
            bytes
        } else if let Some(rest) = pattern.strip_prefix(b"~/") {
            // Keep glob syntax out of PathBuf::join: Windows verbatim paths normalize
            // components and lose the trailing slash that requests recursive matching.
            let home = self.expand(b"~/", location)?;
            let mut bytes = os_bytes(&home);
            if bytes.last() != Some(&b'/') {
                bytes.push(b'/');
            }
            bytes.extend_from_slice(rest);
            bytes
        } else {
            pattern.to_vec()
        };
        if !path_bytes(&pattern)
            .map_err(|e| self.error(location, e))?
            .is_absolute()
        {
            pattern.splice(..0, b"**/".iter().copied());
        }
        pattern = trailing_glob(&pattern);
        for path in &self.inputs.context.git_dirs {
            if self.glob(&pattern, &os_bytes(path), fold, location)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn trailing_glob(pattern: &[u8]) -> Vec<u8> {
    let mut result = pattern.to_vec();
    if result.last() == Some(&b'/') {
        result.extend_from_slice(b"**");
    }
    result
}

fn os_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().replace('\\', "/").into_bytes()
    }
}

fn path_bytes(bytes: &[u8]) -> Result<PathBuf, ResolveFailure> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| ResolveFailure::Input("path is not UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::missing_key(Some(b"1".as_slice()), None, Some(b"v".as_slice()))]
    #[case::missing_value(Some(b"1".as_slice()), Some(b"a.b".as_slice()), None)]
    #[case::negative(Some(b"-1".as_slice()), None, None)]
    #[case::limit(Some(b"3".as_slice()), None, None)]
    #[case::bad_key(Some(b"1".as_slice()), Some(b"a.1".as_slice()), Some(b"v".as_slice()))]
    #[case::nul(Some(b"1".as_slice()), Some(b"a.b".as_slice()), Some(b"v\0".as_slice()))]
    fn rejects_environment_input(
        #[case] count: Option<&[u8]>,
        #[case] key: Option<&[u8]>,
        #[case] value: Option<&[u8]>,
    ) {
        let result = Config::from_environment(
            |name| match name {
                "GIT_CONFIG_COUNT" => count.map(<[u8]>::to_vec),
                "GIT_CONFIG_KEY_0" => key.map(<[u8]>::to_vec),
                "GIT_CONFIG_VALUE_0" => value.map(<[u8]>::to_vec),
                _ => None,
            },
            2,
        );
        assert!(result.is_err());
    }

    #[rstest]
    #[case::empty(Some(b"".to_vec()))]
    #[case::absent(None)]
    #[case::zero(Some(b"0".to_vec()))]
    fn empty_environment(#[case] count: Option<Vec<u8>>) {
        let config = Config::from_environment(|_| count.clone(), 0).unwrap();
        assert!(config.entries().is_empty());
    }

    #[test]
    fn runtime_relative_include_has_contextual_error() {
        let input = ConfigInputs {
            command: Some(Config::parse(b"[include]\npath=relative\n").unwrap()),
            ..Default::default()
        };
        let error = Config::resolve(&input).unwrap_err();
        assert!(matches!(error.source, ResolveFailure::Input(_)));
        assert_eq!(
            error.location,
            SourceLocation {
                path: None,
                line: 2
            }
        );
    }

    #[test]
    fn repeated_expansion_is_bounded_even_when_reads_are_cached() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("child");
        std::fs::write(&child, format!("[a]\nb={}\n", "x".repeat(400))).unwrap();
        let path = root.path().join("config");
        std::fs::write(&path, b"[include]\npath=child\npath=child\npath=child\n").unwrap();
        let mut inputs = ConfigInputs {
            files: vec![super::super::ConfigFile {
                path,
                scope: ConfigScope::Local,
                optional: false,
            }],
            ..Default::default()
        };
        inputs.limits.bytes = 1000;
        assert!(matches!(
            Config::resolve(&inputs).unwrap_err().source,
            ResolveFailure::Limit("expanded bytes")
        ));
    }

    #[test]
    fn named_home_and_prefix_expansion_use_explicit_context() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("child"), b"[a]\nb=ok\n").unwrap();
        let mut input = ConfigInputs {
            command: Some(
                Config::parse(b"[include]\npath=~someone/child\npath=%(prefix)/child\n").unwrap(),
            ),
            ..Default::default()
        };
        input
            .context
            .user_homes
            .push((b"someone".to_vec(), root.path().into()));
        input.context.prefix = Some(root.path().into());
        assert_eq!(
            Config::resolve(&input)
                .unwrap()
                .values("a", None, "b")
                .count(),
            2
        );
    }

    #[rstest]
    #[case::recursive("~/repo/", true)]
    #[case::wildcard("~/r*/", true)]
    #[case::exact("~/repo/.git", true)]
    #[case::not_recursive("~/repo", false)]
    fn home_condition_preserves_pattern_syntax(#[case] pattern: &str, #[case] matches: bool) {
        let root = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(root.path()).unwrap();
        let mut inputs = ConfigInputs::default();
        inputs.context.git_dirs.push(home.join("repo/.git"));
        inputs.context.home = Some(home);
        let resolver = Resolver::new(&inputs);
        let location = SourceLocation {
            path: None,
            line: 1,
        };
        assert_eq!(
            resolver
                .condition(format!("gitdir:{pattern}").as_bytes(), &location)
                .unwrap(),
            matches
        );
    }

    #[test]
    fn missing_home_context_fails_without_ambient_lookup() {
        let input = ConfigInputs {
            command: Some(Config::parse(b"[include]\npath=~/child\n").unwrap()),
            ..Default::default()
        };
        assert!(matches!(
            Config::resolve(&input).unwrap_err().source,
            ResolveFailure::Input("missing path expansion context")
        ));
    }
}
