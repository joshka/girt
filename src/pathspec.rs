//! Pathspecs: the patterns Git commands use to choose paths.
//!
//! A pathspec such as `src/*.rs`, `:(exclude)tests` or `:/README` selects repository paths for
//! status, diff, history and similar path-limited operations. [`Pathspec::parse`] compiles a list
//! of them once against a [`Context`] (the current directory inside the worktree and the
//! `GIT_*_PATHSPECS` defaults); [`Pathspec::matches`] then tests repository-relative paths and
//! [`Pathspec::directory`] tells a walk whether a directory can be skipped, taken whole, or must be
//! entered.
//!
//! # Matching
//!
//! A spec without wildcards selects the path it names and everything under it, on component
//! boundaries: `src` selects `src/a.rs` but `sr` selects nothing, and `src/` selects only paths
//! below `src`. A spec with wildcards (`*`, `?`, `[...]`, or a `\` escape) also matches when the
//! wildcard pattern matches the whole path. Without `glob` magic `*` and `?` match `/`, so `*.rs`
//! selects `src/sub/b.rs`; with it they stop at `/` and `**` matches whole components. A wildcard
//! match against a directory name does not select its contents.
//!
//! A path is selected when at least one include spec matches it and no exclude spec does. A list
//! made only of excludes first includes everything under the current directory, as Git does. An
//! empty list selects every path; [`Pathspec::all`] builds that value directly.
//!
//! # Magic
//!
//! A leading `:` introduces magic, either short (`:/` top, `:!` or `:^` exclude, ended by the first
//! other byte or a second `:`) or long (`:(top,icase)`). Supported magic is `top`, `exclude`,
//! `icase`, `literal`, `glob` and `attr:`. `icase` folds ASCII letters only; a filesystem's case
//! insensitivity, or `core.ignoreCase`, does not make pathspecs case-insensitive. Unknown magic,
//! `glob` with `literal`, and malformed `attr:` lists are [`ParseError`]s.
//!
//! # Attributes
//!
//! `:(attr:text -diff !eol filter=lfs)` adds [`Requirement`]s that a path's attributes must all
//! meet: set, unset, unspecified, or equal to a value. Matching them needs attribute lookup, so
//! use [`Pathspec::matches_with`] and answer each requirement from
//! [`attributes::Lookup`](crate::attributes::Lookup) or
//! [`attributes::Attributes`](crate::attributes::Attributes).
//! [`Pathspec::uses_attributes`] reports whether a list needs it.
//!
//! # Example
//!
//! ```
//! use girt::pathspec::{Context, Pathspec, Prune};
//!
//! // As if run from the `src` directory of the worktree.
//! let context = Context::new().with_prefix(b"src");
//! let spec = Pathspec::parse(["*.rs", ":!gen", ":/README"], &context)?;
//!
//! assert!(spec.matches(b"src/lib.rs"));
//! assert!(spec.matches(b"src/sub/mod.rs"));
//! assert!(!spec.matches(b"src/gen/out.rs"));
//! assert!(spec.matches(b"README"));
//! assert!(!spec.matches(b"tests/t.rs"));
//!
//! assert_eq!(spec.directory(b"tests"), Prune::Skip);
//! assert_eq!(spec.directory(b"src/gen"), Prune::Skip);
//! assert_eq!(spec.directory(b"src"), Prune::Descend);
//! # Ok::<(), girt::pathspec::ParseError>(())
//! ```

use std::fmt;

use crate::attributes::State;
use crate::display::quote_path;

/// The directory and environment defaults pathspecs are parsed against.
///
/// [`Context::new`] describes the worktree root with no environment defaults;
/// [`Context::from_env`] reads `GIT_GLOB_PATHSPECS`, `GIT_NOGLOB_PATHSPECS`,
/// `GIT_ICASE_PATHSPECS` and `GIT_LITERAL_PATHSPECS` the way Git does.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Context {
    prefix: Vec<u8>,
    glob: bool,
    noglob: bool,
    icase: bool,
    literal: bool,
}

