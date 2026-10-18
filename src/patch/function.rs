use std::ops::Range;
use std::sync::Arc;

use regex_automata::nfa::thompson::{self, pikevm};
use regex_automata::util::captures::Captures;
use regex_automata::util::syntax;
use regex_automata::{Anchored, Input, MatchKind, meta};

use crate::posix_regex::{Flavor, translate};

/// Compiled regular expressions above this size are refused, bounding memory for hostile config.
const NFA_SIZE_LIMIT: usize = 4 * 1024 * 1024;

/// How a hunk header finds the function line before each hunk.
///
/// [`Self::default`] is Git's rule for files without a diff driver pattern: the nearest earlier
/// line starting with an ASCII letter, `_` or `$`. [`Self::extended`] and [`Self::basic`] compile
/// a `diff.<driver>.xfuncname` or `diff.<driver>.funcname` value, and [`Self::builtin`] supplies
/// the pattern for a driver Git defines itself.
///
/// A pattern value holds one regular expression per line. For each earlier line, the expressions
/// are tried in order and the first that matches decides: a plain expression makes the line a
/// function line and an expression prefixed with `!` rejects it. The header shows the first
/// capture group when it participates, otherwise the whole match, cut to 80 bytes and stripped of
/// trailing whitespace. Matching is POSIX leftmost-longest for the whole match; where POSIX and
/// preference order disagree about a group inside it, the group follows preference order.
///
/// Lines are matched as in a UTF-8 locale: `.` and bracket expressions match whole UTF-8
/// characters, so a line that is not UTF-8 matches only through its valid parts. Named classes
/// such as `[:alpha:]` are ASCII. Back-references, collating elements and equivalence classes
/// are refused ([issue 461](https://github.com/joshka/girt/issues/461)).
///
/// ```
/// use girt::patch::FunctionLines;
///
/// // A pattern that finds `sub` definitions, and one that skips `sub` lines marked private.
/// let perl = FunctionLines::extended(b"^sub (.*)")?;
/// let public = FunctionLines::extended(b"!^sub _\n^sub (.*)")?;
/// assert!(FunctionLines::extended(b"^a\n!^b").is_err()); // The last expression must be positive.
/// assert!(FunctionLines::builtin(b"rust").is_some());
/// # let _ = (perl, public);
/// # Ok::<(), girt::patch::PatternError>(())
/// ```
#[derive(Clone, Debug, Default)]
pub struct FunctionLines(Kind);

#[derive(Clone, Debug, Default)]
enum Kind {
    #[default]
    Default,
    Patterns(Arc<[Rule]>),
}

#[derive(Debug)]
struct Rule {
    negated: bool,
    /// Finds the leftmost match start quickly.
    find: meta::Regex,
    /// Extends a match from that start to the longest one and reports its groups.
    longest: pikevm::PikeVM,
}

