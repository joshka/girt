//! Each expected output was checked against `git grep --threads=1` on the same content.
use rstest::rstest;

use super::{Binary, Expression, GitFormat, Listing, Matching, Options, Query, Searcher, Syntax};

fn query(pattern: &str, matching: Matching) -> Query {
    Query::new(&Expression::pattern(pattern), matching).unwrap()
}

fn spans(query: &Query, line: &str) -> Vec<String> {
    query
        .matches(line.as_bytes())
        .into_iter()
        .map(|span| line[span].to_owned())
        .collect()
}

/// Prints `files` as `git grep` would with `options` and `format`.
fn render(query: Query, options: Options, format: GitFormat, files: &[(&str, &str)]) -> String {
    let mut searcher = Searcher::new(query, options);
    let mut format = format;
    let mut out = Vec::new();
    for (name, content) in files {
        let matches = searcher
            .search(name.as_bytes(), content.as_bytes())
            .unwrap();
        format
            .write(
                &mut out,
                name.as_bytes(),
                content.as_bytes(),
                &matches,
                &searcher,
            )
            .unwrap();
    }
    String::from_utf8(out).unwrap()
}

#[rstest]
#[case::basic_plus_literal("a+", Syntax::Basic, "aa a+", &["a+"])]
#[case::basic_escaped_plus(r"a\+", Syntax::Basic, "aa a+", &["aa", "a"])]
#[case::extended_plus("a+", Syntax::Extended, "aa a+", &["aa", "a"])]
#[case::fixed("a.c", Syntax::Fixed, "abc a.c", &["a.c"])]
#[case::leftmost_longest("f|foo", Syntax::Extended, "foo", &["foo"])]
#[case::empty_match_stops("x*", Syntax::Basic, "ab x", &[])]
fn reports_matches(
    #[case] pattern: &str,
    #[case] syntax: Syntax,
    #[case] line: &str,
    #[case] expected: &[&str],
) {
    let query = query(pattern, Matching::new().with_syntax(syntax));
    assert_eq!(spans(&query, line), expected);
}

/// A failed word test resumes one byte after the match start and never tries a shorter match.
#[rstest]
#[case::word_inside("foo", "foofoo foo_x xfoo foo", &["foo"])]
#[case::resumes_after_start(r"\.\?b", "_.b", &["b"])]
#[case::no_shorter_match(r"foo b\?", "foo bx", &[])]
fn word_matching(#[case] pattern: &str, #[case] line: &str, #[case] expected: &[&str]) {
    let query = query(pattern, Matching::new().with_word(true));
    assert_eq!(spans(&query, line), expected);
}

#[test]
fn ignores_case() {
    let query = query("FOO", Matching::new().with_ignore_case(true));
    assert_eq!(spans(&query, "foo Foo"), ["foo", "Foo"]);
}

/// Each line of a pattern is a pattern of its own.
#[test]
fn splits_patterns_at_newlines() {
    for syntax in [Syntax::Basic, Syntax::Fixed] {
        let query = query("foo\nbar", Matching::new().with_syntax(syntax));
        assert!(query.is_match(b"foo"));
        assert!(query.is_match(b"bar"));
    }
}

/// In the C locale, `.` matches any byte.
#[test]
fn matches_bytes() {
    assert!(!query(".", Matching::new()).is_match(b"\xff"));
    assert!(query(".", Matching::new().with_bytes(true)).is_match(b"\xff"));
}

#[test]
fn refuses_back_references() {
    let refused = Query::new(&Expression::pattern(r"\(a\)\1"), Matching::new());
    assert!(refused.is_err());
}

#[test]
fn combines_expressions() {
    let expression = Expression::pattern("foo").and(!Expression::pattern("bar"));
    let query = Query::new(&expression, Matching::new()).unwrap();
    assert!(query.is_match(b"foo"));
    assert!(!query.is_match(b"foo bar"));
    assert!(!query.is_match(b"baz"));
    // Patterns under --not are not shown by -o.
    assert_eq!(spans(&query, "foo"), ["foo"]);
}

#[test]
fn all_match_needs_every_branch() {
    let expression = Expression::pattern("foo").or(Expression::pattern("baz"));
    let all = || {
        Query::new(&expression, Matching::new())
            .unwrap()
            .with_all_match(true)
    };
    let out = render(
        all(),
        Options::new(),
        GitFormat::new(),
        &[("a", "foo\nbaz\n"), ("b", "foo\n")],
    );
    assert_eq!(out, "a:foo\na:baz\n");
}

