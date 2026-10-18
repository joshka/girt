use std::collections::{HashMap, HashSet};

use super::Case;
use crate::ignore::pattern::Pattern;

/// The state of one attribute for one path.
///
/// These are Git's four attribute states, so the set is closed. A [`Self::Value`] borrows the
/// bytes from the [`Attributes`] that resolved it.
///
/// ```
/// use girt::attributes::{Attributes, Case, Limits, Source, State};
///
/// let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
/// attributes.add(Source::Info, b"a text\nb -text\nc text=auto\nd !text\n")?;
/// assert_eq!(attributes.get(b"a", b"text")?, State::Set);
/// assert_eq!(attributes.get(b"b", b"text")?, State::Unset);
/// assert_eq!(attributes.get(b"c", b"text")?, State::Value(b"auto"));
/// assert_eq!(attributes.get(b"d", b"text")?, State::Unspecified);
/// # Ok::<(), girt::attributes::Error>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum State<'a> {
    /// No line assigns the attribute, or the deciding line uses `!name`.
    Unspecified,
    /// Assigned as `name`.
    Set,
    /// Assigned as `-name`.
    Unset,
    /// Assigned as `name=value`; the value may be empty.
    Value(&'a [u8]),
}

/// An attribute file's precedence and, for `.gitattributes`, its directory.
///
/// Later variants override earlier ones and deeper directories override shallower ones. Adding
/// another file at the same level appends its lines after the existing ones.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Source<'a> {
    /// The system-wide attributes file.
    System,
    /// The file `core.attributesFile` names, or its XDG default.
    Global,
    /// `.gitattributes` in this slash-separated directory; the empty slice denotes the root.
    Directory(&'a [u8]),
    /// The common directory's `info/attributes`.
    Info,
}

/// Resource budgets for a whole attribute set and each query.
///
/// Storage is linear in the input bytes, with the per-pattern constant of
/// [`ignore::Limits`](crate::ignore::Limits). Queries do not recurse.
///
/// ```
/// use girt::attributes::{Attributes, Case, Error, Limits, Source};
///
/// let mut attributes = Attributes::new(Case::Sensitive, Limits::default().with_lines(1));
/// attributes.add(Source::Info, b"*.png binary\n")?;
/// assert!(matches!(
///     attributes.add(Source::Global, b"*.jpg binary\n"),
///     Err(Error::Limit(_))
/// ));
/// # Ok::<(), girt::attributes::Error>(())
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Aggregate file content and directory-prefix bytes; default 8 MiB.
    pub bytes: usize,
    /// Number of added files, including empty ones; default 4096.
    pub sources: usize,
    /// Retained pattern and macro lines; default 100,000.
    pub lines: usize,
    /// Maximum line bytes excluding LF and the initial BOM; default 4096.
    pub line_bytes: usize,
    /// Maximum query or directory path length; default 4096 bytes.
    pub path_bytes: usize,
    /// Aggregate matching work per query, as [`ignore::Limits::work`](crate::ignore::Limits::work)
    /// counts it; default 100 million units.
    pub work: usize,
}

impl Limits {
    /// Returns limits for a repository the caller trusts as Git trusts the user's own.
    ///
    /// Git bounds none of these values; only available memory bounds the work.
    pub const fn trusted() -> Self {
        Self {
            bytes: usize::MAX / 4,
            sources: usize::MAX / 4,
            lines: usize::MAX / 4,
            line_bytes: usize::MAX / 4,
            path_bytes: usize::MAX / 4,
            work: usize::MAX / 4,
        }
    }

    /// Sets [`Self::bytes`].
    #[must_use]
    pub fn with_bytes(mut self, bytes: usize) -> Self {
        self.bytes = bytes;
        self
    }

    /// Sets [`Self::sources`].
    #[must_use]
    pub fn with_sources(mut self, sources: usize) -> Self {
        self.sources = sources;
        self
    }

    /// Sets [`Self::lines`].
    #[must_use]
    pub fn with_lines(mut self, lines: usize) -> Self {
        self.lines = lines;
        self
    }

    /// Sets [`Self::line_bytes`].
    #[must_use]
    pub fn with_line_bytes(mut self, line_bytes: usize) -> Self {
        self.line_bytes = line_bytes;
        self
    }

    /// Sets [`Self::path_bytes`].
    #[must_use]
    pub fn with_path_bytes(mut self, path_bytes: usize) -> Self {
        self.path_bytes = path_bytes;
        self
    }

    /// Sets [`Self::work`].
    #[must_use]
    pub fn with_work(mut self, work: usize) -> Self {
        self.work = work;
        self
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            bytes: 8 * 1024 * 1024,
            sources: 4096,
            lines: 100_000,
            line_bytes: 4096,
            path_bytes: 4096,
            work: 100_000_000,
        }
    }
}

