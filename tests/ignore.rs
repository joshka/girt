//! Original pattern fixtures compared with Git's executable; no upstream test/source inputs.
#[path = "support/layout_git.rs"]
mod git;

use std::fs;

use girt::ObjectFormat;
use girt::ignore::{Case, Ignore, Limits, Source};
use rstest::rstest;

fn repository(format: ObjectFormat) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git::git(
        root.path(),
        &[
            "init",
            "--initial-branch=main",
            &format!("--object-format={format}"),
        ],
        b"",
    );
    git::git(root.path(), &["config", "core.ignoreCase", "false"], b"");
    git::git(
        root.path(),
        &["config", "core.excludesFile", "absent-excludes"],
        b"",
    );
    root
}
fn oracle(root: &std::path::Path, path: &[u8], directory: bool) -> Option<bool> {
    let mut input = path.to_vec();
    if directory {
        fs::create_dir_all(root.join(std::str::from_utf8(path).unwrap())).unwrap();
    }
    input.push(0);
    let output = git::attempt(
        root,
        &["check-ignore", "--no-index", "--verbose", "-z", "--stdin"],
        &input,
    );
    assert!(matches!(output.status.code(), Some(0 | 1)), "{:?}", output);
    if output.stdout.is_empty() {
        return None;
    }
    let fields: Vec<_> = output.stdout.split(|b| *b == 0).collect();
    assert_eq!(fields.len(), 5);
    Some(!fields[2].starts_with(b"!"))
}
#[rstest]
#[case::empty(b"", b"foo", false, false)]
#[case::comment(b"#foo\n", b"#foo", false, false)]
#[case::escaped_comment(b"\\#foo\n", b"#foo", false, false)]
#[case::escaped_negation(b"\\!foo\n", b"!foo", false, false)]
#[case::blank_negation(b"!\n", b"!", false, false)]
#[case::spaces(b"foo   \n", b"foo", false, false)]
#[case::escaped_space(b"foo\\ \n", b"foo ", false, false)]
#[case::space_then_trim(b"foo\\   \n", b"foo ", false, false)]
#[case::leading_space(b" foo\n", b" foo", false, false)]
#[case::tabs(b"foo\t\n", b"foo\t", false, false)]
#[case::crlf(b"foo\r\n", b"foo", false, false)]
#[case::bom(b"\xef\xbb\xbffoo\n", b"foo", false, false)]
#[case::embedded_bom(b"x\n\xef\xbb\xbffoo\n", b"\xef\xbb\xbffoo", false, false)]
#[case::nul(b"foo\0bar\n", b"foo", false, false)]
#[case::nul_next_line(b"foo\0bar\nbaz\n", b"baz", false, false)]
#[case::anchored_root(b"/foo\n", b"foo", false, false)]
#[case::anchored_deep(b"/foo\n", b"a/foo", false, false)]
#[case::basename(b"foo\n", b"a/foo", false, false)]
#[case::middle_slash(b"a/foo\n", b"z/a/foo", false, false)]
#[case::directory(b"foo/\n", b"foo", true, false)]
#[case::directory_file(b"foo/\n", b"foo", false, false)]
#[case::directory_descendant(b"foo/\n", b"foo/bar", false, false)]
#[case::parent_exclusion(b"foo/\n!foo/bar\n", b"foo/bar", false, false)]
#[case::reinclude_parent(b"foo/\n!foo/\n!foo/bar\n", b"foo/bar", false, false)]
#[case::last_match(b"*.log\n!keep.log\n", b"keep.log", false, false)]
#[case::slash_star(b"a/*/z\n", b"a/x/y/z", false, false)]
#[case::escaped_recursive_separator(b"**\\/a\n", b"b/b/a", false, false)]
#[case::escaped_recursive_requires_separator(b"**\\/a\n", b"a", false, false)]
#[case::double_star_zero(b"a/**/z\n", b"a/z", false, false)]
#[case::double_star_many(b"a/**/z\n", b"a/x/y/z", false, false)]
#[case::leading_double_star(b"**/x/z\n", b"x/z", false, false)]
#[case::trailing_double_star(b"a/**\n", b"a/x/y", false, false)]
#[case::trailing_double_star_parent(b"a/**\n", b"a", true, false)]
#[case::triple_star(b"a/***/z\n", b"a/x/y/z", false, false)]
#[case::embedded_double_star(b"a**z\n", b"abz", false, false)]
#[case::question(b"a?z\n", b"a/z", false, false)]
#[case::dot(b"*\n", b".hidden", false, false)]
#[case::class(b"[a-c][0-9]\n", b"b7", false, false)]
#[case::class_negative(b"[!a-c]\n", b"z", false, false)]
#[case::class_caret(b"[^a-c]\n", b"z", false, false)]
#[case::class_close(b"[]a]\n", b"]", false, false)]
#[case::class_dash(b"[-a]\n", b"-", false, false)]
#[case::class_slash(b"[a/]\n", b"a", false, false)]
#[case::class_named(b"[[:digit:]][[:alpha:]]\n", b"7a", false, false)]
#[case::class_upper(b"[[:upper:]]\n", b"a", false, true)]
#[case::class_unknown(b"[![:unknown:]]\n", b"z", false, false)]
#[case::class_open(b"[abc\n", b"[abc", false, false)]
#[case::class_reversed(b"[z-a]\n", b"m", false, false)]
#[case::terminal_escape(b"foo\\\n", b"foo", false, false)]
#[case::escaped_wildcard(b"a\\*\n", b"a*", false, false)]
#[case::escaped_slash(b"foo\\/\n", b"foo", true, false)]
#[case::raw_bytes(b"\xff?\n", b"\xff\xfe", false, false)]
#[case::ascii_fold(b"README\n", b"readme", false, true)]
#[case::ascii_range_fold(b"[A-Z]\n", b"a", false, true)]
#[case::unicode_no_fold(b"\xc3\x84\n", b"\xc3\xa4", false, true)]
#[case::newline_path(b"a?z\n", b"a\nz", false, false)]
fn git_patterns(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] content: &[u8],
    #[case] path: &[u8],
    #[case] directory: bool,
    #[case] fold: bool,
) {
    let root = repository(format);
    fs::write(root.path().join(".gitignore"), content).unwrap();
    git::git(
        root.path(),
        &[
            "config",
            "core.ignoreCase",
            if fold { "true" } else { "false" },
        ],
        b"",
    );
    let mut rules = Ignore::new(
        if fold {
            Case::AsciiInsensitive
        } else {
            Case::Sensitive
        },
        Limits::default(),
    );
    rules.add(Source::Directory(b""), content).unwrap();
    assert_eq!(
        rules.check(path, directory).unwrap().map(|m| m.ignored),
        oracle(root.path(), path, directory)
    );
}

