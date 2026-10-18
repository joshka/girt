use std::ops::Range;

use regex_automata::nfa::thompson::{self, pikevm};
use regex_automata::util::syntax;
use regex_automata::{Anchored, Input, MatchKind, meta};

use crate::posix_regex::{Flavor, literal, translate};

/// Compiled expressions above this size are refused, bounding memory for hostile patterns.
const NFA_SIZE_LIMIT: usize = 8 * 1024 * 1024;

/// How the text of each pattern is read, as `git grep -F`, `-G` and `-E` choose.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Syntax {
    /// Every byte stands for itself (`-F`).
    Fixed,
    /// POSIX basic regular expressions, Git's default (`-G`). As in GNU, `\+`, `\?` and `\|` are
    /// operators.
    #[default]
    Basic,
    /// POSIX extended regular expressions (`-E`).
    Extended,
}

/// A boolean combination of patterns, as `-e`, `--and`, `--or`, `--not` and parentheses build it.
///
/// Adjacent `-e` patterns on Git's command line are joined with [`Expression::Or`].
///
/// ```
/// use girt::grep::Expression;
///
/// // git grep -e foo --and --not -e bar
/// let expression = Expression::pattern("foo").and(!Expression::pattern("bar"));
/// # let _ = expression;
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Expression {
    /// A line matches when the pattern matches somewhere in it. A pattern containing LF stands
    /// for its lines joined with [`Expression::Or`], as in Git.
    Pattern(Vec<u8>),
    /// A line matches when the inner expression does not.
    Not(Box<Expression>),
    /// A line matches when both sides do.
    And(Box<Expression>, Box<Expression>),
    /// A line matches when either side does.
    Or(Box<Expression>, Box<Expression>),
}

impl Expression {
    /// A single pattern.
    pub fn pattern(text: impl Into<Vec<u8>>) -> Self {
        Self::Pattern(text.into())
    }

    /// Lines both expressions match (`--and`).
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        Self::And(Box::new(self), Box::new(other))
    }

    /// Lines either expression matches (`--or`, or two adjacent patterns).
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        Self::Or(Box::new(self), Box::new(other))
    }
}

/// Lines this expression does not match (`--not`).
impl std::ops::Not for Expression {
    type Output = Self;

    fn not(self) -> Self {
        Self::Not(Box::new(self))
    }
}

/// How every pattern of a [`Query`] matches: syntax, case and whole-word matching.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Matching {
    syntax: Syntax,
    ignore_case: bool,
    word: bool,
    bytes: bool,
}

impl Matching {
    /// Basic POSIX syntax, case-sensitive, matching anywhere: `git grep` without options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads patterns with `syntax`.
    #[must_use]
    pub fn with_syntax(mut self, syntax: Syntax) -> Self {
        self.syntax = syntax;
        self
    }

    /// Ignores case (`-i`).
    #[must_use]
    pub fn with_ignore_case(mut self, ignore_case: bool) -> Self {
        self.ignore_case = ignore_case;
        self
    }

    /// Matches byte by byte, as Git does in the C locale, instead of by UTF-8 character, as in a
    /// UTF-8 locale (the default). With bytes, `.` and bracket expressions match any single byte
    /// and case folding is ASCII.
    #[must_use]
    pub fn with_bytes(mut self, bytes: bool) -> Self {
        self.bytes = bytes;
        self
    }

    /// Accepts only matches with a non-word byte or the line edge on each side (`-w`).
    ///
    /// Word bytes are ASCII letters, digits and `_`. As in Git, when the leftmost-longest match
    /// at a position fails this test, the search resumes one byte after that match's start;
    /// shorter matches at the same start are not tried.
    #[must_use]
    pub fn with_word(mut self, word: bool) -> Self {
        self.word = word;
        self
    }
}