/// A function-name pattern could not be compiled.
///
/// Git refuses the same patterns when it compiles them, except where noted on the variant.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PatternError {
    /// The last expression is negated, so no line could ever be a function line.
    #[error("the last function pattern expression is negated")]
    NegatedLast,
    /// An expression is empty, such as after a trailing newline.
    #[error("function pattern has an empty expression")]
    Empty,
    /// The pattern is not UTF-8. Git accepts such bytes; girt does not compile them.
    #[error("function pattern is not UTF-8")]
    Encoding,
    /// The pattern uses a construct girt does not translate. Git may accept it.
    #[error("unsupported function pattern construct: {0}")]
    Unsupported(&'static str),
    /// The expression is malformed or exceeds girt's compiled size bound.
    #[error("invalid function pattern")]
    Invalid(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// An original pattern for Git's built-in `rust` driver, written from the Rust grammar and checked
/// against the lines `git diff` reports for it: optional indentation, `pub` with an optional
/// restriction, at most one of `async`, `const`, `unsafe` or `extern "abi"`, an item keyword,
/// then a line with no `;`.
const RUST: &str = concat!(
    r#"^[\t ]*((pub(\([^)]+\))?[\t ]+)?((async|const|unsafe|extern[\t ]+"[^"]+")[\t ]+)?"#,
    r"(struct|enum|union|mod|trait|fn|impl|macro_rules!)[<\t ]+[^;]*)$",
);

impl FunctionLines {
    /// Compiles a `diff.<driver>.xfuncname` value: POSIX extended expressions, one per line.
    ///
    /// # Errors
    ///
    /// Returns [`PatternError`] for a negated last expression, non-UTF-8 bytes, an unsupported
    /// construct or a malformed expression.
    pub fn extended(patterns: &[u8]) -> Result<Self, PatternError> {
        Self::compile(patterns, Flavor::Extended)
    }

    /// Compiles a `diff.<driver>.funcname` value: POSIX basic expressions, one per line. As in
    /// GNU regular expressions, `\+`, `\?` and `\|` are operators.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::extended`].
    pub fn basic(patterns: &[u8]) -> Result<Self, PatternError> {
        Self::compile(patterns, Flavor::Basic)
    }

    /// Returns the pattern of a diff driver Git defines without configuration, if girt has one.
    ///
    /// girt defines `rust`. Git defines further drivers (such as `python` and `cpp`) whose
    /// patterns girt does not have; for those, this returns `None` and headers use
    /// [`Self::default`] ([issue 460](https://github.com/joshka/girt/issues/460)).
    #[must_use]
    pub fn builtin(driver: &[u8]) -> Option<Self> {
        match driver {
            b"rust" => rule(RUST, false)
                .ok()
                .map(|rule| Self(Kind::Patterns(Arc::from([rule])))),
            _ => None,
        }
    }

    fn compile(patterns: &[u8], flavor: Flavor) -> Result<Self, PatternError> {
        let patterns = std::str::from_utf8(patterns).map_err(|_| PatternError::Encoding)?;
        let mut rules = Vec::new();
        for line in patterns.split('\n') {
            let (negated, expression) = match line.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, line),
            };
            // Git refuses an empty expression, including one after a trailing newline.
            if expression.is_empty() {
                return Err(PatternError::Empty);
            }
            let expression = translate(expression, flavor).map_err(PatternError::Unsupported)?;
            rules.push(rule(&expression, negated)?);
        }
        if rules.last().is_some_and(|rule| rule.negated) {
            return Err(PatternError::NegatedLast);
        }
        Ok(Self(Kind::Patterns(rules.into())))
    }

    pub(crate) fn matcher(&self) -> Matcher<'_> {
        Matcher {
            lines: self,
            caches: Vec::new(),
        }
    }
}

fn rule(expression: &str, negated: bool) -> Result<Rule, PatternError> {
    let invalid = |error: Box<dyn std::error::Error + Send + Sync>| PatternError::Invalid(error);
    let find = meta::Regex::builder()
        .syntax(syntax::Config::new())
        .configure(meta::Config::new().nfa_size_limit(Some(NFA_SIZE_LIMIT)))
        .build(expression)
        .map_err(|error| invalid(Box::new(error)))?;
    let longest = pikevm::PikeVM::builder()
        .syntax(syntax::Config::new())
        .thompson(thompson::Config::new().nfa_size_limit(Some(NFA_SIZE_LIMIT)))
        .configure(pikevm::Config::new().match_kind(MatchKind::All))
        .build(expression)
        .map_err(|error| invalid(Box::new(error)))?;
    Ok(Rule {
        negated,
        find,
        longest,
    })
}

/// Matching state for one comparison, reusing search caches across lines.
pub(crate) struct Matcher<'a> {
    lines: &'a FunctionLines,
    caches: Vec<(pikevm::Cache, Captures)>,
}

