use rstest::rstest;

use super::*;

fn root(content: &[u8]) -> Ignore {
    let mut rules = Ignore::new(Case::Sensitive, Limits::default());
    rules.add(Source::Directory(b""), content).unwrap();
    rules
}
#[rstest]
#[case::last_rule(b"*.log\n!keep.log\n", b"keep.log", false, Some(false))]
#[case::last_exclusion(b"!keep.log\n*.log\n", b"keep.log", false, Some(true))]
#[case::parent_wins(b"build/\n!build/keep\n", b"build/keep", false, Some(true))]
#[case::parent_reincluded(b"build/\n!build/\n!build/keep\n", b"build/keep", false, Some(false))]
#[case::basename(b"cache\n", b"a/cache/x", false, Some(true))]
#[case::anchored(b"/cache\n", b"a/cache", true, None)]
#[case::directory_file(b"cache/\n", b"cache", false, None)]
#[case::recursive_zero(b"a/**/z\n", b"a/z", false, Some(true))]
#[case::recursive_many(b"a/**/z\n", b"a/x/y/z", false, Some(true))]
#[case::single_star(b"a/*/z\n", b"a/x/y/z", false, None)]
#[case::escape_space(b"a\\ \n", b"a ", false, Some(true))]
#[case::comment(b"#a\n", b"#a", false, None)]
#[case::escaped_comment(b"\\#a\n", b"#a", false, Some(true))]
#[case::escaped_negation(b"\\!a\n", b"!a", false, Some(true))]
#[case::bytes(b"\xff?\n", b"\xff\xfe", false, Some(true))]
#[case::bom_crlf(b"\xef\xbb\xbffoo\r\n", b"foo", false, Some(true))]
#[case::nul(b"foo\0bar\n", b"foo", false, Some(true))]
#[case::escape_end(b"foo\\\n", b"foo", false, None)]
#[case::invalid_class(b"[![:invalid:]]\n", b"z", false, None)]
#[case::backslash_is_one_byte(b"a?\n", b"a\\", false, Some(true))]
#[case::literal_backslash(b"a\\\\\n", b"a\\", false, Some(true))]
#[case::range(b"[a-c][[:digit:]]\n", b"b7", false, Some(true))]
fn semantics(
    #[case] content: &[u8],
    #[case] path: &[u8],
    #[case] directory: bool,
    #[case] expected: Option<bool>,
) {
    assert_eq!(
        root(content)
            .check(path, directory)
            .unwrap()
            .map(|m| m.ignored),
        expected
    );
}
#[test]
fn hierarchy_uses_depth_and_source_precedence_with_provenance() {
    let mut rules = Ignore::new(Case::Sensitive, Limits::default());
    rules
        .add(Source::Directory(b"a"), b"# comment\n!x\n")
        .unwrap();
    rules.add(Source::Global, b"x\n").unwrap();
    rules.add(Source::Directory(b""), b"x\n").unwrap();
    rules.add(Source::Info, b"x\n").unwrap();
    assert_eq!(
        rules.check(b"a/x", false).unwrap(),
        Some(Match {
            ignored: false,
            source: 0,
            line: 2,
            matched_bytes: 3
        })
    );
    rules.add(Source::Command, b"x\n").unwrap();
    assert_eq!(rules.check(b"a/x", false).unwrap().unwrap().source, 4);
}
#[test]
fn blocked_parent_reports_ancestor_and_ignores_nested_source() {
    let mut rules = root(b"a/\n");
    rules.add(Source::Directory(b"a"), b"!*\n").unwrap();
    assert_eq!(
        rules.check(b"a/x", false).unwrap(),
        Some(Match {
            ignored: true,
            source: 0,
            line: 1,
            matched_bytes: 1
        })
    );
}
#[test]
fn equal_level_sources_append() {
    let mut rules = root(b"x\n");
    rules.add(Source::Directory(b""), b"!x\n").unwrap();
    assert!(!rules.check(b"x", false).unwrap().unwrap().ignored);
}
#[rstest]
#[case::empty(b"")]
#[case::absolute(b"/x")]
#[case::empty_component(b"a//b")]
#[case::dot(b"a/./b")]
#[case::parent(b"a/../b")]
#[case::trailing(b"a/")]
#[case::nul(b"a\0b")]
fn invalid_query(#[case] path: &[u8]) {
    assert_eq!(root(b"*").check(path, false), Err(Error::Path));
}
#[rstest]
#[case::source_bytes(Limits { bytes: 3, ..Limits::default() }, b"long", "source bytes")]
#[case::source_count(Limits { sources: 1, ..Limits::default() }, b"", "source count")]
#[case::pattern_count(Limits { patterns: 1, ..Limits::default() }, b"y", "pattern count")]
#[case::line_bytes(Limits { line_bytes: 1, ..Limits::default() }, b"yy", "line bytes")]
fn failed_add_is_atomic(
    #[case] limits: Limits,
    #[case] content: &[u8],
    #[case] limit: &'static str,
) {
    let mut rules = Ignore::new(Case::Sensitive, limits);
    rules.add(Source::Global, b"x").unwrap();
    assert_eq!(rules.add(Source::Info, content), Err(Error::Limit(limit)));
    assert_eq!(rules.check(b"x", false).unwrap().unwrap().source, 0);
    assert_eq!(rules.check(b"y", false).unwrap(), None);
}
#[test]
fn aggregate_bytes_include_directory_prefix() {
    let mut rules = Ignore::new(
        Case::Sensitive,
        Limits {
            bytes: 3,
            ..Limits::default()
        },
    );
    assert_eq!(rules.add(Source::Directory(b"ab"), b"x"), Ok(0));
    assert_eq!(
        rules.add(Source::Info, b"y"),
        Err(Error::Limit("source bytes"))
    );
}
#[test]
fn failed_directory_does_not_consume_source_index() {
    let mut rules = root(b"x");
    assert_eq!(
        rules.add(Source::Directory(b"../a"), b"x"),
        Err(Error::Path)
    );
    assert_eq!(rules.add(Source::Info, b"x"), Ok(1));
}
#[rstest]
#[case::zero(0)]
#[case::adversarial(1000)]
fn query_budget_is_not_a_negative_decision(#[case] work: usize) {
    let mut rules = Ignore::new(
        Case::Sensitive,
        Limits {
            work,
            ..Limits::default()
        },
    );
    rules
        .add(Source::Global, b"*a*a*a*a*a*a*a*a*a*a*a*a*a*a*b")
        .unwrap();
    assert_eq!(
        rules.check(&[b'a'; 100], false),
        Err(Error::Limit("matching work"))
    );
    assert_eq!(
        rules.check(&[b'a'; 100], false),
        Err(Error::Limit("matching work"))
    );
}
#[test]
fn query_work_is_shared_across_ancestors() {
    let mut rules = Ignore::new(
        Case::Sensitive,
        Limits {
            work: 5,
            ..Limits::default()
        },
    );
    rules.add(Source::Global, b"z").unwrap();
    assert_eq!(rules.check(b"a", true), Ok(None));
    assert_eq!(
        rules.check(b"a/b", false),
        Err(Error::Limit("matching work"))
    );
}
#[test]
fn path_budget_applies_to_both_boundaries() {
    let mut rules = Ignore::new(
        Case::Sensitive,
        Limits {
            path_bytes: 1,
            ..Limits::default()
        },
    );
    assert_eq!(
        rules.add(Source::Directory(b"ab"), b""),
        Err(Error::Limit("path bytes"))
    );
    assert_eq!(rules.check(b"ab", false), Err(Error::Limit("path bytes")));
}
#[test]
fn case_policy_includes_source_prefix_without_unicode_folding() {
    let mut rules = Ignore::new(Case::AsciiInsensitive, Limits::default());
    rules
        .add(Source::Directory(b"DIR"), b"[A-Z]\n\xc3\x84")
        .unwrap();
    assert!(rules.check(b"dir/a", false).unwrap().unwrap().ignored);
    assert_eq!(rules.check(b"dir/\xc3\xa4", false).unwrap(), None);
    assert_eq!(rules.check(b"directory/a", false).unwrap(), None);
}
