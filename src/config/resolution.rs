use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::parse::{SectionName, SectionOccurrence};
use super::placement::{IncludePlacement, SectionPlacement};
use super::{
    Config, ConfigError, ConfigInputs, ConfigScope, Entry, IncludeConditionVisibility,
    IncludeDirectiveCase, Origin, ResolveOptions, SourceLocation, UnresolvedIncludePath, wildmatch,
};

/// Contextual resolution failure. Source locations may contain private paths; tracing omits them.
#[derive(Debug, thiserror::Error)]
#[error("configuration resolution failed")]
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
    #[error("invalid source syntax")]
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
        Self::resolve_with_include_placement(inputs, IncludePlacement::InPlace)
    }

    /// Resolves explicit sources with a selected placement of included sections.
    ///
    /// [`IncludePlacement::AfterSectionReverse`] changes output order after sources have been
    /// validated in the same forward, depth-first order as [`Self::resolve`]. Includes keep their
    /// original scope, directive ancestry and source line. Conditional matching, source caching
    /// and all resolution budgets remain unchanged. This does not emulate another opener's
    /// source selection, trust rules or parsing. No environment is read and no files are written.
    ///
    /// # Errors
    ///
    /// Returns the same source, include and budget failures as [`Self::resolve`], in the same
    /// order. Placement adds temporary entry/section slots bounded by those existing budgets.
    ///
    /// # Examples
    ///
    /// ```
    /// use girt::Config;
    /// use girt::config::{ConfigInputs, IncludePlacement};
    /// let inputs = ConfigInputs::default();
    /// let config =
    ///     Config::resolve_with_include_placement(&inputs, IncludePlacement::AfterSectionReverse)?;
    /// assert!(config.entries().is_empty());
    /// # Ok::<(), girt::config::ResolveError>(())
    /// ```
    pub fn resolve_with_include_placement(
        inputs: &ConfigInputs,
        placement: IncludePlacement,
    ) -> Result<Self, ResolveError> {
        Self::resolve_with_options(
            inputs,
            ResolveOptions {
                placement,
                ..ResolveOptions::default()
            },
        )
    }

    /// Resolves explicit sources with caller-selected include evaluation and placement.
    ///
    /// Defaults match [`Self::resolve`]. Resource budgets, provenance, physical section identity
    /// and fresh-read guarantees are unchanged. Evaluation order and output placement are separate.
    ///
    /// # Errors
    ///
    /// Reports the source, input and resource failures of [`Self::resolve`]. The selected policy
    /// determines which conditional descendants are read and whether missing interpolation context
    /// is an error; errors from selected sources are never silently retried under another policy.
    /// Root-snapshot URL views are independently bounded by the input entry and byte limits;
    /// seeding an oversized caller-owned root may report a limit before traversing its entries.
    ///
    /// ```
    /// use girt::config::{Config, ConfigInputs, IncludeConditionVisibility, ResolveOptions};
    /// let options = ResolveOptions {
    ///     conditions: IncludeConditionVisibility::RootSnapshot,
    ///     ..ResolveOptions::default()
    /// };
    /// let config = Config::resolve_with_options(&ConfigInputs::default(), options)?;
    /// assert!(config.entries().is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn resolve_with_options(
        inputs: &ConfigInputs,
        options: ResolveOptions,
    ) -> Result<Self, ResolveError> {
        Self::resolve_with_lookup(inputs, options, None)
    }

    /// Resolves includes with an explicit fallback lookup for `~username/` paths.
    ///
    /// Caller-supplied `user_homes` take precedence. The callback receives exact username bytes
    /// only when expansion reaches an unmapped named home. Positive and negative results are
    /// cached for this resolution across roots and passes; a later call starts a fresh cache.
    /// The default condition policy may reach includes during its preliminary validation pass,
    /// including unmatched hasconfig descendants. Root-snapshot policy visits only selected
    /// descendants. Callback order follows traversal, independently of output placement.
    ///
    /// Cache entries and retained username/path bytes are independently bounded by the input
    /// entry and byte limits. The caller bounds its own callback work and temporary allocations.
    /// No ambient home lookup or user enumeration occurs. `~/` and prefix expansion use only
    /// explicit context. Returning `None` follows the selected unresolved-path policy.
    ///
    /// # Errors
    ///
    /// Returns [`Self::resolve_with_options`]'s errors and cache resource-limit failures. An
    /// oversized callback result is rejected before retention. Earlier callback effects are not
    /// rolled back on failure; callbacks should perform read-only lookup.
    ///
    /// ```
    /// use girt::config::{Config, ConfigInputs, ResolveOptions};
    /// let config = Config::resolve_with_user_home_lookup(
    ///     &ConfigInputs::default(),
    ///     ResolveOptions::default(),
    ///     |_name| None,
    /// )?;
    /// assert!(config.entries().is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn resolve_with_user_home_lookup(
        inputs: &ConfigInputs,
        options: ResolveOptions,
        mut lookup: impl FnMut(&[u8]) -> Option<PathBuf>,
    ) -> Result<Self, ResolveError> {
        Self::resolve_with_lookup(inputs, options, Some(&mut lookup))
    }

    fn resolve_with_lookup<'a>(
        inputs: &'a ConfigInputs,
        options: ResolveOptions,
        lookup: Option<&'a mut UserHomeLookup<'a>>,
    ) -> Result<Self, ResolveError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "config.resolve",
            outcome = "incomplete",
            failure_class = tracing::field::Empty
        );
        let operation = || {
            let mut resolver = Resolver::new(inputs, options);
            resolver.user_home_lookup = lookup;
            resolver.resolve()
        };
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
        let mut occurrences = Vec::new();
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
            occurrences.push(SectionOccurrence {
                name: SectionName {
                    section: section.to_vec(),
                    subsection: (first != last).then(|| key[first + 1..last].to_vec()),
                },
                start: index,
                entries: vec![index],
            });
            entries.push(Entry {
                section: section.to_vec(),
                subsection: (first != last).then(|| key[first + 1..last].to_vec()),
                name: name.to_vec(),
                value: Some(value),
                line,
                origin: None,
            });
        }
        Ok(Self {
            entries,
            sections: Vec::new(),
            occurrences,
        })
    }
}