impl Context {
    /// Returns a context at the worktree root with every default off.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads the `GIT_*_PATHSPECS` defaults from the process environment.
    ///
    /// Values use Git's boolean spelling (`1`, `true`, `yes`, `on`, `0`, `false`, `no`, `off`,
    /// empty, or an integer); unset variables are off.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::Environment`] for a value that is not a boolean, and
    /// [`ParseError::GlobAndNoglob`] when both glob variables are on.
    pub fn from_env() -> Result<Self, ParseError> {
        Self::from_vars(|name| std::env::var_os(name))
    }

    fn from_vars(
        mut var: impl FnMut(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Self, ParseError> {
        let mut flag = |name: &'static str| -> Result<bool, ParseError> {
            let Some(value) = var(name) else {
                return Ok(false);
            };
            let value = value.as_encoded_bytes().to_vec();
            crate::config::values::boolean(Some(&value)).ok_or(ParseError::Environment {
                variable: name,
                value,
            })
        };
        let context = Self {
            prefix: Vec::new(),
            glob: flag("GIT_GLOB_PATHSPECS")?,
            noglob: flag("GIT_NOGLOB_PATHSPECS")?,
            icase: flag("GIT_ICASE_PATHSPECS")?,
            literal: flag("GIT_LITERAL_PATHSPECS")?,
        };
        if context.glob && context.noglob {
            return Err(ParseError::GlobAndNoglob);
        }
        Ok(context)
    }

    /// Sets the current directory as a repository-relative path, such as `src/sub`.
    ///
    /// Relative specs resolve against it, and a list of only excludes includes everything under
    /// it. Leading, trailing and repeated `/` are ignored.
    #[must_use]
    pub fn with_prefix(mut self, prefix: &[u8]) -> Self {
        self.prefix = prefix
            .split(|byte| *byte == b'/')
            .filter(|component| !component.is_empty())
            .collect::<Vec<_>>()
            .join(&b'/');
        self
    }

    /// Makes `*` stop at `/` unless a spec has `literal` magic (`GIT_GLOB_PATHSPECS`).
    #[must_use]
    pub fn with_glob(mut self, glob: bool) -> Self {
        self.glob = glob;
        self
    }

    /// Treats wildcards literally unless a spec has `glob` magic (`GIT_NOGLOB_PATHSPECS`).
    #[must_use]
    pub fn with_noglob(mut self, noglob: bool) -> Self {
        self.noglob = noglob;
        self
    }

    /// Folds ASCII case in every spec (`GIT_ICASE_PATHSPECS`).
    #[must_use]
    pub fn with_icase(mut self, icase: bool) -> Self {
        self.icase = icase;
        self
    }

    /// Treats every spec as a literal path with no magic or wildcards (`GIT_LITERAL_PATHSPECS`).
    #[must_use]
    pub fn with_literal(mut self, literal: bool) -> Self {
        self.literal = literal;
        self
    }

    /// Returns the current directory set by [`Self::with_prefix`].
    pub fn prefix(&self) -> &[u8] {
        &self.prefix
    }
}

/// A compiled pathspec list.
///
/// Immutable and cheap to share; build it once and test many paths. See the
/// [module documentation](self) for the matching rules.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Pathspec {
    includes: Vec<Item>,
    excludes: Vec<Item>,
}

/// What a directory walk should do with one directory.
///
/// Variants are ordered from least to most selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Prune {
    /// Nothing under the directory can match; skip it.
    Skip,
    /// Some paths under the directory may match; enter it and test each path.
    Descend,
    /// Every path under the directory matches; take it without further tests.
    All,
}

impl Pathspec {
    /// Returns a pathspec that selects every path, as an empty list does.
    pub fn all() -> Self {
        Self::default()
    }

    /// Compiles `specs` against `context`.
    ///
    /// # Errors
    ///
    /// Returns a [`ParseError`] for an empty spec, malformed or unknown magic, incompatible magic,
    /// a malformed `attr:` list, or a path that leaves the repository.
    pub fn parse<I>(specs: I, context: &Context) -> Result<Self, ParseError>
    where
        I: IntoIterator,
        I::Item: AsRef<[u8]>,
    {
        let mut pathspec = Self::default();
        for spec in specs {
            let item = Item::parse(spec.as_ref(), context)?;
            if item.exclude {
                pathspec.excludes.push(item);
            } else {
                pathspec.includes.push(item);
            }
        }
        if pathspec.includes.is_empty() && !pathspec.excludes.is_empty() {
            pathspec
                .includes
                .push(Item::literal(context.prefix.clone()));
        }
        Ok(pathspec)
    }

