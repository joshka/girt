use std::ops::Range;

use super::{Error, Query};
use crate::patch::{DiffAttributes, FunctionLines};

/// Git reads this many leading bytes to decide whether content without a `diff` attribute is
/// binary: it is when they contain NUL.
const BINARY_PROBE: usize = 8000;

/// What to do with files classified as binary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Binary {
    /// Report that a binary file matches without showing lines, as Git does by default.
    #[default]
    Report,
    /// Search and show binary files as text (`-a`).
    Text,
    /// Skip binary files (`-I`).
    Skip,
}

/// Which lines a search selects and which surrounding lines it shows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Options {
    invert: bool,
    before: usize,
    after: usize,
    function_name: bool,
    function_body: bool,
    max_count: Option<usize>,
    binary: Binary,
}

impl Options {
    /// Selects matching lines with no context: `git grep` without options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Selects the lines that do not match instead (`-v`).
    #[must_use]
    pub fn with_invert(mut self, invert: bool) -> Self {
        self.invert = invert;
        self
    }

    /// Shows this many lines before each selected line (`-B`).
    #[must_use]
    pub fn with_before(mut self, lines: usize) -> Self {
        self.before = lines;
        self
    }

    /// Shows this many lines after each selected line (`-A`).
    #[must_use]
    pub fn with_after(mut self, lines: usize) -> Self {
        self.after = lines;
        self
    }

    /// Shows the nearest function line before each selected line unless it is already shown
    /// (`-p`).
    ///
    /// A path's `diff` driver chooses function lines through `diff.<driver>.xfuncname` or
    /// `diff.<driver>.funcname`; otherwise a function line starts with an ASCII letter, `_` or
    /// `$`, as for diff hunk headers.
    #[must_use]
    pub fn with_function_name(mut self, function_name: bool) -> Self {
        self.function_name = function_name;
        self
    }

    /// Shows the whole function around each selected line (`-W`): from its function line, and
    /// the non-blank lines directly above it, to the line before the next function line, less
    /// trailing blank lines.
    #[must_use]
    pub fn with_function_body(mut self, function_body: bool) -> Self {
        self.function_body = function_body;
        self
    }

    /// Stops selecting lines in a file after this many (`-m`); context after the last is still
    /// shown.
    #[must_use]
    pub fn with_max_count(mut self, max_count: Option<usize>) -> Self {
        self.max_count = max_count;
        self
    }

    /// Chooses how binary files are handled.
    #[must_use]
    pub fn with_binary(mut self, binary: Binary) -> Self {
        self.binary = binary;
        self
    }

    /// Whether lines that do not match are selected.
    pub fn invert(&self) -> bool {
        self.invert
    }

    /// Whether context or whole functions are shown, so groups of lines are separated by `--`.
    pub(super) fn has_context(&self) -> bool {
        self.before > 0 || self.after > 0 || self.function_body
    }
}

/// Why a line is shown.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum LineKind {
    /// Context around a selected line (`-` in Git's output).
    Context,
    /// A function line shown by `-p` or `-W` (`=` in Git's output).
    Function,
    /// A selected line (`:` in Git's output).
    Selected,
}

/// One shown line of a file.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Line {
    /// The 1-based line number.
    pub number: usize,
    /// Why the line is shown.
    pub kind: LineKind,
    /// The line's byte range in the file content, without its LF.
    pub range: Range<usize>,
}

/// The result of searching one file.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct FileMatches {
    /// Whether the file was classified as binary; its lines are then not listed.
    pub binary: bool,
    /// How many lines were selected, after `-m`.
    pub count: usize,
    /// The lines to show in order, empty for a binary file.
    pub lines: Vec<Line>,
}

impl FileMatches {
    /// Whether any line was selected.
    #[must_use]
    pub fn is_match(&self) -> bool {
        self.count > 0
    }
}

/// Searches file contents with a [`Query`] and [`Options`].
///
/// [`Self::with_attributes`] adds a repository's diff attributes, which decide binary handling
/// and function lines for each path; without them, NUL detection and Git's default function rule
/// apply.
///
/// ```
/// use girt::grep::{Expression, LineKind, Matching, Options, Query, Searcher};
///
/// let query = Query::new(&Expression::pattern("needle"), Matching::new())?;
/// let mut searcher = Searcher::new(query, Options::new().with_before(1));
/// let found = searcher.search(b"notes.txt", b"hay\nneedle\nhay\n")?;
/// assert_eq!(found.count, 1);
/// let kinds: Vec<_> = found
///     .lines
///     .iter()
///     .map(|line| (line.number, line.kind))
///     .collect();
/// assert_eq!(kinds, [(1, LineKind::Context), (2, LineKind::Selected)]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Searcher {
    query: Query,
    options: Options,
    attributes: Option<DiffAttributes>,
}

impl Searcher {
    /// Searches with `query` and `options`, without attributes.
    pub fn new(query: Query, options: Options) -> Self {
        Self {
            query,
            options,
            attributes: None,
        }
    }

    /// Uses `attributes` (from [`Repository::diff_attributes`](crate::Repository::diff_attributes))
    /// to classify binary files and find function lines.
    #[must_use]
    pub fn with_attributes(mut self, attributes: DiffAttributes) -> Self {
        self.attributes = Some(attributes);
        self
    }

    /// The query lines are matched with.
    pub fn query(&self) -> &Query {
        &self.query
    }

