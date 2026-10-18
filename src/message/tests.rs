use rstest::rstest;

use super::{Cleanup, CommentPrefix, cleanup};
use crate::Config;

#[rstest]
#[case::empty(b"", b"")]
#[case::only_blank(b"\n \n\t\n", b"")]
#[case::adds_final_newline(b"a", b"a\n")]
#[case::collapses_blank_runs(b"\n\na  \n\n\n b\t\n\n", b"a\n\n b\n")]
#[case::strips_carriage_returns(b"a\r\nb \r\n", b"a\nb\n")]
#[case::keeps_vertical_tab_and_form_feed(b"a\x0b\x0c \n", b"a\x0b\x0c\n")]
#[case::keeps_nul(b"a\0b  \n", b"a\0b\n")]
#[case::keeps_comments(b"#c\na\n", b"#c\na\n")]
fn whitespace(#[case] input: &[u8], #[case] expected: &[u8]) {
    let comment = CommentPrefix::default();
    assert_eq!(cleanup(input, Cleanup::Whitespace, &comment), expected);
}

#[rstest]
#[case::column_zero_only(b" #x\n#\tx\n##y\na", b" #x\na\n")]
#[case::comment_between_blank_lines(b"a\n\n# c\n\nb\n", b"a\n\nb\n")]
#[case::all_comments(b"# a\n#\n", b"")]
fn strip(#[case] input: &[u8], #[case] expected: &[u8]) {
    let comment = CommentPrefix::default();
    assert_eq!(cleanup(input, Cleanup::Strip, &comment), expected);
}

#[test]
fn verbatim_keeps_bytes() {
    let input = b"\n# x  \n\n\n";
    assert_eq!(
        cleanup(input, Cleanup::Verbatim, &CommentPrefix::default()),
        input
    );
}

#[rstest]
#[case::cuts(
    b"a\n# ------------------------ >8 ------------------------\nz\n",
    b"a\n"
)]
#[case::needs_one_space(
    b"a\n#  ------------------------ >8 ------------------------\nz\n",
    b"a\n#  ------------------------ >8 ------------------------\nz\n"
)]
#[case::trailing_space(
    b"a\n# ------------------------ >8 ------------------------ \nz\n",
    b"a\n"
)]
#[case::trailing_tab_cr(
    b"a\n# ------------------------ >8 ------------------------\t\r\nz\n",
    b"a\n"
)]
#[case::needs_exact_line(
    b"a\n# ------------------------ >8 ------------------------ x\n",
    b"a\n# ------------------------ >8 ------------------------ x\n"
)]
#[case::keeps_comments_above(
    b"a\n  #\n# c\n# ------------------------ >8 ------------------------\n",
    b"a\n  #\n# c\n"
)]
fn scissors(#[case] input: &[u8], #[case] expected: &[u8]) {
    let comment = CommentPrefix::default();
    assert_eq!(cleanup(input, Cleanup::Scissors, &comment), expected);
}

#[test]
fn multi_byte_prefix() {
    let comment = CommentPrefix::new(b"//").unwrap();
    assert_eq!(
        cleanup(b"// c\n//\n/x\na\n", Cleanup::Strip, &comment),
        b"/x\na\n"
    );
    let scissors = b"a\n// ------------------------ >8 ------------------------\nz\n";
    assert_eq!(cleanup(scissors, Cleanup::Scissors, &comment), b"a\n");
}

#[rstest]
#[case::unset(b"", true, Cleanup::Strip)]
#[case::unset_given(b"", false, Cleanup::Whitespace)]
#[case::default_edited(b"[commit]\ncleanup = default\n", true, Cleanup::Strip)]
#[case::default_given(b"[commit]\ncleanup = default\n", false, Cleanup::Whitespace)]
#[case::scissors_given(b"[commit]\ncleanup = scissors\n", false, Cleanup::Whitespace)]
#[case::strip_given(b"[commit]\ncleanup = strip\n", false, Cleanup::Strip)]
#[case::verbatim(b"[commit]\ncleanup = verbatim\n", true, Cleanup::Verbatim)]
#[case::last_wins(
    b"[commit]\ncleanup = strip\ncleanup = whitespace\n",
    true,
    Cleanup::Whitespace
)]
fn configured_mode(#[case] config: &[u8], #[case] edited: bool, #[case] expected: Cleanup) {
    let config = Config::parse(config).unwrap();
    assert_eq!(Cleanup::for_commit(&config, edited).unwrap(), expected);
}