    /// Returns whether this pathspec selects every path.
    pub fn is_all(&self) -> bool {
        self.excludes.is_empty()
            && self
                .includes
                .iter()
                .all(|item| item.pattern.is_empty() && item.attributes.is_empty())
    }

    /// Tests a repository-relative path such as `src/a.rs`.
    ///
    /// The path uses `/` separators with no leading or trailing `/`. Leaf paths (files, symbolic
    /// links and gitlinks) are what Git tests; use [`Self::directory`] for directories.
    ///
    /// Every attribute is treated as unspecified, as if no `.gitattributes` applied. When
    /// [`Self::uses_attributes`] is true, use [`Self::matches_with`] instead.
    pub fn matches(&self, path: &[u8]) -> bool {
        let unspecified = |requirement: &Requirement| {
            Ok::<_, std::convert::Infallible>(requirement.accepts(State::Unspecified))
        };
        match self.matches_with(path, unspecified) {
            Ok(matched) => matched,
            Err(never) => match never {},
        }
    }

    /// Tests a path, asking `satisfied` whether the path meets each `attr:` [`Requirement`].
    ///
    /// `satisfied` is called only for specs whose pattern already matches, and an error from it
    /// is returned unchanged.
    ///
    /// ```
    /// use girt::attributes::{Attributes, Case, Limits, Source};
    /// use girt::pathspec::{Context, Pathspec};
    ///
    /// let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
    /// attributes.add(Source::Info, b"*.rs rust\n")?;
    /// let spec = Pathspec::parse([":(attr:rust)src"], &Context::new())?;
    ///
    /// let rust = |path: &[u8]| {
    ///     spec.matches_with(path, |requirement| {
    ///         Ok::<_, girt::Error>(requirement.accepts(attributes.get(path, requirement.name())?))
    ///     })
    /// };
    /// assert!(rust(b"src/lib.rs")?);
    /// assert!(!rust(b"src/notes.md")?);
    /// assert!(!rust(b"build.rs")?);
    /// # Ok::<(), girt::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns the first error `satisfied` returns.
    pub fn matches_with<E>(
        &self,
        path: &[u8],
        mut satisfied: impl FnMut(&Requirement) -> Result<bool, E>,
    ) -> Result<bool, E> {
        let mut included = self.includes.is_empty();
        for item in &self.includes {
            if item.matches(path, &mut satisfied)? {
                included = true;
                break;
            }
        }
        if !included {
            return Ok(false);
        }
        for item in &self.excludes {
            if item.matches(path, &mut satisfied)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Returns whether any spec has `attr:` magic, so matching needs [`Self::matches_with`].
    pub fn uses_attributes(&self) -> bool {
        self.includes
            .iter()
            .chain(&self.excludes)
            .any(|item| !item.attributes.is_empty())
    }

    /// Classifies a repository-relative directory for pruning a walk.
    ///
    /// [`Prune::Skip`] guarantees [`Self::matches`] is false for every path below `directory`, and
    /// [`Prune::All`] guarantees it is true. Pass an empty slice for the worktree root.
    pub fn directory(&self, directory: &[u8]) -> Prune {
        if self.includes.is_empty() && self.excludes.is_empty() {
            return Prune::All;
        }
        let mut excluded = Prune::Skip;
        for item in &self.excludes {
            excluded = excluded.max(item.directory(directory));
            if excluded == Prune::All {
                return Prune::Skip;
            }
        }
        let included = self
            .includes
            .iter()
            .map(|item| item.directory(directory))
            .max()
            .unwrap_or(Prune::All);
        if included == Prune::All && excluded == Prune::Descend {
            Prune::Descend
        } else {
            included
        }
    }
}

/// One compiled spec with its pattern resolved to the repository root.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Item {
    /// Repository-relative pattern; a trailing `/` limits it to paths below a directory.
    pattern: Vec<u8>,
    /// Length of the leading part with no wildcard bytes; equals `pattern.len()` when literal.
    literal_len: usize,
    exclude: bool,
    icase: bool,
    glob: bool,
    /// `attr:` requirements, all of which must hold.
    attributes: Vec<Requirement>,
}

impl Item {
    fn literal(pattern: Vec<u8>) -> Self {
        Self {
            literal_len: pattern.len(),
            pattern,
            exclude: false,
            icase: false,
            glob: false,
            attributes: Vec::new(),
        }
    }

