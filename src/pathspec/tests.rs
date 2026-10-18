use rstest::rstest;

use super::*;
use crate::attributes::State;

fn spec(specs: &[&str]) -> Pathspec {
    Pathspec::parse(specs, &Context::new()).unwrap()
}

#[rstest]
#[case::current("a.rs", b"src", b"src/a.rs")]
#[case::parent("../tests", b"src", b"tests")]
#[case::dot("./", b"src", b"src/")]
#[case::top(":/README", b"src", b"README")]
#[case::top_long(":(top)", b"src/sub", b"")]
fn resolves_against_prefix(#[case] raw: &str, #[case] prefix: &[u8], #[case] pattern: &[u8]) {
    let context = Context::new().with_prefix(prefix);
    assert_eq!(
        Pathspec::parse([raw], &context).unwrap().includes[0].pattern,
        pattern
    );
}

#[rstest]
#[case::trailing_dot("src/.", b"src")]
#[case::directory("src/", b"src/")]
#[case::parent("sub/deep/..", b"sub")]
#[case::root("./", b"")]
#[case::doubled("src//a.rs", b"src/a.rs")]
#[case::inner_parent("src/../docs", b"docs")]
fn normalized_pattern(#[case] raw: &str, #[case] pattern: &[u8]) {
    assert_eq!(spec(&[raw]).includes[0].pattern, pattern);
}

#[rstest]
#[case::empty("", ParseError::Empty)]
#[case::unknown(":(bogus)x", invalid(":(bogus)x", Reason::UnknownMagic(b"bogus".to_vec())))]
#[case::case_sensitive(":(GLOB)x", invalid(":(GLOB)x", Reason::UnknownMagic(b"GLOB".to_vec())))]
#[case::spaces(":( glob )x", invalid(":( glob )x", Reason::UnknownMagic(b" glob ".to_vec())))]
#[case::unclosed(":(top", invalid(":(top", Reason::UnclosedMagic))]
#[case::incompatible(
    ":(glob,literal)x",
    invalid(":(glob,literal)x", Reason::GlobWithLiteral)
)]
#[case::escapes("../..", invalid("../..", Reason::OutsideRepository))]
#[case::absolute(":!:/x", invalid(":!:/x", Reason::OutsideRepository))]
#[case::empty_attr(":(attr:)", invalid(":(attr:)", Reason::EmptyAttr))]
#[case::two_attr(":(attr:a,attr:b)", invalid(":(attr:a,attr:b)", Reason::MultipleAttr))]
#[case::attr_name(":(attr:b@d)", invalid(":(attr:b@d)", Reason::InvalidAttrName(b"b@d".to_vec())))]
#[case::attr_escape(r":(attr:a=b\ x)", invalid(r":(attr:a=b\ x)", Reason::TrailingEscape))]
fn rejects(#[case] raw: &str, #[case] expected: ParseError) {
    assert_eq!(Pathspec::parse([raw], &Context::new()), Err(expected));
}

fn invalid(spec: &str, reason: Reason) -> ParseError {
    ParseError::Invalid {
        spec: spec.as_bytes().to_vec(),
        reason,
    }
}

#[test]
fn environment_booleans() {
    let read = |pairs: &[(&str, &str)]| {
        let environment = pairs
            .iter()
            .fold(Environment::default(), |environment, (name, value)| {
                environment.with(*name, *value)
            });
        Context::from_environment(&environment)
    };
    assert_eq!(
        read(&[("GIT_GLOB_PATHSPECS", "on")]),
        Ok(Context::new().with_glob(true))
    );
    assert_eq!(read(&[("GIT_ICASE_PATHSPECS", "0")]), Ok(Context::new()));
    assert_eq!(
        read(&[("GIT_GLOB_PATHSPECS", "1"), ("GIT_NOGLOB_PATHSPECS", "1")]),
        Err(ParseError::GlobAndNoglob)
    );
    assert!(matches!(
        read(&[("GIT_LITERAL_PATHSPECS", "bogus")]),
        Err(ParseError::Environment {
            variable: "GIT_LITERAL_PATHSPECS",
            ..
        })
    ));
}

#[rstest]
#[case::unrelated(&["src"], b"tests", Prune::Skip)]
#[case::named(&["src"], b"src", Prune::All)]
#[case::below(&["src"], b"src/sub", Prune::All)]
#[case::above(&["src/sub/a.rs"], b"src", Prune::Descend)]
#[case::component_boundary(&["sr"], b"src", Prune::Skip)]
#[case::wildcard_anywhere(&["*.rs"], b"tests", Prune::Descend)]
#[case::wildcard_prefix(&["src/*.rs"], b"tests", Prune::Skip)]
#[case::wildcard_inside(&["src/*.rs"], b"src/sub", Prune::Descend)]
#[case::excluded(&["src", ":!src/gen"], b"src/gen", Prune::Skip)]
#[case::partly_excluded(&["src", ":!src/gen"], b"src", Prune::Descend)]
#[case::exclude_only(&[":!src"], b"docs", Prune::All)]
#[case::icase(&[":(icase)SRC"], b"src", Prune::All)]
#[case::root(&["src"], b"", Prune::Descend)]
fn prunes(#[case] specs: &[&str], #[case] directory: &[u8], #[case] expected: Prune) {
    assert_eq!(spec(specs).directory(directory), expected);
}

#[test]
fn empty_list_selects_everything() {
    let all = Pathspec::all();
    assert!(all.is_all() && all.matches(b"any/path").unwrap());
    assert_eq!(all.directory(b"any"), Prune::All);
    assert!(spec(&["."]).is_all());
    assert!(!spec(&[".", ":!x"]).is_all());
}

#[rstest]
#[case::set("a", State::Set, true)]
#[case::set_rejects_value("a", State::Value(b"x"), false)]
#[case::unset("-a", State::Unset, true)]
#[case::unspecified("!a", State::Unspecified, true)]
#[case::value("a=x", State::Value(b"x"), true)]
#[case::other_value("a=x", State::Value(b"y"), false)]
#[case::empty_value("a=", State::Value(b""), true)]
#[case::escaped_comma(r"a=b\,c", State::Value(b"b,c"), true)]
fn attribute_requirements(#[case] word: &str, #[case] state: State<'_>, #[case] expected: bool) {
    let parsed = spec(&[&format!(":(attr:{word})")]);
    assert!(parsed.uses_attributes());
    let accepted = parsed
        .matches_with(b"x", |requirement| Ok::<_, ()>(requirement.accepts(state)))
        .unwrap();
    assert_eq!(accepted, expected);
}

#[test]
fn attributes_never_take_whole_directories() {
    assert_eq!(spec(&[":(attr:a)src"]).directory(b"src"), Prune::Descend);
    assert_eq!(spec(&[":(attr:a)src"]).directory(b"docs"), Prune::Skip);
    assert_eq!(
        spec(&[":(attr:a,exclude)src"]).directory(b"src"),
        Prune::Descend
    );
}

#[test]
fn exclude_only_lists_start_from_prefix_or_root() {
    let context = Context::new().with_prefix(b"src");
    let local = Pathspec::parse([":!gen"], &context).unwrap();
    assert!(local.matches(b"src/a.rs").unwrap() && !local.matches(b"README").unwrap());
    let whole = Pathspec::parse([":!gen"], &context.with_excludes_from_root(true)).unwrap();
    assert!(whole.matches(b"src/a.rs").unwrap() && whole.matches(b"README").unwrap());
    assert!(!whole.matches(b"src/gen/out.rs").unwrap());
}