type UserHomeLookup<'a> = dyn FnMut(&[u8]) -> Option<PathBuf> + 'a;

struct Resolver<'a> {
    inputs: &'a ConfigInputs,
    options: ResolveOptions,
    cache: HashMap<PathBuf, Config>,
    user_home_lookup: Option<&'a mut UserHomeLookup<'a>>,
    user_homes: HashMap<Vec<u8>, Option<PathBuf>>,
    user_home_bytes: usize,
    bytes: usize,
    visited: usize,
    expanded_bytes: usize,
    section_bytes: usize,
    occurrences: Vec<SectionOccurrence>,
    stack: Vec<PathBuf>,
    ancestry: Vec<SourceLocation>,
    urls: Vec<Vec<u8>>,
    url_bytes: usize,
    output: Vec<Entry>,
    sections: BTreeSet<super::parse::SectionName>,
    scanning: bool,
}

impl<'a> Resolver<'a> {
    fn new(inputs: &'a ConfigInputs, options: ResolveOptions) -> Self {
        Self {
            inputs,
            options,
            cache: HashMap::new(),
            user_home_lookup: None,
            user_homes: HashMap::new(),
            user_home_bytes: 0,
            bytes: 0,
            visited: 0,
            expanded_bytes: 0,
            section_bytes: 0,
            occurrences: Vec::new(),
            stack: Vec::new(),
            ancestry: Vec::new(),
            urls: Vec::new(),
            url_bytes: 0,
            output: Vec::new(),
            sections: BTreeSet::new(),
            scanning: true,
        }
    }

