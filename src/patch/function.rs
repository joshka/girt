use std::ops::Range;
use std::sync::Arc;

use regex_automata::nfa::thompson::{self, pikevm};
use regex_automata::util::captures::Captures;
use regex_automata::util::syntax;
use regex_automata::{Anchored, Input, MatchKind, meta};

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

#[derive(Clone, Copy, PartialEq)]
enum Flavor {
    Extended,
    Basic,
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
            rules.push(rule(&translate(expression, flavor)?, negated)?);
        }
        if rules.last().is_some_and(|rule| rule.negated) {
            return Err(PatternError::NegatedLast);
        }
        Ok(Self(Kind::Patterns(rules.into())))
    }

    pub(super) fn matcher(&self) -> Matcher<'_> {
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
pub(super) struct Matcher<'a> {
    lines: &'a FunctionLines,
    caches: Vec<(pikevm::Cache, Captures)>,
}

impl Matcher<'_> {
    /// The range of `line` (without its LF) a header shows, or `None` when it is not a function
    /// line.
    pub(super) fn name(&mut self, line: &[u8]) -> Option<Range<usize>> {
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

/// Translates one POSIX expression to `regex-syntax`, which differs in escapes, brackets,
/// literal operators and anchors.
fn translate(pattern: &str, flavor: Flavor) -> Result<String, PatternError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut index = 0;
    // At the start of an expression, `*` (and in extended syntax `+`, `?` and `{`) is literal.
    let mut start = true;
    while let Some(&c) = chars.get(index) {
        index += 1;
        let at_start = std::mem::replace(&mut start, false);
        match (c, flavor) {
            ('[', _) => index = bracket(&chars, index, &mut out)?,
            ('\\', _) => {
                let next = *chars
                    .get(index)
                    .ok_or(PatternError::Unsupported("trailing backslash"))?;
                index += 1;
                match (next, flavor) {
                    ('(' | '|', Flavor::Basic) => {
                        out.push(next);
                        start = true;
                    }
                    (')', Flavor::Basic) => out.push(')'),
                    ('{', Flavor::Basic) if !at_start => {
                        index = interval(&chars, index, flavor, &mut out)
                            .ok_or(PatternError::Unsupported("malformed interval"))?;
                    }
                    ('+' | '?', Flavor::Basic) if !at_start => out.push(next),
                    ('<', _) => out.push_str(r"\b{start}"),
                    ('>', _) => out.push_str(r"\b{end}"),
                    ('b' | 'B' | 'w' | 'W' | 's' | 'S', _) => {
                        out.push('\\');
                        out.push(next);
                    }
                    ('`', _) => out.push_str(r"\A"),
                    ('\'', _) => out.push_str(r"\z"),
                    ('1'..='9', _) => return Err(PatternError::Unsupported("back-reference")),
                    _ => literal(next, &mut out),
                }
            }
            ('(' | '|', Flavor::Extended) => {
                out.push(c);
                start = true;
            }
            (')', Flavor::Extended) => out.push(')'),
            ('+' | '?', Flavor::Extended) if !at_start => out.push(c),
            ('{', Flavor::Extended) if !at_start => {
                match interval(&chars, index, flavor, &mut out) {
                    Some(next) => index = next,
                    None => literal('{', &mut out),
                }
            }
            ('*', _) if !at_start => out.push('*'),
            ('^', Flavor::Extended) => {
                out.push('^');
                start = true;
            }
            ('^', Flavor::Basic) if at_start => {
                out.push('^');
                start = true;
            }
            ('$', Flavor::Extended) => out.push('$'),
            // A basic `$` anchors only at the end of an expression.
            ('$', Flavor::Basic)
                if index == chars.len()
                    || matches!(chars.get(index..index + 2), Some(['\\', ')' | '|'])) =>
            {
                out.push('$');
            }
            ('.', _) => out.push('.'),
            _ => literal(c, &mut out),
        }
    }
    Ok(out)
}