    /// The options in effect.
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// Searches `content`, the bytes of the file at repository-relative `path`; an empty `path`
    /// reads no attributes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Attributes`] when the path's attributes cannot be read or its driver's
    /// function pattern does not compile.
    pub fn search(&mut self, path: &[u8], content: &[u8]) -> Result<FileMatches, Error> {
        let (binary, function) = match &mut self.attributes {
            Some(attributes) if !path.is_empty() => {
                attributes
                    .grep_driver(path)
                    .map_err(|source| Error::Attributes {
                        path: path.to_vec(),
                        source: Box::new(source),
                    })?
            }
            _ => (None, None),
        };
        let binary =
            binary.unwrap_or_else(|| content.get(..BINARY_PROBE).unwrap_or(content).contains(&0));
        let binary = match self.options.binary {
            Binary::Text => false,
            Binary::Skip if binary => return Ok(FileMatches::default()),
            Binary::Report | Binary::Skip => binary,
        };
        let lines = split_lines(content);
        let selected = self.select(content, &lines);
        let count = selected.len();
        if binary || count == 0 {
            return Ok(FileMatches {
                binary,
                count,
                lines: Vec::new(),
            });
        }
        let shown = self.show(content, &lines, &selected, function.unwrap_or_default());
        Ok(FileMatches {
            binary,
            count,
            lines: shown,
        })
    }

    /// The indexes of selected lines, honoring `-v`, `-m` and `--all-match`.
    fn select(&self, content: &[u8], lines: &[Range<usize>]) -> Vec<usize> {
        let query = &self.query;
        let options = &self.options;
        let branches = query.all_match().then(|| query.branches());
        let mut hit = vec![false; branches.as_ref().map_or(0, Vec::len)];
        let mut selected = Vec::new();
        let limit = options.max_count.unwrap_or(usize::MAX);
        // Without -v or --all-match, whole-content searches skip lines that cannot match.
        let skip = !options.invert && branches.is_none();
        let mut index = 0;
        while index < lines.len() {
            let full = selected.len() >= limit;
            // Past `-m`, lines are only read to learn whether every --all-match branch matches.
            if full && !hit.contains(&false) {
                break;
            }
            if skip {
                let Some(start) = lines
                    .get(index)
                    .and_then(|line| query.skip_to(content, line.start))
                else {
                    break;
                };
                index += lines
                    .get(index..)
                    .unwrap_or_default()
                    .partition_point(|line| line.end < start);
            }
            let Some(range) = lines.get(index) else { break };
            let line = content.get(range.clone()).unwrap_or_default();
            let matched = match &branches {
                Some(branches) => {
                    let mut any = false;
                    for (branch, hit) in branches.iter().zip(&mut hit) {
                        if query.eval(branch, line) {
                            *hit = true;
                            any = true;
                        }
                    }
                    any
                }
                None => query.is_match(line),
            };
            if matched != options.invert && !full {
                selected.push(index);
            }
            index += 1;
        }
        if hit.contains(&false) {
            selected.clear();
        }
        selected
    }

    /// The lines to show around `selected`, in order.
    fn show(
        &self,
        content: &[u8],
        lines: &[Range<usize>],
        selected: &[usize],
        function: FunctionLines,
    ) -> Vec<Line> {
        let options = &self.options;
        let mut matcher = function.matcher();
        let text = |index: usize| {
            lines
                .get(index)
                .and_then(|range| content.get(range.clone()))
                .unwrap_or_default()
        };
        let mut is_function = |index: usize| matcher.name(text(index)).is_some();
        let is_blank = |index: usize| text(index).iter().all(u8::is_ascii_whitespace);
        let mut kinds: Vec<Option<LineKind>> = vec![None; lines.len()];
        let mark = |kinds: &mut Vec<Option<LineKind>>, index: usize, kind: LineKind| {
            if let Some(slot) = kinds.get_mut(index) {
                *slot = (*slot).max(Some(kind));
            }
        };
        // The last line marked so far, which Git has already printed when it reaches a match.
        let mut last: Option<usize> = None;
        for &index in selected {
            if options.function_name
                && let Some(function) = (0..index).rev().find(|&line| is_function(line))
                && last.is_none_or(|last| function > last)
            {
                mark(&mut kinds, function, LineKind::Function);
            }
            let mut first = index.saturating_sub(options.before);
            let mut end = index.saturating_add(options.after);
            if options.function_body {
                match (0..=index).rev().find(|&line| is_function(line)) {
                    Some(function) => {
                        mark(&mut kinds, function, LineKind::Function);
                        let mut start = function;
                        while start > 0 && !is_blank(start - 1) && !is_function(start - 1) {
                            start -= 1;
                        }
                        first = first.min(start);
                    }
                    None => first = 0,
                }
                let mut stop = (index + 1..lines.len())
                    .find(|&line| is_function(line))
                    .unwrap_or(lines.len());
                while stop > index + 1 && is_blank(stop - 1) {
                    stop -= 1;
                }
                end = end.max(stop - 1);
            }
            let end = end.min(lines.len().saturating_sub(1));
            for line in first..index {
                mark(&mut kinds, line, LineKind::Context);
            }
            mark(&mut kinds, index, LineKind::Selected);
            for line in index + 1..=end {
                mark(&mut kinds, line, LineKind::Context);
            }
            last = last.max(Some(end));
        }
        kinds
            .into_iter()
            .zip(lines)
            .enumerate()
            .filter_map(|(index, (kind, range))| {
                kind.map(|kind| Line {
                    number: index + 1,
                    kind,
                    range: range.clone(),
                })
            })
            .collect()
    }
}

/// The byte ranges of `content`'s lines without their LF; a final line without LF counts.
fn split_lines(content: &[u8]) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, &byte) in content.iter().enumerate() {
        if byte == b'\n' {
            lines.push(start..index);
            start = index + 1;
        }
    }
    if start < content.len() {
        lines.push(start..content.len());
    }
    lines
}