/// A pattern could not be compiled.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PatternError {
    /// The pattern is not UTF-8. Git accepts such bytes; girt does not compile them.
    #[error("grep pattern is not UTF-8")]
    Encoding,
    /// The pattern uses a POSIX construct girt does not translate, such as a back-reference.
    /// Git accepts it.
    #[error("unsupported grep pattern construct: {0}")]
    Unsupported(&'static str),
    /// The expression is malformed or exceeds girt's compiled size bound.
    #[error("invalid grep pattern")]
    Invalid(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Compiled patterns and the expression that combines them; decides which lines match.
///
/// Patterns are POSIX expressions translated to the dialect girt's regex engine compiles, with
/// the same rules and refusals as
/// [`FunctionLines`](crate::patch::FunctionLines): lines match as in a UTF-8 locale unless
/// [`Matching::with_bytes`] selects the C locale's byte matching, named classes such as
/// `[:alpha:]` are ASCII, and back-references, collating elements and
/// equivalence classes are refused. Reported match positions are POSIX leftmost-longest.
///
/// ```
/// use girt::grep::{Expression, Matching, Query, Syntax};
///
/// let query = Query::new(
///     &Expression::pattern("ab+c"),
///     Matching::new().with_syntax(Syntax::Extended),
/// )?;
/// assert!(query.is_match(b"xabbc"));
/// assert_eq!(query.matches(b"abc abbc"), [0..3, 4..8]);
/// assert!(!query.is_match(b"ac"));
/// # Ok::<(), girt::grep::PatternError>(())
/// ```
#[derive(Debug)]
pub struct Query {
    node: Node,
    leaves: Vec<Leaf>,
    word: bool,
    all_match: bool,
}

#[derive(Debug)]
pub(super) enum Node {
    Leaf(usize),
    Not(Box<Node>),
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
}

#[derive(Debug)]
struct Leaf {
    /// Finds the leftmost match start quickly.
    find: meta::Regex,
    /// Extends a match from that start to the longest one.
    longest: pikevm::PikeVM,
    /// Whether the leaf sits under a `--not`; such leaves select lines but are not shown by
    /// `-o`.
    negated: bool,
}

impl Query {
    /// Compiles every pattern of `expression` with `matching`.
    ///
    /// # Errors
    ///
    /// Returns [`PatternError`] for a non-UTF-8 pattern, an unsupported construct or a
    /// malformed expression.
    pub fn new(expression: &Expression, matching: Matching) -> Result<Self, PatternError> {
        let mut leaves = Vec::new();
        let node = compile(expression, matching, false, &mut leaves)?;
        Ok(Self {
            node,
            leaves,
            word: matching.word,
            all_match: false,
        })
    }

    /// Selects a file only when each pattern joined by a top-level [`Expression::Or`] matches
    /// at least one of its lines (`--all-match`). Line selection is unchanged.
    #[must_use]
    pub fn with_all_match(mut self, all_match: bool) -> Self {
        self.all_match = all_match;
        self
    }

    /// Returns whether `line` (without its line terminator) matches.
    #[must_use]
    pub fn is_match(&self, line: &[u8]) -> bool {
        self.eval(&self.node, line)
    }

    /// The positions `-o` and `--column` report in `line`: from each position, the earliest
    /// match of any pattern not under a `--not`, the longest on a tie, then the search continues
    /// after it. An empty match ends the list, as in Git.
    #[must_use]
    pub fn matches(&self, line: &[u8]) -> Vec<Range<usize>> {
        let mut found = Vec::new();
        let mut caches: Vec<_> = self.leaves.iter().map(|_| None).collect();
        let mut at = 0;
        while at <= line.len() {
            let best = self
                .leaves
                .iter()
                .zip(&mut caches)
                .filter(|(leaf, _)| !leaf.negated)
                .filter_map(|(leaf, cache)| self.find(leaf, cache, line, at))
                .min_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
            let Some(span) = best.filter(|span| !span.is_empty()) else {
                break;
            };
            at = span.end;
            found.push(span);
        }
        found
    }

    /// The branches of a top-level [`Expression::Or`] chain, for `--all-match`.
    pub(super) fn branches(&self) -> Vec<&Node> {
        fn collect<'a>(node: &'a Node, out: &mut Vec<&'a Node>) {
            match node {
                Node::Or(left, right) => {
                    collect(left, out);
                    collect(right, out);
                }
                other => out.push(other),
            }
        }
        let mut out = Vec::new();
        collect(&self.node, &mut out);
        out
    }

    pub(super) fn all_match(&self) -> bool {
        self.all_match
    }

    pub(super) fn eval(&self, node: &Node, line: &[u8]) -> bool {
        match node {
            Node::Leaf(index) => self
                .leaves
                .get(*index)
                .and_then(|leaf| self.find(leaf, &mut None, line, 0))
                .is_some(),
            Node::Not(inner) => !self.eval(inner, line),
            Node::And(left, right) => self.eval(left, line) && self.eval(right, line),
            Node::Or(left, right) => self.eval(left, line) || self.eval(right, line),
        }
    }

    /// When the expression is a plain `--or` of patterns, the offset of the earliest position in
    /// `haystack` at or after `from` where a pattern may match; lines before it cannot match.
    /// `None` means no line at or after `from` matches. Returns `Some(from)` when the expression
    /// cannot be searched this way.
    pub(super) fn skip_to(&self, haystack: &[u8], from: usize) -> Option<usize> {
        if has_and(&self.node) {
            return Some(from);
        }
        self.leaves
            .iter()
            .filter_map(|leaf| {
                leaf.find
                    .search(&Input::new(haystack).range(from..))
                    .map(|found| found.start())
            })
            .min()
    }

    /// The leftmost-longest match of `leaf` in `line` starting at or after `at`, honoring `-w`.
    fn find(
        &self,
        leaf: &Leaf,
        cache: &mut Option<pikevm::Cache>,
        line: &[u8],
        mut at: usize,
    ) -> Option<Range<usize>> {
        loop {
            let found = leaf.find.search(&Input::new(line).range(at..))?;
            let start = found.start();
            let cache = cache.get_or_insert_with(|| leaf.longest.create_cache());
            let input = Input::new(line).range(start..).anchored(Anchored::Yes);
            let end = leaf
                .longest
                .find(cache, input)
                .map_or(found.end(), |longest| longest.end().max(found.end()));
            if !self.word || is_word_bounded(line, start, end) {
                return Some(start..end);
            }
            at = start + 1;
            if at > line.len() {
                return None;
            }
        }
    }
}

fn has_and(node: &Node) -> bool {
    match node {
        Node::Leaf(_) => false,
        Node::Not(_) | Node::And(..) => true,
        Node::Or(left, right) => has_and(left) || has_and(right),
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_word_bounded(line: &[u8], start: usize, end: usize) -> bool {
    let before = start
        .checked_sub(1)
        .and_then(|index| line.get(index))
        .is_some_and(|&byte| is_word_byte(byte));
    let after = line.get(end).is_some_and(|&byte| is_word_byte(byte));
    !before && !after
}

fn compile(
    expression: &Expression,
    matching: Matching,
    negated: bool,
    leaves: &mut Vec<Leaf>,
) -> Result<Node, PatternError> {
    Ok(match expression {
        Expression::Pattern(text) => {
            // As in Git, each line of a pattern is a pattern of its own, joined with --or.
            let mut node = None;
            for line in text.split(|&byte| byte == b'\n') {
                leaves.push(leaf(line, matching, negated)?);
                let next = Node::Leaf(leaves.len() - 1);
                node = Some(match node {
                    None => next,
                    Some(previous) => Node::Or(Box::new(previous), Box::new(next)),
                });
            }
            node.unwrap_or(Node::Leaf(0))
        }
        Expression::Not(inner) => Node::Not(Box::new(compile(inner, matching, !negated, leaves)?)),
        Expression::And(left, right) => Node::And(
            Box::new(compile(left, matching, negated, leaves)?),
            Box::new(compile(right, matching, negated, leaves)?),
        ),
        Expression::Or(left, right) => Node::Or(
            Box::new(compile(left, matching, negated, leaves)?),
            Box::new(compile(right, matching, negated, leaves)?),
        ),
    })
}

fn leaf(text: &[u8], matching: Matching, negated: bool) -> Result<Leaf, PatternError> {
    let text = std::str::from_utf8(text).map_err(|_| PatternError::Encoding)?;
    let expression = match matching.syntax {
        Syntax::Fixed => {
            let mut out = String::with_capacity(text.len());
            text.chars().for_each(|c| literal(c, &mut out));
            out
        }
        Syntax::Basic => translate(text, Flavor::Basic).map_err(PatternError::Unsupported)?,
        Syntax::Extended => translate(text, Flavor::Extended).map_err(PatternError::Unsupported)?,
    };
    // Multi-line anchors let one search over a whole file find candidate lines; each candidate
    // is then checked against its own line.
    let syntax = syntax::Config::new()
        .case_insensitive(matching.ignore_case)
        .multi_line(true)
        .unicode(!matching.bytes)
        .utf8(!matching.bytes);
    let invalid = |error: Box<dyn std::error::Error + Send + Sync>| PatternError::Invalid(error);
    let find = meta::Regex::builder()
        .syntax(syntax)
        .configure(meta::Config::new().nfa_size_limit(Some(NFA_SIZE_LIMIT)))
        .build(&expression)
        .map_err(|error| invalid(Box::new(error)))?;
    let longest = pikevm::PikeVM::builder()
        .syntax(syntax)
        .thompson(thompson::Config::new().nfa_size_limit(Some(NFA_SIZE_LIMIT)))
        .configure(pikevm::Config::new().match_kind(MatchKind::All))
        .build(&expression)
        .map_err(|error| invalid(Box::new(error)))?;
    Ok(Leaf {
        find,
        longest,
        negated,
    })
}
