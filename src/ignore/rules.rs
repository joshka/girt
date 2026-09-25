use super::pattern::Pattern;

/// Explicit byte comparison policy, independent of the host filesystem.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Case {
    /// Compare bytes exactly (the policy used by jj's ignore consumer).
    Sensitive,
    /// Fold ASCII letters, including directory-source prefixes. No Unicode folding is performed.
    AsciiInsensitive,
}

/// An ignore source's precedence and repository-relative directory.
///
/// Higher variants override lower variants; deeper directories override shallower ones.
/// Adding another source at the same level appends its rules after existing rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source<'a> {
    /// Caller-resolved `core.excludesFile` content, anchored at the repository root.
    Global,
    /// Common-directory `info/exclude`, anchored at the repository root.
    Info,
    /// `.gitignore` in this slash-separated directory; the empty slice denotes the root.
    Directory(&'a [u8]),
    /// Explicit command rules, anchored at the root and higher than directory files.
    Command,
}

/// Resource budgets for an entire retained set and each independent query.
///
/// Compilation and storage are linear in input bytes with a constant factor for compiled tokens
/// (up to 40 bytes per pattern byte, plus vector/record overhead). Queries use two linear rows,
/// never recursive backtracking. Allocation failure follows Rust's allocator policy; these limits
/// are not a recoverable allocator or an exact RSS bound.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Aggregate source content and directory-prefix bytes; default 8 MiB.
    pub bytes: usize,
    /// Number of added sources, including empty sources; default 4096.
    pub sources: usize,
    /// Retained non-comment, nonempty rules; default 100,000.
    pub patterns: usize,
    /// Maximum line bytes excluding LF and the initial BOM, before trimming; default 4096.
    pub line_bytes: usize,
    /// Maximum query or source-directory path length; default 4096 bytes.
    pub path_bytes: usize,
    /// Aggregate conservative matching work per query; default 100 million units.
    ///
    /// Includes source-prefix comparisons, pattern checks and dynamic-programming cells across
    /// every ancestor. Exhaustion returns an error, never a partial ignore decision.
    pub work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            bytes: 8 * 1024 * 1024,
            sources: 4096,
            patterns: 100_000,
            line_bytes: 4096,
            path_bytes: 4096,
            work: 100_000_000,
        }
    }
}

/// An invalid path or exhausted budget; errors never imply an ignore decision.
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum Error {
    /// Paths must be relative and nonempty, without NUL, empty, `.` or `..` components.
    /// The root directory source alone permits an empty path; backslash is an ordinary byte.
    #[error("invalid repository-relative ignore path")]
    Path,
    /// A named input, retained-storage or matching-work limit was exceeded.
    #[error("ignore {0} limit exceeded")]
    Limit(&'static str),
}

/// The decisive rule, including a possible excluded ancestor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Match {
    /// Whether this rule excludes the path; false identifies an explicit negation.
    pub ignored: bool,
    /// Zero-based successful source-addition index, for caller-owned source labels.
    pub source: usize,
    /// One-based physical source line.
    pub line: usize,
    /// Byte length of the matched path prefix; shorter than the query for an excluded ancestor.
    pub matched_bytes: usize,
}

/// An owned set of compiled Git ignore sources.
///
/// Matching accepts arbitrary non-NUL bytes and does not normalize Unicode, separators or case
/// beyond the chosen [`Case`]. Content recognizes LF, CRLF, an initial UTF-8 BOM, escaped trailing
/// spaces, comments and negation. A NUL truncates its physical line, matching Git's file reader.
/// Invalid glob syntax (including a terminal escape or unclosed class) is retained as a rule that
/// never matches. No source files are opened, and directory flags are supplied by the caller.
#[derive(Debug)]
pub struct Ignore {
    case: Case,
    limits: Limits,
    sources: Vec<Rules>,
    bytes: usize,
    patterns: usize,
}
#[derive(Debug)]
struct Rules {
    base: Vec<u8>,
    rank: (u8, usize, usize),
    patterns: Vec<(usize, Pattern)>,
}
impl Ignore {
    /// Creates an empty set with explicit comparison and resource policies.
    pub fn new(case: Case, limits: Limits) -> Self {
        Self {
            case,
            limits,
            sources: Vec::new(),
            bytes: 0,
            patterns: 0,
        }
    }