/// An invalid path or exhausted budget; an error never implies an attribute state.
///
/// ```
/// use girt::attributes::{Attributes, Case, Error, Limits};
///
/// let attributes = Attributes::new(Case::Sensitive, Limits::default());
/// assert_eq!(attributes.get(b"../outside", b"diff"), Err(Error::Path));
/// ```
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[non_exhaustive]
pub enum Error {
    /// Paths must be relative and nonempty, without NUL, empty, `.` or `..` components. The root
    /// directory source alone permits an empty path.
    #[error("invalid repository-relative attribute path")]
    Path,
    /// A named input, retained-storage or matching-work limit was exceeded.
    #[error("attribute {0} limit exceeded")]
    Limit(&'static str),
}

/// An owned set of compiled attribute files.
///
/// See the [module documentation](super) for the syntax and precedence rules. Adding a file
/// compiles it once; each [`Self::get`] walks the files from the highest precedence down and stops
/// at the first line that decides the attribute.
///
/// ```
/// use girt::attributes::{Attributes, Case, Limits, Source, State};
///
/// let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
/// attributes.add(
///     Source::Directory(b""),
///     b"[attr]generated -diff linguist-generated\n",
/// )?;
/// attributes.add(Source::Directory(b"api"), b"*.pb.go generated\n")?;
///
/// assert_eq!(attributes.get(b"api/v1/x.pb.go", b"diff")?, State::Unset);
/// assert_eq!(
///     attributes.get(b"api/v1/x.pb.go", b"linguist-generated")?,
///     State::Set
/// );
/// # Ok::<(), girt::attributes::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct Attributes {
    case: Case,
    limits: Limits,
    /// Sorted by ascending precedence.
    sources: Vec<Rules>,
    /// The highest-precedence definition of each macro, rebuilt when a file is added.
    macros: HashMap<Vec<u8>, Vec<Assignment>>,
    bytes: usize,
    lines: usize,
}

#[derive(Clone, Debug)]
struct Rules {
    base: Vec<u8>,
    rank: (u8, usize, usize),
    lines: Vec<Line>,
    macros: Vec<Macro>,
}

#[derive(Clone, Debug)]
struct Line {
    pattern: Pattern,
    assignments: Vec<Assignment>,
}

#[derive(Clone, Debug)]
struct Macro {
    name: Vec<u8>,
    assignments: Vec<Assignment>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Assignment {
    name: Vec<u8>,
    value: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Value {
    Set,
    Unset,
    Unspecified,
    Text(Vec<u8>),
}

impl Value {
    fn state(&self) -> State<'_> {
        match self {
            Self::Set => State::Set,
            Self::Unset => State::Unset,
            Self::Unspecified => State::Unspecified,
            Self::Text(value) => State::Value(value),
        }
    }
}

impl Attributes {
    /// Creates an empty set with explicit comparison and resource policies.
    pub fn new(case: Case, limits: Limits) -> Self {
        Self {
            case,
            limits,
            sources: Vec::new(),
            macros: HashMap::new(),
            bytes: 0,
            lines: 0,
        }
    }

    /// The comparison policy chosen at construction.
    #[must_use]
    pub fn case(&self) -> Case {
        self.case
    }

    /// The budgets every addition and query is checked against.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// File bytes that can still be added before [`Limits::bytes`] is reached.
    pub(crate) fn remaining_bytes(&self) -> usize {
        self.limits.bytes.saturating_sub(self.bytes)
    }

    /// Compiles one attribute file.
    ///
    /// Lines Git ignores (comments, negative patterns, invalid attribute names and macro
    /// definitions in a `.gitattributes` below the root) are skipped, not reported.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Path`] for an invalid directory and [`Error::Limit`] before retaining input
    /// beyond the budgets. A rejected file leaves the set unchanged.
    pub fn add(&mut self, source: Source<'_>, content: &[u8]) -> Result<(), Error> {
        let (level, base) = match source {
            Source::System => (0, &b""[..]),
            Source::Global => (1, &b""[..]),
            Source::Directory(base) => (2, base),
            Source::Info => (3, &b""[..]),
        };
        validate_path(base, true, self.limits.path_bytes)?;
        let bytes = self
            .bytes
            .saturating_add(content.len())
            .saturating_add(base.len());
        if bytes > self.limits.bytes {
            return Err(Error::Limit("source bytes"));
        }
        if self.sources.len() >= self.limits.sources {
            return Err(Error::Limit("source count"));
        }
        let macros_allowed = !matches!(source, Source::Directory(base) if !base.is_empty());
        let content = content.strip_prefix(b"\xef\xbb\xbf").unwrap_or(content);
        let (mut lines, mut macros) = (Vec::new(), Vec::new());
        for raw in content.split(|b| *b == b'\n') {
            if raw.len() > self.limits.line_bytes {
                return Err(Error::Limit("line bytes"));
            }
            let parsed = parse_line(raw, self.case);
            if parsed.is_some() && self.lines + lines.len() + macros.len() >= self.limits.lines {
                return Err(Error::Limit("line count"));
            }
            match parsed {
                Some(Parsed::Line(line)) => lines.push(line),
                Some(Parsed::Macro(definition)) if macros_allowed => macros.push(definition),
                Some(Parsed::Macro(_)) | None => {}
            }
        }
        let depth = if base.is_empty() {
            0
        } else {
            base.split(|b| *b == b'/').count()
        };
        self.lines += lines.len() + macros.len();
        self.bytes = bytes;
        let index = self.sources.len();
        self.sources.push(Rules {
            base: base.to_vec(),
            rank: (level, depth, index),
            lines,
            macros,
        });
        self.sources.sort_by_key(|rules| rules.rank);
        self.macros = self
            .sources
            .iter()
            .flat_map(|source| &source.macros)
            .map(|definition| (definition.name.clone(), definition.assignments.clone()))
            .collect();
        Ok(())
    }

