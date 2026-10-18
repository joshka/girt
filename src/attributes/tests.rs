use rstest::rstest;

use super::{Attributes, Case, Limits, Source, State};

/// `m0` expands through nineteen macros to `-diff`; `git check-attr` reports `diff: unset`.
const DEEP_CHAIN: &[u8] = b"[attr]m19 -diff\n[attr]m18 m19\n[attr]m17 m18\n[attr]m16 m17\n\
[attr]m15 m16\n[attr]m14 m15\n[attr]m13 m14\n[attr]m12 m13\n[attr]m11 m12\n[attr]m10 m11\n\
[attr]m9 m10\n[attr]m8 m9\n[attr]m7 m8\n[attr]m6 m7\n[attr]m5 m6\n[attr]m4 m5\n[attr]m3 m4\n\
[attr]m2 m3\n[attr]m1 m2\n[attr]m0 m1\nf m0\n";

fn root(content: &[u8]) -> Attributes {
    let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
    attributes.add(Source::Directory(b""), content).unwrap();
    attributes
}

/// Each case was checked with `git check-attr diff` against the same root file.
#[rstest]
#[case::later_assignment_wins(b"f diff -diff\n", b"f", State::Unset)]
#[case::later_assignment_wins_reversed(b"f -diff diff\n", b"f", State::Set)]
#[case::direct_after_macro(b"f binary diff\n", b"f", State::Set)]
#[case::macro_after_direct(b"f diff binary\n", b"f", State::Unset)]
#[case::unset_macro_does_not_expand(b"f -binary\n", b"f", State::Unspecified)]
#[case::unspecify(b"f -diff\nf !diff\n", b"f", State::Unspecified)]
#[case::value(b"f diff=rust\n", b"f", State::Value(b"rust"))]
#[case::value_with_equals(b"f diff=a=b\n", b"f", State::Value(b"a=b"))]
#[case::empty_value(b"f diff=\n", b"f", State::Value(b""))]
#[case::value_after_unset_ignored(b"f -diff=x\n", b"f", State::Unset)]
#[case::later_line_wins(b"*.rs diff=rust\nf.rs -diff\n", b"f.rs", State::Unset)]
#[case::earlier_line_used_when_later_misses(
    b"*.rs diff=rust\ng.rs -diff\n",
    b"f.rs",
    State::Value(b"rust")
)]
#[case::basename_anywhere(b"*.rs -diff\n", b"a/b/c.rs", State::Unset)]
#[case::anchored(b"/top -diff\n", b"a/top", State::Unspecified)]
#[case::directory_pattern_matches_no_file(b"dir/ -diff\n", b"dir/f", State::Unspecified)]
#[case::recursive(b"vendor/** -diff\n", b"vendor/a/b", State::Unset)]
#[case::quoted(b"\"q x\" -diff\n", b"q x", State::Unset)]
#[case::quoted_octal(b"\"oct\\101\" -diff\n", b"octA", State::Unset)]
#[case::quote_then_attribute(b"\"x\"y -diff\n", b"x", State::Unset)]
#[case::unterminated_quote_literal(b"\"un -diff\n", b"\"un", State::Unset)]
#[case::negative_pattern_ignored(b"!neg -diff\n", b"neg", State::Unspecified)]
#[case::escaped_bang(b"\\!esc -diff\n", b"!esc", State::Unset)]
#[case::escaped_hash(b"\\#hash -diff\n", b"#hash", State::Unset)]
#[case::comment(b"# f -diff\n", b"#", State::Unspecified)]
#[case::leading_blanks_and_tab(b"  f\t-diff\n", b"f", State::Unset)]
#[case::crlf(b"f -diff\r\n", b"f", State::Unset)]
#[case::invalid_name_drops_line(b"f -diff a@b\n", b"f", State::Unspecified)]
#[case::empty_name_drops_line(b"f -diff -\n", b"f", State::Unspecified)]
#[case::quoted_name_drops_line(b"f \"-diff\"\n", b"f", State::Unspecified)]
#[case::digit_name_allowed(b"f 0bad=1 -diff\n", b"f", State::Unset)]
#[case::user_macro(b"[attr]gen -diff\nf gen\n", b"f", State::Unset)]
#[case::nested_macro(b"[attr]inner -diff\n[attr]outer inner\nf outer\n", b"f", State::Unset)]
#[case::later_unset_macro_blocks_expansion(b"f binary\nf -binary\n", b"f", State::Unspecified)]
#[case::later_unspecified_macro_blocks_expansion(
    b"f binary\nf !binary\n",
    b"f",
    State::Unspecified
)]
#[case::same_line_unset_macro_blocks_expansion(b"f binary -binary\n", b"f", State::Unspecified)]
#[case::deep_macro_chain(DEEP_CHAIN, b"f", State::Unset)]
#[case::bom(b"\xef\xbb\xbff -diff\n", b"f", State::Unset)]
fn root_file_state(#[case] content: &[u8], #[case] path: &[u8], #[case] expected: State<'_>) {
    assert_eq!(root(content).get(path, b"diff").unwrap(), expected);
}