    fn parse(spec: &[u8], context: &Context) -> Result<Self, ParseError> {
        if spec.is_empty() {
            return Err(ParseError::Empty);
        }
        let error = |reason| ParseError::Invalid {
            spec: spec.to_vec(),
            reason,
        };
        let mut magic = Magic::default();
        let path = if context.literal {
            magic.literal = true;
            spec
        } else {
            magic.parse(spec).map_err(error)?
        };
        if magic.glob && magic.literal {
            return Err(error(Reason::GlobWithLiteral));
        }
        let literal = magic.literal || (context.noglob && !magic.glob);
        let glob = !literal && (magic.glob || context.glob);
        let joined;
        let path = if magic.top || context.prefix.is_empty() {
            path
        } else {
            joined = [&context.prefix[..], b"/", path].concat();
            &joined
        };
        let pattern = normalize(path).ok_or_else(|| error(Reason::OutsideRepository))?;
        let literal_len = if literal {
            pattern.len()
        } else {
            pattern
                .iter()
                .position(|byte| matches!(byte, b'*' | b'?' | b'[' | b'\\'))
                .unwrap_or(pattern.len())
        };
        Ok(Self {
            pattern,
            literal_len,
            exclude: magic.exclude,
            icase: magic.icase || context.icase,
            glob,
            attributes: magic.attributes.unwrap_or_default(),
        })
    }

    fn has_wildcards(&self) -> bool {
        self.literal_len < self.pattern.len()
    }

    fn matches<E>(
        &self,
        path: &[u8],
        satisfied: &mut impl FnMut(&Requirement) -> Result<bool, E>,
    ) -> Result<bool, E> {
        if !self.matches_pattern(path) {
            return Ok(false);
        }
        for requirement in &self.attributes {
            if !satisfied(requirement)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn matches_pattern(&self, path: &[u8]) -> bool {
        let prefix = &self.pattern[..self.literal_len];
        if !path
            .get(..prefix.len())
            .is_some_and(|start| self.equal(start, prefix))
        {
            return false;
        }
        if self.names(path) {
            return true;
        }
        self.has_wildcards()
            && crate::wildmatch::matches_with_pathname(
                &self.pattern,
                path,
                self.icase,
                usize::MAX,
                self.glob,
            )
            .unwrap_or(false)
    }

    /// Whether the whole pattern, read literally, names `path` or a directory above it.
    fn names(&self, path: &[u8]) -> bool {
        let pattern = &self.pattern[..];
        if pattern.is_empty() {
            return true;
        }
        let Some(start) = path.get(..pattern.len()) else {
            return false;
        };
        self.equal(start, pattern)
            && (pattern.ends_with(b"/")
                || path.len() == pattern.len()
                || path.get(pattern.len()) == Some(&b'/'))
    }

    fn directory(&self, directory: &[u8]) -> Prune {
        let prune = self.directory_pattern(directory);
        if prune == Prune::All && !self.attributes.is_empty() {
            Prune::Descend
        } else {
            prune
        }
    }

    fn directory_pattern(&self, directory: &[u8]) -> Prune {
        let named = self.pattern.strip_suffix(b"/").unwrap_or(&self.pattern);
        if named.is_empty() || self.below(directory, named) {
            return Prune::All;
        }
        let mut inside = directory.to_vec();
        if !inside.is_empty() {
            inside.push(b'/');
        }
        if self.above(&inside, named) {
            return Prune::Descend;
        }
        // A wildcard can match anything that starts with the literal prefix.
        let prefix = &self.pattern[..self.literal_len];
        if self.has_wildcards() && (self.above(prefix, &inside) || self.above(&inside, prefix)) {
            Prune::Descend
        } else {
            Prune::Skip
        }
    }

    /// Whether `directory` is `named` or lies below it.
    fn below(&self, directory: &[u8], named: &[u8]) -> bool {
        directory.get(..named.len()).is_some_and(|start| {
            self.equal(start, named)
                && (directory.len() == named.len() || directory.get(named.len()) == Some(&b'/'))
        })
    }

    /// Whether `long` starts with `short`.
    fn above(&self, short: &[u8], long: &[u8]) -> bool {
        long.get(..short.len())
            .is_some_and(|start| self.equal(start, short))
    }

    fn equal(&self, left: &[u8], right: &[u8]) -> bool {
        if self.icase {
            left.eq_ignore_ascii_case(right)
        } else {
            left == right
        }
    }
}

#[derive(Default)]
struct Magic {
    top: bool,
    exclude: bool,
    icase: bool,
    literal: bool,
    glob: bool,
    attributes: Option<Vec<Requirement>>,
}

impl Magic {
    /// Consumes leading magic and returns the path that follows it.
    fn parse<'a>(&mut self, spec: &'a [u8]) -> Result<&'a [u8], Reason> {
        let Some(rest) = spec.strip_prefix(b":") else {
            return Ok(spec);
        };
        if let Some(long) = rest.strip_prefix(b"(") {
            // A backslash keeps the next byte, including `,` and `)`, inside an `attr:` value.
            let mut start = 0;
            let mut index = 0;
            loop {
                match long.get(index) {
                    None => return Err(Reason::UnclosedMagic),
                    Some(b'\\') => index += 2,
                    Some(b',') => {
                        self.word(&long[start..index])?;
                        index += 1;
                        start = index;
                    }
                    Some(b')') => {
                        self.word(&long[start..index])?;
                        return Ok(&long[index + 1..]);
                    }
                    Some(_) => index += 1,
                }
            }
        }
        let mut rest = rest;
        while let [byte, after @ ..] = rest {
            match byte {
                b'/' => self.top = true,
                b'!' | b'^' => self.exclude = true,
                b':' => return Ok(after),
                _ => break,
            }
            rest = after;
        }
        Ok(rest)
    }