    /// Returns the state of attribute `name` for the file at `path`.
    ///
    /// `path` is a slash-separated repository-relative file path without a trailing slash.
    /// Patterns are matched as files, never as directories.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Path`] for an invalid path and [`Error::Limit`] when the path or the work
    /// budget is exceeded. Each call receives a fresh work budget.
    pub fn get(&self, path: &[u8], name: &[u8]) -> Result<State<'_>, Error> {
        validate_path(path, false, self.limits.path_bytes)?;
        let mut work = self.limits.work;
        // Each attribute is decided once, by its highest-precedence assignment; a macro expands
        // only where that assignment sets it.
        let mut decided = HashSet::new();
        for source in self.sources.iter().rev() {
            charge(&mut work, source.base.len().saturating_add(1))?;
            let Some(relative) = relative(path, &source.base, self.case) else {
                continue;
            };
            for line in source.lines.iter().rev() {
                charge(&mut work, 1)?;
                let matched = line
                    .pattern
                    .matches(relative, false, &mut work)
                    .map_err(|_| Error::Limit("matching work"))?;
                if matched
                    && let Some(state) =
                        self.decide(&line.assignments, name, &mut decided, &mut work)?
                {
                    return Ok(state);
                }
            }
        }
        Ok(State::Unspecified)
    }

    /// Decides the attributes one matching line assigns, later assignments first, expanding set
    /// macros in place; returns the state of `name` once it is decided here.
    ///
    /// Attributes already in `decided` keep their higher-precedence state. Each macro expands at
    /// most once per query, so cycles and deep chains terminate without a depth limit.
    fn decide<'a>(
        &'a self,
        assignments: &'a [Assignment],
        name: &[u8],
        decided: &mut HashSet<&'a [u8]>,
        work: &mut usize,
    ) -> Result<Option<State<'a>>, Error> {
        let mut pending = vec![assignments.iter().rev()];
        while let Some(top) = pending.last_mut() {
            let Some(assignment) = top.next() else {
                pending.pop();
                continue;
            };
            charge(work, assignment.name.len().saturating_add(1))?;
            if !decided.insert(&assignment.name) {
                continue;
            }
            if assignment.name == name {
                return Ok(Some(assignment.value.state()));
            }
            if assignment.value == Value::Set
                && let Some(expansion) = self.expansion(&assignment.name)
            {
                pending.push(expansion.iter().rev());
            }
        }
        Ok(None)
    }

    /// The definition of macro `name` with the highest precedence, or the built-in `binary`.
    fn expansion(&self, name: &[u8]) -> Option<&[Assignment]> {
        self.macros
            .get(name)
            .map(Vec::as_slice)
            .or_else(|| (name == b"binary").then_some(BINARY.as_slice()))
    }
}

/// The built-in `binary` macro: `-diff -merge -text`.
static BINARY: std::sync::LazyLock<[Assignment; 3]> = std::sync::LazyLock::new(|| {
    [&b"diff"[..], b"merge", b"text"].map(|name| Assignment {
        name: name.to_vec(),
        value: Value::Unset,
    })
});

enum Parsed {
    Line(Line),
    Macro(Macro),
}

fn is_blank(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\x0b' | b'\x0c')
}

/// Parses one physical line; `None` for blank, comment and ignored lines.
fn parse_line(raw: &[u8], case: Case) -> Option<Parsed> {
    let raw = raw.split(|b| *b == 0).next().unwrap_or_default();
    let start = raw.iter().position(|b| !is_blank(*b))?;
    let line = raw.get(start..)?;
    if line.starts_with(b"#") {
        return None;
    }
    if let Some(rest) = line.strip_prefix(b"[attr]") {
        let (name, rest) = token(rest);
        if !valid_name(name) {
            return None;
        }
        return Some(Parsed::Macro(Macro {
            name: name.to_vec(),
            assignments: assignments(rest)?,
        }));
    }
    let (pattern, rest) = pattern(line);
    if pattern.starts_with(b"!") {
        return None;
    }
    Some(Parsed::Line(Line {
        pattern: Pattern::glob(&pattern, case),
        assignments: assignments(rest)?,
    }))
}