#[rstest]
fn hierarchy_and_loading_policy(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = repository(format);
    fs::create_dir(root.path().join("nested")).unwrap();
    fs::write(root.path().join("global-excludes"), b"*.log\n*.tmp\n").unwrap();
    fs::write(root.path().join(".git/info/exclude"), b"!info.log\n").unwrap();
    fs::write(
        root.path().join(".gitignore"),
        b"!root.log\ninfo.log\nblocked/\n",
    )
    .unwrap();
    fs::write(root.path().join("nested/.gitignore"), b"!deep.log\n").unwrap();
    git::git(
        root.path(),
        &["config", "core.excludesFile", "global-excludes"],
        b"",
    );
    let mut rules = Ignore::new(Case::Sensitive, Limits::default());
    rules
        .add(
            Source::Directory(b"nested"),
            &fs::read(root.path().join("nested/.gitignore")).unwrap(),
        )
        .unwrap();
    rules
        .add(
            Source::Global,
            &fs::read(root.path().join("global-excludes")).unwrap(),
        )
        .unwrap();
    rules
        .add(
            Source::Info,
            &fs::read(root.path().join(".git/info/exclude")).unwrap(),
        )
        .unwrap();
    rules
        .add(
            Source::Directory(b""),
            &fs::read(root.path().join(".gitignore")).unwrap(),
        )
        .unwrap();
    assert_eq!(
        rules
            .check(b"nested/deep.log", false)
            .unwrap()
            .map(|m| m.ignored),
        oracle(root.path(), b"nested/deep.log", false)
    );
    assert_eq!(
        rules.check(b"root.log", false).unwrap().map(|m| m.ignored),
        oracle(root.path(), b"root.log", false)
    );
    assert_eq!(
        rules.check(b"info.log", false).unwrap().map(|m| m.ignored),
        oracle(root.path(), b"info.log", false)
    );
    assert_eq!(
        rules.check(b"other.tmp", false).unwrap().map(|m| m.ignored),
        oracle(root.path(), b"other.tmp", false)
    );
    assert_eq!(
        rules
            .check(b"blocked/keep", false)
            .unwrap()
            .map(|m| m.ignored),
        oracle(root.path(), b"blocked/keep", false)
    );
}

#[rstest]
fn tracked_selection_remains_caller_owned(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = repository(format);
    fs::write(root.path().join("tracked"), b"data").unwrap();
    git::git(root.path(), &["add", "tracked"], b"");
    fs::write(root.path().join(".gitignore"), b"tracked\n").unwrap();
    let mut rules = Ignore::new(Case::Sensitive, Limits::default());
    rules.add(Source::Directory(b""), b"tracked\n").unwrap();
    assert!(rules.check(b"tracked", false).unwrap().unwrap().ignored);
    assert_eq!(oracle(root.path(), b"tracked", false), Some(true));
    assert_eq!(
        git::attempt(root.path(), &["check-ignore", "tracked"], b"")
            .status
            .code(),
        Some(1)
    );
}