/// The `binary` macro itself is an attribute and sets `-merge -text` as well as `-diff`.
#[rstest]
#[case::binary(b"binary", State::Set)]
#[case::merge(b"merge", State::Unset)]
#[case::text(b"text", State::Unset)]
fn binary_macro(#[case] name: &[u8], #[case] expected: State<'_>) {
    assert_eq!(root(b"f binary\n").get(b"f", name).unwrap(), expected);
}

/// A higher-precedence file that unsets a macro stops a lower file's expansion of it.
#[test]
fn higher_file_unset_macro_blocks_expansion() {
    let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
    attributes
        .add(Source::Directory(b""), b"f binary\n")
        .unwrap();
    attributes.add(Source::Info, b"f -binary\n").unwrap();
    assert_eq!(attributes.get(b"f", b"diff").unwrap(), State::Unspecified);
}

/// Deeper `.gitattributes` and `info/attributes` override; global and system files lose.
#[rstest]
#[case::deeper_directory(b"sub/f", State::Value(b"deep"))]
#[case::info_overrides(b"info", State::Value(b"info"))]
#[case::root_over_global(b"g", State::Value(b"root"))]
#[case::global_over_system(b"s", State::Value(b"global"))]
fn precedence(#[case] path: &[u8], #[case] expected: State<'_>) {
    let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
    attributes.add(Source::Info, b"info diff=info\n").unwrap();
    attributes
        .add(Source::Directory(b"sub"), b"f diff=deep\n")
        .unwrap();
    attributes
        .add(
            Source::Directory(b""),
            b"f diff=root\ninfo diff=root\ng diff=root\n",
        )
        .unwrap();
    attributes
        .add(Source::Global, b"g diff=global\ns diff=global\n")
        .unwrap();
    attributes.add(Source::System, b"s diff=system\n").unwrap();
    assert_eq!(attributes.get(path, b"diff").unwrap(), expected);
}

/// Git refuses macro definitions in a `.gitattributes` below the root.
#[test]
fn nested_macro_definition_ignored() {
    let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
    attributes
        .add(Source::Directory(b"sub"), b"[attr]m -diff\nf m\n")
        .unwrap();
    assert_eq!(
        attributes.get(b"sub/f", b"diff").unwrap(),
        State::Unspecified
    );
    assert_eq!(attributes.get(b"sub/f", b"m").unwrap(), State::Set);
}

/// Directory prefixes and patterns fold ASCII case together under `core.ignoreCase`.
#[test]
fn ascii_insensitive() {
    let mut attributes = Attributes::new(Case::AsciiInsensitive, Limits::default());
    attributes
        .add(Source::Directory(b"dir"), b"X -diff\n")
        .unwrap();
    assert_eq!(attributes.get(b"DIR/x", b"diff").unwrap(), State::Unset);
}

/// A rejected file leaves earlier files usable.
#[test]
fn limit_is_atomic() {
    let mut attributes = Attributes::new(Case::Sensitive, Limits::default().with_lines(1));
    attributes.add(Source::Info, b"a -diff\n").unwrap();
    assert!(
        attributes
            .add(Source::Global, b"b -diff\nc -diff\n")
            .is_err()
    );
    assert_eq!(attributes.get(b"a", b"diff").unwrap(), State::Unset);
    assert_eq!(attributes.get(b"b", b"diff").unwrap(), State::Unspecified);
}

/// A self-referential macro expands once and terminates.
#[test]
fn recursive_macro_terminates() {
    let attributes = root(b"[attr]loop loop\nf loop\n");
    assert_eq!(attributes.get(b"f", b"diff").unwrap(), State::Unspecified);
}