    /// Compiles a source and returns its stable insertion index.
    ///
    /// Source order need not follow hierarchy; precedence is determined by [`Source`]. All
    /// previously added sources remain unchanged on error, including their provenance indices.
    /// Duplicate directory additions are concatenated in addition order, not replacements.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Path`] for invalid directory prefixes and [`Error::Limit`] before retaining
    /// input beyond the configured budgets. A rejected addition leaves the set unchanged.
    pub fn add(&mut self, source: Source<'_>, content: &[u8]) -> Result<usize, Error> {
        let (level, base) = match source {
            Source::Global => (0, &b""[..]),
            Source::Info => (1, &b""[..]),
            Source::Directory(base) => (2, base),
            Source::Command => (3, &b""[..]),
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
        let content = content.strip_prefix(b"\xef\xbb\xbf").unwrap_or(content);
        let mut patterns = Vec::new();
        for (line, raw) in content.split(|b| *b == b'\n').enumerate() {
            if raw.len() > self.limits.line_bytes {
                return Err(Error::Limit("line bytes"));
            }
            if let Some(pattern) = Pattern::parse(raw, self.case) {
                if self.patterns.saturating_add(patterns.len()) >= self.limits.patterns {
                    return Err(Error::Limit("pattern count"));
                }
                patterns.push((line + 1, pattern));
            }
        }
        let index = self.sources.len();
        let depth = if base.is_empty() {
            0
        } else {
            base.split(|b| *b == b'/').count()
        };
        self.patterns += patterns.len();
        self.bytes = bytes;
        self.sources.push(Rules {
            base: base.to_vec(),
            rank: (level, depth, index),
            patterns,
        });
        self.sources.sort_by_key(|s| s.rank);
        Ok(index)
    }

    /// Resolves a file or directory, including all parent exclusions.
    ///
    /// Supply a nonempty repository-relative path without a trailing slash. Intermediate
    /// components are treated as directories. A symlink itself is not a directory; callers must
    /// not traverse through symlinks. `None` means no rule matched the final path. A negation
    /// returns `Some` with `ignored == false`; an excluded ancestor returns its own provenance.
    /// This method does not consult the index: apply tracked-file policy before using its result.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Path`] for invalid paths and [`Error::Limit`] for path/work exhaustion.
    /// There are no side effects or partial decisions. Each call receives a fresh work budget.
    pub fn check(&self, path: &[u8], is_directory: bool) -> Result<Option<Match>, Error> {
        validate_path(path, false, self.limits.path_bytes)?;
        let mut work = self.limits.work;
        for end in path
            .iter()
            .enumerate()
            .filter_map(|(i, b)| (*b == b'/').then_some(i))
        {
            let found = self.direct(&path[..end], true, &mut work)?;
            if found.is_some_and(|m| m.ignored) {
                return Ok(found);
            }
        }
        self.direct(path, is_directory, &mut work)
    }

    fn direct(
        &self,
        path: &[u8],
        directory: bool,
        work: &mut usize,
    ) -> Result<Option<Match>, Error> {
        for source in self.sources.iter().rev() {
            charge(work, source.base.len().saturating_add(1))?;
            let Some(relative) = relative(path, &source.base, self.case) else {
                continue;
            };
            for (line, pattern) in source.patterns.iter().rev() {
                charge(work, 1)?;
                if pattern.matches(relative, directory, work)? {
                    return Ok(Some(Match {
                        ignored: !pattern.negative,
                        source: source.rank.2,
                        line: *line,
                        matched_bytes: path.len(),
                    }));
                }
            }
        }
        Ok(None)
    }
}

pub(super) fn charge(work: &mut usize, cost: usize) -> Result<(), Error> {
    *work = work
        .checked_sub(cost)
        .ok_or(Error::Limit("matching work"))?;
    Ok(())
}
fn validate_path(path: &[u8], root: bool, limit: usize) -> Result<(), Error> {
    if path.len() > limit {
        return Err(Error::Limit("path bytes"));
    }
    if path.is_empty() && root {
        return Ok(());
    }
    if path.contains(&0)
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
    (equal && path.get(base.len()) == Some(&b'/')).then(|| &path[base.len() + 1..])
}