    fn word(&mut self, word: &[u8]) -> Result<(), Reason> {
        match word {
            b"" => {}
            b"top" => self.top = true,
            b"exclude" => self.exclude = true,
            b"icase" => self.icase = true,
            b"literal" => self.literal = true,
            b"glob" => self.glob = true,
            _ if let Some(list) = word.strip_prefix(b"attr:") => {
                if self.attributes.is_some() {
                    return Err(Reason::MultipleAttr);
                }
                self.attributes = Some(Requirement::parse_list(list)?);
            }
            _ => return Err(Reason::UnknownMagic(word.to_vec())),
        }
        Ok(())
    }
}

/// One condition from `attr:` magic on a path's attribute.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Requirement {
    name: Vec<u8>,
    expected: Expected,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum Expected {
    Set,
    Unset,
    Unspecified,
    Value(Vec<u8>),
}

impl Requirement {
    /// The attribute name to look up for the path.
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// Whether the path's state for [`Self::name`] meets this requirement.
    ///
    /// `name` requires [`State::Set`], `-name` [`State::Unset`], `!name` [`State::Unspecified`]
    /// and `name=value` a [`State::Value`] with exactly those bytes.
    pub fn accepts(&self, state: State<'_>) -> bool {
        match (&self.expected, state) {
            (Expected::Set, State::Set)
            | (Expected::Unset, State::Unset)
            | (Expected::Unspecified, State::Unspecified) => true,
            (Expected::Value(expected), State::Value(actual)) => expected == actual,
            _ => false,
        }
    }

    /// Parses the space-separated list after `attr:`.
    fn parse_list(list: &[u8]) -> Result<Vec<Self>, Reason> {
        let requirements = list
            .split(|byte| *byte == b' ')
            .filter(|word| !word.is_empty())
            .map(Self::parse)
            .collect::<Result<Vec<_>, _>>()?;
        if requirements.is_empty() {
            return Err(Reason::EmptyAttr);
        }
        Ok(requirements)
    }