/// `-m` stops selecting lines, but --all-match still reads on to see every branch match.
#[test]
fn all_match_reads_past_max_count() {
    let expression = Expression::pattern("foo").or(Expression::pattern("bar"));
    let query = Query::new(&expression, Matching::new())
        .unwrap()
        .with_all_match(true);
    let out = render(
        query,
        Options::new().with_max_count(Some(1)),
        GitFormat::new(),
        &[("f", "foo\nbar\n")],
    );
    assert_eq!(out, "f:foo\n");
}

const P: &str = "fa\n 1\n 2\n 3 m\n 4\n 5\n 6 m\nfb m\n 7\n 8\n 9\n10 m\n11 m\n";

#[rstest]
#[case::function_name(
    Options::new().with_function_name(true),
    "p=fa\np: 3 m\np: 6 m\np:fb m\np:10 m\np:11 m\n"
)]
#[case::function_name_before_context(
    Options::new().with_function_name(true).with_before(1),
    "p=fa\n--\np- 2\np: 3 m\n--\np- 5\np: 6 m\np:fb m\n--\np- 9\np:10 m\np:11 m\n"
)]
#[case::max_count_keeps_after_context(
    Options::new().with_max_count(Some(1)).with_after(1),
    "p: 3 m\np- 4\n"
)]
fn shows_function_lines(#[case] options: Options, #[case] expected: &str) {
    let out = render(
        query("m$", Matching::new()),
        options,
        GitFormat::new(),
        &[("p", P)],
    );
    assert_eq!(out, expected);
}

/// `-p` marks the nearest earlier function line; `-W` takes the non-blank lines above a function
/// line and drops blank lines before the next one.
#[rstest]
#[case::nearest_function(
    " m",
    "fa\nfb\n m\n",
    Options::new().with_function_name(true).with_before(2),
    "s-fa\ns=fb\ns: m\n"
)]
#[case::body_with_comment(
    "1",
    "fz\n 0\n  \n x\nfa\n 1\n \t\n\nfb\n 2\n",
    Options::new().with_function_body(true),
    "s- x\ns=fa\ns: 1\n"
)]
#[case::body_without_function(
    "x",
    " 1\n x\nfa\n",
    Options::new().with_function_body(true),
    "s- 1\ns: x\n"
)]
fn function_context(
    #[case] pattern: &str,
    #[case] content: &str,
    #[case] options: Options,
    #[case] expected: &str,
) {
    let out = render(
        query(pattern, Matching::new()),
        options,
        GitFormat::new(),
        &[("s", content)],
    );
    assert_eq!(out, expected);
}

#[rstest]
#[case::report(Binary::Report, "Binary file b matches\nt:foo\n")]
#[case::text(Binary::Text, "b:foo\0\nt:foo\n")]
#[case::skip(Binary::Skip, "t:foo\n")]
fn binary_files(#[case] binary: Binary, #[case] expected: &str) {
    let out = render(
        query("foo", Matching::new()),
        Options::new().with_binary(binary),
        GitFormat::new(),
        &[("b", "foo\0\n"), ("t", "foo\n")],
    );
    assert_eq!(out, expected);
}

/// Binary messages neither take nor trigger a `--` separator; `--break` replaces it.
#[rstest]
#[case::separators(GitFormat::new(), "Binary file b matches\nt:foo\nt-x\n--\nu:foo\n")]
#[case::break_replaces_separator(
    GitFormat::new().with_break(true).with_heading(true).with_line_numbers(true),
    "Binary file b matches\nt\n1:foo\n2-x\n\nu\n1:foo\n"
)]
#[case::null(GitFormat::new().with_null(true).with_line_numbers(true), "Binary file b matches\nt\x001\0foo\nt\x002\0x\n--\nu\x001\0foo\n")]
#[case::count(GitFormat::new().with_listing(Listing::Count), "b:1\nt:1\nu:1\n")]
#[case::names(GitFormat::new().with_listing(Listing::FilesWithMatches).with_null(true), "b\0t\0u\0")]
#[case::without(GitFormat::new().with_listing(Listing::FilesWithoutMatch), "v\n")]
fn formats(#[case] format: GitFormat, #[case] expected: &str) {
    let out = render(
        query("foo", Matching::new()),
        Options::new().with_after(1),
        format,
        &[
            ("b", "foo\0"),
            ("t", "foo\nx"),
            ("u", "foo\n"),
            ("v", "bar\n"),
        ],
    );
    assert_eq!(out, expected);
}

#[test]
fn only_matching_and_column() {
    let format = GitFormat::new()
        .with_only_matching(true)
        .with_column(true)
        .with_line_numbers(true);
    let out = render(
        query("foo", Matching::new()),
        Options::new(),
        format,
        &[("w", "x foo foo\n")],
    );
    assert_eq!(out, "w:1:3:foo\nw:1:7:foo\n");
}