#[rstest]
#[case::unknown(b"[commit]\ncleanup = bogus\n")]
#[case::case_sensitive(b"[commit]\ncleanup = STRIP\n")]
#[case::implicit(b"[commit]\ncleanup\n")]
fn configured_mode_rejects(#[case] config: &[u8]) {
    let config = Config::parse(config).unwrap();
    assert!(Cleanup::for_commit(&config, true).is_err());
}

#[rstest]
#[case::unset(b"", Some(&b"#"[..]))]
#[case::char(b"[core]\ncommentChar = \";\"\n", Some(&b";"[..]))]
#[case::string_after_char(b"[core]\ncommentChar = \";\"\ncommentString = %\n", Some(&b"%"[..]))]
#[case::char_after_string(b"[core]\ncommentString = %\ncommentChar = \";\"\n", Some(&b";"[..]))]
#[case::multi_byte_char(b"[core]\ncommentChar = ab\n", Some(&b"ab"[..]))]
#[case::auto(b"[core]\ncommentChar = auto\n", None)]
#[case::auto_string(b"[core]\ncommentString = auto\n", None)]
fn configured_prefix(#[case] config: &[u8], #[case] expected: Option<&[u8]>) {
    let config = Config::parse(config).unwrap();
    let prefix = CommentPrefix::from_config(&config).unwrap();
    assert_eq!(prefix.as_ref().map(CommentPrefix::as_bytes), expected);
}

#[rstest]
#[case::implicit(b"[core]\ncommentChar\n")]
#[case::empty(b"[core]\ncommentString =\n")]
#[case::newline(b"[core]\ncommentChar = \"a\\nb\"\n")]
#[case::earlier_empty(b"[core]\ncommentChar =\ncommentString = %\n")]
#[case::earlier_implicit(b"[core]\ncommentString\ncommentChar = %\n")]
fn configured_prefix_rejects(#[case] config: &[u8]) {
    let config = Config::parse(config).unwrap();
    assert!(CommentPrefix::from_config(&config).is_err());
}

#[rstest]
#[case::none(b"x\n", b"#")]
#[case::hash(b"#z\nx\n", b";")]
#[case::two(b"#z\n;z\n", b"@")]
#[case::indented(b" #z\n", b"#")]
fn auto_prefix(#[case] message: &[u8], #[case] expected: &[u8]) {
    assert_eq!(CommentPrefix::auto(message).unwrap().as_bytes(), expected);
}

#[test]
fn auto_prefix_exhausted() {
    assert!(CommentPrefix::auto(b"#\n;\n@\n!\n$\n%\n^\n&\n|\n:\nx\n").is_none());
}

#[rstest]
#[case::command(b"[trailer \"fix\"]\ncommand = echo\n")]
#[case::cmd(b"[trailer \"fix\"]\ncmd = echo\n")]
fn trailer_commands_are_refused(#[case] config: &[u8]) {
    let config = Config::parse(config).unwrap();
    let error = super::TrailerConfig::from_config(&config).unwrap_err();
    assert_eq!(error.name, "fix");
}

#[rstest]
#[case::colon(b"Key: value", Some((&b"Key"[..], &b"value"[..])))]
#[case::equals(b" Key = value ", Some((&b"Key"[..], &b"value"[..])))]
#[case::no_separator(b"Key", Some((&b"Key"[..], &b""[..])))]
#[case::empty_key(b" : value", None)]
fn trailer_arguments(#[case] argument: &[u8], #[case] expected: Option<(&[u8], &[u8])>) {
    let config = super::TrailerConfig::default();
    let trailer = super::Trailer::parse_argument(argument, &config);
    assert_eq!(
        trailer
            .as_ref()
            .map(|t| (t.key.as_slice(), t.value.as_slice())),
        expected
    );
}

/// Git 2.56 stopped reading `key://…` lines as trailers; earlier versions print
/// `http: //example.com`. girt follows 2.56, which the live comparison skips on older Git.
#[test]
fn url_lines_are_not_trailers() {
    let config = super::TrailerConfig::default();
    let message = b"subject\n\nhttp://example.com\n";
    assert!(super::trailers(message, &config).is_empty());
    let new = super::Trailer::parse_argument(b"K: v", &config).unwrap();
    assert_eq!(
        super::add_trailers(message, &[new], &config),
        b"subject\n\nhttp://example.com\n\nK: v\n"
    );
}