    fn parse(word: &[u8]) -> Result<Self, Reason> {
        let (name, expected) = match word {
            [b'-', name @ ..] => (name, Expected::Unset),
            [b'!', name @ ..] => (name, Expected::Unspecified),
            _ => match crate::bytes::split_once(word, b'=') {
                Some((name, value)) => (name, Expected::Value(unescape(value)?)),
                None => (word, Expected::Set),
            },
        };
        if !crate::attributes::valid_name(name) {
            return Err(Reason::InvalidAttrName(name.to_vec()));
        }
        Ok(Self {
            name: name.to_vec(),
            expected,
        })
    }
}

/// Removes backslash escapes from an `attr:` value.
fn unescape(value: &[u8]) -> Result<Vec<u8>, Reason> {
    let mut unescaped = Vec::with_capacity(value.len());
    let mut bytes = value.iter();
    while let Some(byte) = bytes.next() {
        if *byte == b'\\' {
            unescaped.push(*bytes.next().ok_or(Reason::TrailingEscape)?);
        } else {
            unescaped.push(*byte);
        }
    }
    Ok(unescaped)
}

/// Resolves `.`, `..` and repeated `/`, keeping a trailing `/` that follows a named component.
///
/// Returns `None` when `..` climbs above the root or the path is absolute.
fn normalize(path: &[u8]) -> Option<Vec<u8>> {
    if path.starts_with(b"/") {
        return None;
    }
    let mut components: Vec<&[u8]> = Vec::new();
    let mut directory = false;
    for component in path.split(|byte| *byte == b'/') {
        directory = true;
        match component {
            b"" => {}
            b"." => directory = false,
            b".." => {
                components.pop()?;
                directory = false;
            }
            _ => {
                components.push(component);
                directory = false;
            }
        }
    }
    let mut normalized = components.join(&b'/');
    if directory && !normalized.is_empty() {
        normalized.push(b'/');
    }
    Some(normalized)
}

/// A pathspec that could not be compiled.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    /// An empty string; Git asks for `.` to mean every path.
    #[error("empty string is not a valid pathspec; use . to match all paths")]
    Empty,
    /// One spec is malformed.
    #[error("invalid pathspec {}: {reason}", quote_path(spec))]
    Invalid {
        /// The spec as given.
        spec: Vec<u8>,
        /// What is wrong with it.
        reason: Reason,
    },
    /// A `GIT_*_PATHSPECS` variable holds a value that is not a Git boolean.
    #[error("bad boolean value {} for {variable}", quote_path(value))]
    Environment {
        /// The variable name.
        variable: &'static str,
        /// Its value.
        value: Vec<u8>,
    },
    /// `GIT_GLOB_PATHSPECS` and `GIT_NOGLOB_PATHSPECS` are both on.
    #[error("global glob and noglob pathspec settings are incompatible")]
    GlobAndNoglob,
}

/// Why one spec was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Reason {
    /// Long magic opened with `:(` has no closing `)`.
    UnclosedMagic,
    /// A long magic word Git does not define; names are case-sensitive.
    UnknownMagic(Vec<u8>),
    /// `glob` and `literal` magic on the same spec.
    GlobWithLiteral,
    /// More than one `attr:` entry on the same spec.
    MultipleAttr,
    /// `attr:` with no attribute names.
    EmptyAttr,
    /// An `attr:` name with bytes other than ASCII letters, digits, `-`, `.` and `_`, or starting
    /// with `-`.
    InvalidAttrName(Vec<u8>),
    /// An `attr:` value that ends with a backslash.
    TrailingEscape,
    /// The path is absolute or `..` climbs above the repository root.
    OutsideRepository,
}

impl fmt::Display for Reason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnclosedMagic => formatter.write_str("missing ')' after pathspec magic"),
            Self::UnknownMagic(word) => write!(formatter, "unknown magic '{}'", quote_path(word)),
            Self::GlobWithLiteral => formatter.write_str("'glob' and 'literal' are incompatible"),
            Self::MultipleAttr => formatter.write_str("only one 'attr:' specification is allowed"),
            Self::EmptyAttr => formatter.write_str("'attr:' must name an attribute"),
            Self::InvalidAttrName(name) => {
                write!(formatter, "invalid attribute name '{}'", quote_path(name))
            }
            Self::TrailingEscape => formatter.write_str("'attr:' value ends with '\\'"),
            Self::OutsideRepository => formatter.write_str("path is outside the repository"),
        }
    }
}

#[cfg(test)]
mod tests;