/// Translates a POSIX bracket expression starting after `[`; returns the index after its `]`.
fn bracket(chars: &[char], mut index: usize, out: &mut String) -> Result<usize, PatternError> {
    out.push('[');
    if chars.get(index) == Some(&'^') {
        out.push('^');
        index += 1;
    }
    let mut first = true;
    loop {
        let c = *chars
            .get(index)
            .ok_or(PatternError::Unsupported("unterminated bracket expression"))?;
        if c == ']' && !first {
            out.push(']');
            return Ok(index + 1);
        }
        first = false;
        if c == '['
            && let Some(&kind @ (':' | '=' | '.')) = chars.get(index + 1)
        {
            let body = chars.get(index + 2..).unwrap_or_default();
            let end = body
                .windows(2)
                .position(|pair| pair == [kind, ']'])
                .ok_or(PatternError::Unsupported("unterminated bracket class"))?;
            if kind != ':' {
                return Err(PatternError::Unsupported(
                    "collating element or equivalence class",
                ));
            }
            out.push_str("[:");
            out.extend(body.iter().take(end));
            out.push_str(":]");
            index += 2 + end + 2;
            continue;
        }
        literal(c, out);
        index += 1;
        if chars.get(index) == Some(&'-')
            && let Some(&high) = chars.get(index + 1)
            && high != ']'
        {
            out.push('-');
            literal(high, out);
            index += 2;
        }
    }
}

/// Translates `{n}`, `{n,}`, `{,m}` or `{n,m}` after the opening brace; `None` when malformed.
fn interval(chars: &[char], index: usize, flavor: Flavor, out: &mut String) -> Option<usize> {
    let close: &[char] = match flavor {
        Flavor::Extended => &['}'],
        Flavor::Basic => &['\\', '}'],
    };
    let rest = chars.get(index..)?;
    let length = rest
        .iter()
        .position(|c| !(c.is_ascii_digit() || *c == ','))?;
    let body: String = rest.get(..length)?.iter().collect();
    if !rest.get(length..)?.starts_with(close) || body.matches(',').count() > 1 || body.is_empty() {
        return None;
    }
    let (low, high) = body.split_once(',').unwrap_or((&body, &body));
    if low.is_empty() && high.is_empty() {
        return None;
    }
    out.push('{');
    out.push_str(if low.is_empty() { "0" } else { low });
    if body.contains(',') {
        out.push(',');
        out.push_str(high);
    }
    out.push('}');
    Some(index + length + close.len())
}

/// Characters `regex-syntax` treats specially; escaping any of them yields the literal.
fn is_meta(c: char) -> bool {
    matches!(
        c,
        '\\' | '.'
            | '+'
            | '*'
            | '?'
            | '('
            | ')'
            | '|'
            | '['
            | ']'
            | '{'
            | '}'
            | '^'
            | '$'
            | '#'
            | '&'
            | '-'
            | '~'
    )
}

fn literal(c: char, out: &mut String) {
    if is_meta(c) {
        out.push('\\');
    }
    out.push(c);
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::{Flavor, FunctionLines, translate};

    fn name(lines: &FunctionLines, line: &str) -> Option<String> {
        let range = lines.matcher().name(line.as_bytes())?;
        Some(line[range].to_owned())
    }

    /// Translations keep POSIX meaning where `regex-syntax` differs.
    #[rstest]
    #[case::bracket_backslash_literal(r"[\t]", Flavor::Extended, r"[\\t]")]
    #[case::bracket_leading_close(r"[]a]", Flavor::Extended, r"[\]a]")]
    #[case::bracket_class(r"[[:alpha:]_]", Flavor::Extended, r"[[:alpha:]_]")]
    #[case::leading_star_literal("*a", Flavor::Extended, r"\*a")]
    #[case::invalid_interval_literal("a{x", Flavor::Extended, r"a\{x")]
    #[case::interval("a{2,}", Flavor::Extended, "a{2,}")]
    #[case::open_low_interval("a{,3}", Flavor::Extended, "a{0,3}")]
    #[case::word_start(r"\<fn\>", Flavor::Extended, r"\b{start}fn\b{end}")]
    #[case::basic_group(r"\(a\)*", Flavor::Basic, "(a)*")]
    #[case::basic_literal_operators("(a)+?|{", Flavor::Basic, r"\(a\)\+\?\|\{")]
    #[case::basic_interval(r"a\{2\}", Flavor::Basic, "a{2}")]
    #[case::basic_inner_caret_literal("a^b$c$", Flavor::Basic, r"a\^b\$c$")]
    fn translates(#[case] pattern: &str, #[case] flavor: Flavor, #[case] expected: &str) {
        assert_eq!(translate(pattern, flavor).unwrap(), expected);
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