impl Matcher<'_> {
    /// The range of `line` (without its LF) a header shows, or `None` when it is not a function
    /// line.
    pub(crate) fn name(&mut self, line: &[u8]) -> Option<Range<usize>> {
        let span = match &self.lines.0 {
            Kind::Default => matches!(line.first(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$'))
                .then_some(0..line.len()),
            Kind::Patterns(rules) => {
                if self.caches.is_empty() {
                    self.caches = rules
                        .iter()
                        .map(|rule| (rule.longest.create_cache(), rule.longest.create_captures()))
                        .collect();
                }
                rules
                    .iter()
                    .zip(&mut self.caches)
                    .find_map(|(rule, (cache, captures))| {
                        let found = rule.find.search(&Input::new(line))?;
                        let input = Input::new(line)
                            .range(found.start()..)
                            .anchored(Anchored::Yes);
                        rule.longest.search(cache, &input, captures);
                        let span = captures.get_group(1).or_else(|| captures.get_group(0))?;
                        Some((!rule.negated).then(|| span.range()))
                    })
                    .flatten()
            }
        }?;
        let end = span.end.min(span.start + 80);
        let kept = line.get(span.start..end)?.trim_ascii_end().len();
        Some(span.start..span.start + kept)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::FunctionLines;

    fn name(lines: &FunctionLines, line: &str) -> Option<String> {
        let range = lines.matcher().name(line.as_bytes())?;
        Some(line[range].to_owned())
    }

    /// Each case was checked against `git diff` with the same `xfuncname`.
    #[rstest]
    #[case::group(r"BEGIN (f[a-z]+)", "  BEGIN foo(bar) tail   ", Some("foo"))]
    #[case::whole_match_from_start(
        "BEGIN .*",
        "  BEGIN foo(bar) tail   ",
        Some("BEGIN foo(bar) tail")
    )]
    #[case::leftmost_longest("BEGIN (f|foo)", "  BEGIN foo(bar)", Some("foo"))]
    #[case::no_match("NOMATCH", "line", None)]
    #[case::case_sensitive("bar", "fooBAR", None)]
    #[case::end_anchor("end$", "line0 end", Some("end"))]
    #[case::first_expression_decides("^  P.*\n!POS\n^ .*", "  POS two", Some("  POS two"))]
    #[case::negation_rejects("!POS\n^ .*", "  POS two", None)]
    fn extended_names(#[case] pattern: &str, #[case] line: &str, #[case] expected: Option<&str>) {
        let lines = FunctionLines::extended(pattern.as_bytes()).unwrap();
        assert_eq!(name(&lines, line).as_deref(), expected);
    }

    /// Headers are cut to 80 bytes before trailing whitespace is removed.
    #[test]
    fn cuts_to_80_bytes() {
        let lines = FunctionLines::extended(b"BEGIN.*").unwrap();
        let line = format!("BEGIN {}   yyy", "x".repeat(73));
        assert_eq!(
            name(&lines, &line),
            Some(format!("BEGIN {}", "x".repeat(73)))
        );
    }

    /// The built-in rust pattern, against lines whose headers `git diff` reported.
    #[rstest]
    #[case::function("fn foo() {", Some("fn foo() {"))]
    #[case::indented(
        "    pub(crate) fn foo(&self) -> u8 {",
        Some("pub(crate) fn foo(&self) -> u8 {")
    )]
    #[case::restricted("pub(in crate::a) fn foo() {", Some("pub(in crate::a) fn foo() {"))]
    #[case::qualifier("pub async fn foo() {", Some("pub async fn foo() {"))]
    #[case::extern_abi("extern \"C\" fn foo() {", Some("extern \"C\" fn foo() {"))]
    #[case::generic("impl<'a> Foo {", Some("impl<'a> Foo {"))]
    #[case::macro_rules("macro_rules! foo {", Some("macro_rules! foo {"))]
    #[case::keyword_only("fn ", Some("fn"))]
    #[case::two_qualifiers("pub const unsafe fn foo() {", None)]
    #[case::extern_without_abi("extern fn foo() {", None)]
    #[case::semicolon("struct Foo;", None)]
    #[case::declaration("mod foo;", None)]
    #[case::attribute("#[test] fn foo() {", None)]
    #[case::no_space_after_restriction("pub(crate)fn foo() {", None)]
    #[case::brace_after_keyword("impl{", None)]
    #[case::other_item("const FOO: u8 = 1;", None)]
    fn rust_names(#[case] line: &str, #[case] expected: Option<&str>) {
        let lines = FunctionLines::builtin(b"rust").unwrap();
        assert_eq!(name(&lines, line).as_deref(), expected);
    }

    #[rstest]
    #[case::negated_last(b"a\n!b")]
    #[case::back_reference(br"(a)\1")]
    #[case::equivalence_class(b"[[=a=]]")]
    #[case::malformed(b"(")]
    #[case::not_utf8(b"\xff")]
    #[case::trailing_newline(b"^HEADER\n")]
    #[case::empty_line(b"^a\n\n^b")]
    #[case::empty(b"")]
    fn refuses(#[case] pattern: &[u8]) {
        assert!(FunctionLines::extended(pattern).is_err());
    }
}
