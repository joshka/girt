use rstest::rstest;

use super::*;

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
    let vars = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).into())
        }
    };
    let context = Context::from_vars(vars(&[("GIT_GLOB_PATHSPECS", "on")])).unwrap();
    assert_eq!(context, Context::new().with_glob(true));
    let context = Context::from_vars(vars(&[("GIT_ICASE_PATHSPECS", "0")])).unwrap();
    assert_eq!(context, Context::new());
    assert_eq!(
        Context::from_vars(vars(&[
            ("GIT_GLOB_PATHSPECS", "1"),
            ("GIT_NOGLOB_PATHSPECS", "1")
        ])),
        Err(ParseError::GlobAndNoglob)
    );
    assert!(matches!(
        Context::from_vars(vars(&[("GIT_LITERAL_PATHSPECS", "bogus")])),
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
    assert!(all.is_all() && all.matches(b"any/path"));
    assert_eq!(all.directory(b"any"), Prune::All);
    assert!(spec(&["."]).is_all());
    assert!(!spec(&[".", ":!x"]).is_all());
}