    fn resolve(mut self) -> Result<Config, ResolveError> {
        if self.options.conditions == IncludeConditionVisibility::AllInputs {
            self.roots()?;
        }
        self.scanning = false;
        self.visited = 0;
        self.expanded_bytes = 0;
        self.section_bytes = 0;
        self.roots()?;
        Ok(Config {
            entries: self.output,
            sections: self.sections.into_iter().collect(),
            occurrences: self.occurrences,
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
        let root_snapshot = self.options.conditions == IncludeConditionVisibility::RootSnapshot;
        let root = self.ancestry.is_empty();
        if root_snapshot && root {
            self.urls.clear();
            self.url_bytes = 0;
            append_remote_urls(
                &mut self.urls,
                &mut self.url_bytes,
                &config.entries,
                self.inputs.limits,
            )
            .map_err(|source| {
                self.error(
                    &SourceLocation {
                        path: path.map(Path::to_path_buf),
                        line: 1,
                    },
                    source,
                )
            })?;
        }
        if !self.scanning {
            self.sections.extend(config.sections.iter().cloned());
        }
        // Inputs may themselves be resolved: headers can begin between members of an outer
        // occurrence. Replay header positions without regrouping the flat entry stream.
        let mut owners = vec![0; config.entries.len()];
        for (ordinal, occurrence) in config.occurrences.iter().enumerate() {
            for &index in &occurrence.entries {
                owners[index] = ordinal;
            }
        }
        let mut placement = (!self.scanning
            && self.options.placement == IncludePlacement::AfterSectionReverse)
            .then(|| {
                SectionPlacement::new(
                    self.output.len(),
                    self.occurrences.len(),
                    config.occurrences.len(),
                )
            });
        let mut mapped = Vec::new();
        let mut headers = config.occurrences.iter().peekable();
        for index in 0..=config.entries.len() {
            while headers.peek().is_some_and(|header| header.start == index) {
                let header = headers.next().expect("peeked header");
                let location = SourceLocation {
                    path: path.map(Path::to_path_buf),
                    line: 1,
                };
                self.section_budget(
                    128usize
                        .saturating_add(header.name.section.len())
                        .saturating_add(header.name.subsection.as_ref().map_or(0, Vec::len)),
                    &location,
                )?;
                if !self.scanning {
                    mapped.push(self.occurrences.len());
                    self.occurrences.push(SectionOccurrence {
                        name: header.name.clone(),
                        start: self.output.len(),
                        entries: Vec::new(),
                    });
                }
            }
            let Some(entry) = config.entries.get(index) else {
                break;
            };
            let location = SourceLocation {
                path: path.map(Path::to_path_buf),
                line: entry.line,
            };
            self.section_budget(8, &location)?;
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
                self.occurrences[mapped[owners[index]]]
                    .entries
                    .push(self.output.len());
                self.output.push(entry);
            }
            if !entry.name.eq_ignore_ascii_case(b"path") {
                continue;
            }
            let mut hasconfig = false;
            let include =
                if self.include_section(&entry.section, b"include") && entry.subsection.is_none() {
                    true
                } else if self.include_section(&entry.section, b"includeIf") {
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
                let Some(target) = self.expand(value, &location)? else {
                    continue;
                };
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
                let first_section = self.occurrences.len();
                let first_entry = self.output.len();
                self.ancestry.push(location);
                let result = self.file(
                    &target,
                    scope,
                    true,
                    prohibited || (hasconfig && !root_snapshot),
                );
                self.ancestry.pop();
                result?;
                if root_snapshot && root {
                    append_remote_urls(
                        &mut self.urls,
                        &mut self.url_bytes,
                        &self.output[first_entry..],
                        self.inputs.limits,
                    )
                    .map_err(|source| {
                        self.error(
                            &SourceLocation {
                                path: path.map(Path::to_path_buf),
                                line: entry.line,
                            },
                            source,
                        )
                    })?;
                }
                if let Some(placement) = &mut placement {
                    placement.included(owners[index], first_section..self.occurrences.len());
                }
            }
        }
        if let Some(placement) = placement {
            placement.apply(mapped, &mut self.output, &mut self.occurrences);
        }
        Ok(())
    }

    // Independent logical metadata units, not an allocator-size estimate. Keeping this budget
    // separate preserves both the variable-count and expanded-value-byte contracts.
    fn section_budget(
        &mut self,
        bytes: usize,
        location: &SourceLocation,
    ) -> Result<(), ResolveError> {
        self.section_bytes = self
            .section_bytes
            .checked_add(bytes)
            .ok_or_else(|| self.error(location, ResolveFailure::Limit("section metadata bytes")))?;
        if self.section_bytes > self.inputs.limits.bytes {
            return Err(self.error(location, ResolveFailure::Limit("section metadata bytes")));
        }
        Ok(())
    }

    fn include_section(&self, actual: &[u8], canonical: &[u8]) -> bool {
        match self.options.directive_case {
            IncludeDirectiveCase::Insensitive => actual.eq_ignore_ascii_case(canonical),
            IncludeDirectiveCase::Canonical => actual == canonical,
        }
    }

    fn expand(
        &mut self,
        bytes: &[u8],
        location: &SourceLocation,
    ) -> Result<Option<PathBuf>, ResolveError> {
        if self.user_home_lookup.is_some() {
            if bytes.contains(&0) {
                return Err(
                    self.error(location, ResolveFailure::Input("include path contains NUL"))
                );
            }
            path_bytes(bytes).map_err(|source| self.error(location, source))?;
        }
        let context = &self.inputs.context;
        let expanded = if let Some(rest) = bytes.strip_prefix(b"~/") {
            context.home.clone().map(|home| (home, rest))
        } else if let Some(rest) = bytes.strip_prefix(b"%(prefix)/") {
            context.prefix.clone().map(|prefix| (prefix, rest))
        } else if bytes.starts_with(b"~") {
            match bytes.iter().position(|b| *b == b'/') {
                Some(slash) => self
                    .named_home(&bytes[1..slash], location)?
                    .map(|home| (home, &bytes[slash + 1..])),
                None => None,
            }
        } else {
            return path_bytes(bytes)
                .map(Some)
                .map_err(|e| self.error(location, e));
        };
        if expanded.is_none() && self.options.unresolved_paths == UnresolvedIncludePath::Skip {
            if bytes.contains(&0) {
                return Err(
                    self.error(location, ResolveFailure::Input("include path contains NUL"))
                );
            }
            path_bytes(bytes).map_err(|source| self.error(location, source))?;
            return Ok(None);
        }
        let (base, rest) = expanded.ok_or_else(|| {
            self.error(
                location,
                ResolveFailure::Input("missing path expansion context"),
            )
        })?;
        Ok(Some(base.join(
            path_bytes(rest).map_err(|e| self.error(location, e))?,
        )))
    }

    fn named_home(
        &mut self,
        name: &[u8],
        location: &SourceLocation,
    ) -> Result<Option<PathBuf>, ResolveError> {
        if let Some((_, home)) = self
            .inputs
            .context
            .user_homes
            .iter()
            .find(|(user, _)| user == name)
        {
            return Ok(Some(home.clone()));
        }
        if let Some(home) = self.user_homes.get(name) {
            return Ok(home.clone());
        }
        if self.user_home_lookup.is_none() {
            return Ok(None);
        }
        if name.contains(&0) {
            return Err(self.error(
                location,
                ResolveFailure::Input("include username contains NUL"),
            ));
        }
        if self.user_homes.len() >= self.inputs.limits.entries {
            return Err(self.error(location, ResolveFailure::Limit("user home cache entries")));
        }
        let names_bytes = self
            .user_home_bytes
            .checked_add(name.len())
            .filter(|total| *total <= self.inputs.limits.bytes)
            .ok_or_else(|| self.error(location, ResolveFailure::Limit("user home cache bytes")))?;
        let home = self.user_home_lookup.as_mut().expect("lookup present")(name);
        let retained_bytes = names_bytes
            .checked_add(
                home.as_ref()
                    .map_or(0, |path| path.as_os_str().as_encoded_bytes().len()),
            )
            .filter(|total| *total <= self.inputs.limits.bytes)
            .ok_or_else(|| self.error(location, ResolveFailure::Limit("user home cache bytes")))?;
        self.user_home_bytes = retained_bytes;
        self.user_homes.insert(name.to_vec(), home.clone());
        Ok(home)
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

    fn condition(
        &mut self,
        condition: &[u8],
        location: &SourceLocation,
    ) -> Result<bool, ResolveError> {
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
            let Some(home) = self.expand(b"~/", location)? else {
                return Ok(false);
            };
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

// Root snapshots can be seeded from caller-owned runtime data before normal traversal charges
// its entries. Charge this retained URL view before cloning, including later completed subtrees.
fn append_remote_urls(
    urls: &mut Vec<Vec<u8>>,
    bytes: &mut usize,
    entries: &[Entry],
    limits: super::ResolveLimits,
) -> Result<(), ResolveFailure> {
    for entry in entries {
        if !entry.section.eq_ignore_ascii_case(b"remote")
            || entry.subsection.is_none()
            || !entry.name.eq_ignore_ascii_case(b"url")
        {
            continue;
        }
        let Some(url) = &entry.value else {
            continue;
        };
        let next = bytes
            .checked_add(url.len())
            .ok_or(ResolveFailure::Limit("condition URL bytes"))?;
        if next > limits.bytes {
            return Err(ResolveFailure::Limit("condition URL bytes"));
        }
        if urls.len() >= limits.entries {
            return Err(ResolveFailure::Limit("condition URLs"));
        }
        *bytes = next;
        urls.push(url.clone());
    }
    Ok(())
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

    #[test]
    fn repeated_includes_resume_outer_membership_with_fresh_headers() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config");
        std::fs::write(
            &path,
            b"[include]\npath=child\nmarker=outer\npath=child\n[empty]\n[core]\nx=end\n",
        )
        .unwrap();
        std::fs::write(root.path().join("child"), b"[core]\nx=child\n[empty]\n").unwrap();
        let config = Config::resolve(&ConfigInputs {
            files: vec![super::super::ConfigFile {
                path: path.clone(),
                scope: ConfigScope::Local,
                optional: false,
            }],
            ..Default::default()
        })
        .unwrap();
        let sections: Vec<_> = config.section_occurrences().collect();
        assert_eq!(
            sections
                .iter()
                .map(|s| s.entry_indices())
                .collect::<Vec<_>>(),
            [&[0, 2, 3][..], &[1], &[], &[4], &[], &[], &[5]]
        );
        assert_eq!(
            config
                .entries()
                .iter()
                .map(|entry| entry.value.as_deref())
                .collect::<Vec<_>>(),
            [
                Some(b"child".as_slice()),
                Some(b"child".as_slice()),
                Some(b"outer".as_slice()),
                Some(b"child".as_slice()),
                Some(b"child".as_slice()),
                Some(b"end".as_slice())
            ]
        );
        assert_eq!(
            config.entries()[1].origin.as_ref().unwrap().included_from[0].line,
            2
        );
        assert_eq!(
            config.entries()[4].origin.as_ref().unwrap().included_from[0].line,
            4
        );
        assert_eq!(
            config.entries()[2]
                .origin
                .as_ref()
                .unwrap()
                .location
                .path
                .as_ref(),
            Some(&path)
        );
    }

    #[test]
    fn runtime_pairs_and_command_headers_remain_distinct() {
        let environment = Config::from_environment(
            |key| match key {
                "GIT_CONFIG_COUNT" => Some(b"2".to_vec()),
                "GIT_CONFIG_KEY_0" | "GIT_CONFIG_KEY_1" => Some(b"core.x".to_vec()),
                "GIT_CONFIG_VALUE_0" => Some(b"first".to_vec()),
                "GIT_CONFIG_VALUE_1" => Some(Vec::new()),
                _ => None,
            },
            2,
        )
        .unwrap();
        let config = Config::resolve(&ConfigInputs {
            environment: Some(environment),
            command: Some(Config::parse(b"[core]\nx\nx=last\n[empty]\n").unwrap()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            config
                .section_occurrences()
                .map(|s| s.entry_indices())
                .collect::<Vec<_>>(),
            [&[0][..], &[1], &[2, 3], &[]]
        );
        assert_eq!(config.entries()[1].value.as_deref(), Some(b"".as_slice()));
        assert_eq!(
            config.entries()[0].origin.as_ref().unwrap().scope,
            ConfigScope::Environment
        );
        assert_eq!(
            config.entries()[2].origin.as_ref().unwrap().scope,
            ConfigScope::Command
        );
    }

    #[test]
    fn resolved_runtime_input_keeps_interleaved_entries_and_headers() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("child");
        std::fs::write(&child, b"[core]\nx=child\n[empty]\n").unwrap();
        let source = format!(
            "[include]\npath={}\nmarker=outer\n[tail]\n",
            config_path(&child)
        );
        let first = Config::resolve(&ConfigInputs {
            command: Some(Config::parse(source.as_bytes()).unwrap()),
            ..Default::default()
        })
        .unwrap();
        // Missing optional include avoids a second expansion while replaying an already-resolved
        // snapshot. Its retained child header and entries still precede the resumed outer member.
        std::fs::remove_file(child).unwrap();
        let second = Config::resolve(&ConfigInputs {
            command: Some(first),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            second
                .section_occurrences()
                .map(|s| s.entry_indices())
                .collect::<Vec<_>>(),
            [&[0, 2][..], &[1], &[], &[]]
        );
        assert_eq!(
            second.entries()[1].value.as_deref(),
            Some(b"child".as_slice())
        );
        assert_eq!(
            second.entries()[2].value.as_deref(),
            Some(b"outer".as_slice())
        );
    }

    #[rstest]
    #[case::exhausted(924, false)]
    #[case::exact(925, true)]
    fn repeated_empty_include_headers_consume_metadata_budget(
        #[case] bytes: usize,
        #[case] succeeds: bool,
    ) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config");
        std::fs::write(&path, b"[include]\npath=child\npath=child\n").unwrap();
        std::fs::write(root.path().join("child"), b"[x]\n[y]\n[z]\n").unwrap();
        let result = Config::resolve(&ConfigInputs {
            files: vec![super::super::ConfigFile {
                path,
                scope: ConfigScope::Local,
                optional: false,
            }],
            limits: super::super::ResolveLimits {
                bytes,
                entries: 2,
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(result.is_ok(), succeeds);
        assert!(result.as_ref().err().is_none_or(|error| matches!(
            error.source,
            ResolveFailure::Limit("section metadata bytes")
        )));
    }

    #[test]
    fn metadata_accounting_rejects_arithmetic_overflow() {
        let inputs = ConfigInputs {
            limits: super::super::ResolveLimits {
                bytes: usize::MAX,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut resolver = Resolver::new(&inputs, ResolveOptions::default());
        let location = SourceLocation {
            path: None,
            line: 1,
        };
        resolver.section_budget(1, &location).unwrap();
        assert!(matches!(
            resolver
                .section_budget(usize::MAX, &location)
                .unwrap_err()
                .source,
            ResolveFailure::Limit("section metadata bytes")
        ));
    }

    #[test]
    fn subsection_names_include_inherited_headers_and_runtime_assignments() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("child"),
            b"[remote \"empty\"]\n[remote \"duplicate\"]\n",
        )
        .unwrap();
        let environment = Config::from_environment(
            |name| match name {
                "GIT_CONFIG_COUNT" => Some(b"2".to_vec()),
                "GIT_CONFIG_KEY_0" => Some(b"remote.runtime.url".to_vec()),
                "GIT_CONFIG_VALUE_0" => Some(Vec::new()),
                "GIT_CONFIG_KEY_1" => Some(b"remote.duplicate.url".to_vec()),
                "GIT_CONFIG_VALUE_1" => Some(b"repo".to_vec()),
                _ => None,
            },
            2,
        )
        .unwrap();
        assert_eq!(
            environment.subsection_names("REMOTE"),
            [b"duplicate".as_slice(), b"runtime"]
        );
        let mut input = ConfigInputs {
            environment: Some(environment),
            command: Some(
                Config::parse(b"[include]\npath=~/child\npath=~/child\n[remote \"command\"]\n")
                    .unwrap(),
            ),
            ..Default::default()
        };
        input.context.home = Some(root.path().into());
        let config = Config::resolve(&input).unwrap();
        assert_eq!(
            config.subsection_names("remote"),
            [b"command".as_slice(), b"duplicate", b"empty", b"runtime"]
        );
        assert_eq!(
            config
                .entries()
                .iter()
                .filter(|entry| entry.section == b"remote")
                .count(),
            2
        );
    }

    #[rstest]
    #[case::empty(b"", false)]
    #[case::header(b"[ReMoTe \"Name\"]", true)]
    #[case::nonempty(b"[remote \"Name\"]\nurl=value", true)]
    #[case::different_case(b"[remote \"name\"]", false)]
    #[case::missing_subsection(b"[remote]", false)]
    fn section_existence_survives_resolution(#[case] bytes: &[u8], #[case] exists: bool) {
        let parsed = Config::parse(bytes).unwrap();
        assert_eq!(parsed.contains_section("remote", Some(b"Name")), exists);
        let resolved = Config::resolve(&ConfigInputs {
            command: Some(parsed),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(resolved.contains_section("REMOTE", Some(b"Name")), exists);
    }

    #[test]
    fn included_empty_headers_survive_without_entry_or_provenance_changes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("child"),
            b"[remote \"empty\"]\n[remote \"\xff\"]\n[a]\nb=child\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("parent"),
            b"[a]\nb=before\n[include]\npath=child\npath=child\n[a]\nb=after\n",
        )
        .unwrap();
        let resolved = Config::resolve(&ConfigInputs {
            files: vec![super::super::ConfigFile {
                path: root.path().join("parent"),
                scope: ConfigScope::Global,
                optional: false,
            }],
            ..Default::default()
        })
        .unwrap();
        assert!(resolved.contains_section("remote", Some(b"empty")));
        // Repeated includes retain existence once, without multiplying header storage.
        assert_eq!(resolved.sections.len(), 4);
        assert!(resolved.contains_section("remote", Some(b"\xff")));
        assert_eq!(
            resolved.values("a", None, "b").collect::<Vec<_>>(),
            [
                Some(b"before".as_slice()),
                Some(b"child".as_slice()),
                Some(b"child".as_slice()),
                Some(b"after".as_slice())
            ]
        );
        let child = resolved
            .entries()
            .iter()
            .find(|entry| entry.name == b"b" && entry.value.as_deref() == Some(b"child"))
            .unwrap();
        let origin = child.origin.as_ref().unwrap();
        assert_eq!(origin.scope, ConfigScope::Global);
        assert_eq!(origin.location.path, Some(root.path().join("child")));
        assert_eq!(origin.included_from.len(), 1);
        assert!(
            crate::remote::ConfiguredRemoteRecord::find(&resolved, b"empty")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn runtime_assignment_implies_section_existence() {
        let environment = Config::from_environment(
            |name| match name {
                "GIT_CONFIG_COUNT" => Some(b"1".to_vec()),
                "GIT_CONFIG_KEY_0" => Some(b"remote.origin.url".to_vec()),
                "GIT_CONFIG_VALUE_0" => Some(Vec::new()),
                _ => None,
            },
            10,
        )
        .unwrap();
        assert!(environment.contains_section("remote", Some(b"origin")));
        let resolved = Config::resolve(&ConfigInputs {
            environment: Some(environment),
            ..Default::default()
        })
        .unwrap();
        assert!(resolved.contains_section("remote", Some(b"origin")));
        assert!(
            crate::remote::ConfiguredRemoteRecord::find(&resolved, b"origin")
                .unwrap()
                .is_some()
        );
        assert!(
            !Config::resolve(&ConfigInputs::default())
                .unwrap()
                .contains_section("remote", Some(b"origin"))
        );
    }

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
        let mut resolver = Resolver::new(&inputs, ResolveOptions::default());
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

    /// Renders a path as a config value; backslashes (Windows separators) must be escaped.
    fn config_path(path: &std::path::Path) -> String {
        path.display().to_string().replace('\\', "\\\\")
    }
}