/// Splits off a whitespace-delimited token after skipping leading blanks.
fn token(bytes: &[u8]) -> (&[u8], &[u8]) {
    let start = bytes
        .iter()
        .position(|b| !is_blank(*b))
        .unwrap_or(bytes.len());
    let bytes = bytes.get(start..).unwrap_or_default();
    let end = bytes
        .iter()
        .position(|b| is_blank(*b))
        .unwrap_or(bytes.len());
    (
        bytes.get(..end).unwrap_or_default(),
        bytes.get(end..).unwrap_or_default(),
    )
}

/// Splits off the line's pattern, unquoting a C-style quoted one. A quote that does not unquote
/// leaves the token literal, as Git does.
fn pattern(line: &[u8]) -> (Vec<u8>, &[u8]) {
    if line.starts_with(b"\"")
        && let Some(unquoted) = unquote(line)
    {
        return unquoted;
    }
    let (pattern, rest) = token(line);
    (pattern.to_vec(), rest)
}

/// Unquotes a leading C-style string, returning it and the bytes after the closing quote.
fn unquote(line: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    let mut result = Vec::new();
    let mut position = 1;
    loop {
        let byte = *line.get(position)?;
        position += 1;
        match byte {
            b'"' => return Some((result, line.get(position..).unwrap_or_default())),
            b'\\' => {
                let escaped = *line.get(position)?;
                position += 1;
                result.push(match escaped {
                    b'a' => 7,
                    b'b' => 8,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 11,
                    b'f' => 12,
                    b'r' => b'\r',
                    b'\\' | b'"' => escaped,
                    b'0'..=b'3' => {
                        let digits = line.get(position..position + 2)?;
                        if !digits.iter().all(|b| (b'0'..=b'7').contains(b)) {
                            return None;
                        }
                        position += 2;
                        digits
                            .iter()
                            .fold(escaped - b'0', |value, digit| value * 8 + (digit - b'0'))
                    }
                    _ => return None,
                });
            }
            _ => result.push(byte),
        }
    }
}

/// Parses the assignments after a pattern; `None` when any name is invalid, which drops the line.
fn assignments(mut rest: &[u8]) -> Option<Vec<Assignment>> {
    let mut assignments = Vec::new();
    loop {
        let (item, after) = token(rest);
        if item.is_empty() {
            return Some(assignments);
        }
        rest = after;
        let (item, prefix) = match item.first() {
            Some(b'-') => (item.get(1..)?, Some(Value::Unset)),
            Some(b'!') => (item.get(1..)?, Some(Value::Unspecified)),
            _ => (item, None),
        };
        let (name, value) = match crate::bytes::split_once(item, b'=') {
            Some((name, value)) => (name, Value::Text(value.to_vec())),
            None => (item, Value::Set),
        };
        if !valid_name(name) {
            return None;
        }
        assignments.push(Assignment {
            name: name.to_vec(),
            // A value after `-name` or `!name` is ignored.
            value: prefix.unwrap_or(value),
        });
    }
}

/// Git accepts ASCII letters, digits, `-`, `.` and `_`, not starting with `-`.
fn valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && !name.starts_with(b"-")
        && name
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
}

fn charge(work: &mut usize, cost: usize) -> Result<(), Error> {
    *work = work
        .checked_sub(cost)
        .ok_or(Error::Limit("matching work"))?;
    Ok(())
}

pub(super) fn validate_path(path: &[u8], root: bool, limit: usize) -> Result<(), Error> {
    if path.len() > limit {
        return Err(Error::Limit("path bytes"));
    }
    if path.is_empty() && root {
        return Ok(());
    }
    if path.is_empty()
        || path.contains(&0)
        || path
            .split(|b| *b == b'/')
            .any(|p| p.is_empty() || p == b"." || p == b"..")
    {
        return Err(Error::Path);
    }
    Ok(())
}

fn relative<'a>(path: &'a [u8], base: &[u8], case: Case) -> Option<&'a [u8]> {
    if base.is_empty() {
        return Some(path);
    }
    let prefix = path.get(..base.len())?;
    let equal = match case {
        Case::Sensitive => prefix == base,
        Case::AsciiInsensitive => prefix.eq_ignore_ascii_case(base),
    };
    (equal && path.get(base.len()) == Some(&b'/'))
        .then(|| path.get(base.len() + 1..))
        .flatten()
}
