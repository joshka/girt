use rstest::rstest;

use super::*;

fn source(name: &[u8]) -> RefSource {
    RefSource {
        name: RefName::new(name).unwrap(),
        id: ObjectId::for_blob(b"tip"),
    }
}
fn specs(direction: Direction, values: &[&[u8]]) -> Refspecs {
    Refspecs::parse(direction, values).unwrap()
}

#[rstest]
#[case::fetch_exact(Direction::Fetch, b"refs/heads/main:refs/remotes/o/main", false, false)]
#[case::force(Direction::Fetch, b"+refs/heads/main:refs/remotes/o/main", true, false)]
#[case::fetch_only(Direction::Fetch, b"HEAD", false, false)]
#[case::empty_fetch_destination(Direction::Fetch, b"HEAD:", false, false)]
#[case::negative(Direction::Fetch, b"^refs/heads/private/*", false, true)]
#[case::negative_head(Direction::Fetch, b"^HEAD", false, true)]
#[case::partial_glob(
    Direction::Fetch,
    b"refs/heads/pre*post:refs/remotes/o/*",
    false,
    false
)]
#[case::push_same(Direction::Push, b"refs/heads/main", false, false)]
#[case::push_glob(Direction::Push, b"refs/heads/*:refs/heads/*", false, false)]
#[case::push_head(Direction::Push, b"+HEAD:refs/heads/main", true, false)]
#[case::delete(Direction::Push, b":refs/heads/old", false, false)]
#[case::force_delete(Direction::Push, b"+:refs/heads/old", true, false)]
#[case::bytes(Direction::Fetch, b"refs/heads/\xff:refs/remotes/\xfe", false, false)]
fn parses_supported(
    #[case] direction: Direction,
    #[case] bytes: &[u8],
    #[case] force: bool,
    #[case] negative: bool,
) {
    let spec = Refspec::parse(direction, bytes).unwrap();
    assert_eq!(spec.as_bytes(), bytes);
    assert_eq!(spec.force(), force);
    assert_eq!(spec.is_negative(), negative);
}

#[rstest]
#[case::empty(Direction::Fetch, b"")]
#[case::matching(Direction::Push, b":")]
#[case::force_matching(Direction::Push, b"+:")]
#[case::short_source(Direction::Fetch, b"main:refs/remotes/o/main")]
#[case::short_destination(Direction::Push, b"HEAD:main")]
#[case::head_destination(Direction::Fetch, b"refs/heads/main:HEAD")]
#[case::push_head_inference(Direction::Push, b"HEAD")]
#[case::revision(Direction::Push, b"HEAD~1:refs/heads/old")]
#[case::oid(
    Direction::Fetch,
    b"0123456789012345678901234567890123456789:refs/heads/main"
)]
#[case::tag_shorthand(Direction::Fetch, b"tag v1")]
#[case::empty_source(Direction::Fetch, b":refs/heads/main")]
#[case::empty_destination(Direction::Push, b"refs/heads/main:")]
fn rejects_unsupported(#[case] direction: Direction, #[case] bytes: &[u8]) {
    assert!(matches!(
        Refspec::parse(direction, bytes),
        Err(RefspecError::Unsupported(_))
    ));
}

#[rstest]
#[case::negative_push(Direction::Push, b"^refs/heads/main")]
#[case::negative_force(Direction::Fetch, b"+^refs/heads/main")]
#[case::negative_destination(Direction::Fetch, b"^refs/heads/main:refs/heads/main")]
#[case::extra_colon(Direction::Fetch, b"refs/a:refs/b:refs/c")]
#[case::one_sided_glob(Direction::Fetch, b"refs/heads/*:refs/remotes/o/main")]
#[case::destination_glob(Direction::Push, b"refs/heads/main:refs/heads/*")]
#[case::many_globs(Direction::Fetch, b"refs/**:refs/**")]
#[case::wildcard_selection(Direction::Fetch, b"refs/heads/*")]
#[case::wildcard_deletion(Direction::Push, b":refs/heads/*")]
fn rejects_syntax(#[case] direction: Direction, #[case] bytes: &[u8]) {
    assert!(matches!(
        Refspec::parse(direction, bytes),
        Err(RefspecError::Syntax(_))
    ));
}

#[rstest]
#[case::dotdot(b"refs/heads/a..b:refs/heads/main")]
#[case::reflog(b"refs/heads/a@{1}:refs/heads/main")]
#[case::space(b"refs/heads/a b:refs/heads/main")]
#[case::nul(b"refs/heads/a\0b:refs/heads/main")]
#[case::lock(b"refs/heads/*.lock:refs/heads/*")]
#[case::slash(b"refs/heads//a:refs/heads/main")]
fn rejects_invalid_names(#[case] bytes: &[u8]) {
    assert_eq!(
        Refspec::parse(Direction::Fetch, bytes),
        Err(RefspecError::Name)
    );
}

#[rstest]
#[case::nested(
    b"refs/heads/*:refs/remotes/o/*",
    b"refs/heads/a/b",
    b"refs/remotes/o/a/b"
)]
#[case::empty_capture(
    b"refs/heads/pre*post:refs/tags/x*y",
    b"refs/heads/prepost",
    b"refs/tags/xy"
)]
#[case::suffix(
    b"refs/heads/pre*post:refs/tags/*",
    b"refs/heads/pre-middle-post",
    b"refs/tags/-middle-"
)]
#[case::bytes(
    b"refs/heads/*:refs/remotes/o/*",
    b"refs/heads/\xff",
    b"refs/remotes/o/\xff"
)]
fn wildcard_substitution(#[case] spec: &[u8], #[case] name: &[u8], #[case] destination: &[u8]) {
    let plan = specs(Direction::Fetch, &[spec])
        .map(&[source(name)])
        .unwrap();
    assert_eq!(
        plan[0].destination.as_ref().unwrap().as_bytes(),
        destination
    );
    assert_eq!(plan[0].source.as_ref().unwrap().name.as_bytes(), name);
}

#[rstest]
#[case::before(vec![b"^refs/heads/private/*".as_slice(), b"refs/heads/*:refs/remotes/o/*"])]
#[case::after(vec![b"refs/heads/*:refs/remotes/o/*".as_slice(), b"^refs/heads/private/*"])]
fn exclusions_are_global(#[case] values: Vec<&[u8]>) {
    let inputs = [
        source(b"refs/heads/z"),
        source(b"refs/heads/private/a"),
        source(b"refs/heads/a"),
    ];
    let plan = specs(Direction::Fetch, &values).map(&inputs).unwrap();
    assert_eq!(
        plan.iter()
            .map(|m| m.destination.as_ref().unwrap().as_bytes())
            .collect::<Vec<_>>(),
        vec![b"refs/remotes/o/z".as_slice(), b"refs/remotes/o/a"]
    );
}

#[test]
fn exact_duplicates_collapse_and_spec_order_precedes_source_order() {
    let values = [
        b"refs/heads/b:refs/tags/b".as_slice(),
        b"+refs/heads/*:refs/remotes/o/*",
        b"refs/heads/b:refs/tags/b",
    ];
    let inputs = [source(b"refs/heads/a"), source(b"refs/heads/b")];
    let plan = specs(Direction::Fetch, &values).map(&inputs).unwrap();
    assert_eq!(
        plan.iter()
            .map(|m| m.destination.as_ref().unwrap().as_bytes())
            .collect::<Vec<_>>(),
        vec![
            b"refs/tags/b".as_slice(),
            b"refs/remotes/o/a",
            b"refs/remotes/o/b"
        ]
    );
    assert_eq!(
        plan.iter().map(|m| m.force).collect::<Vec<_>>(),
        vec![false, true, true]
    );
}

#[rstest]
#[case::different_sources(b"refs/heads/b:refs/tags/t")]
#[case::different_force(b"+refs/heads/a:refs/tags/t")]
#[case::deletion(b":refs/tags/t")]
fn rejects_destination_collisions(#[case] second: &[u8]) {
    let inputs = [source(b"refs/heads/a"), source(b"refs/heads/b")];
    let result = specs(Direction::Push, &[b"refs/heads/a:refs/tags/t", second]).map(&inputs);
    assert_eq!(
        result,
        Err(MappingError::Collision(
            RefName::new(b"refs/tags/t").unwrap()
        ))
    );
}

#[rstest]
#[case::explicit(vec![b"refs/heads/missing".as_slice()])]
#[case::excluded_missing(vec![b"refs/heads/missing".as_slice(), b"^refs/heads/missing"])]
fn missing_explicit_sources_fail(#[case] values: Vec<&[u8]>) {
    assert_eq!(
        specs(Direction::Fetch, &values).map(&[]),
        Err(MappingError::UnmatchedSource(
            b"refs/heads/missing".to_vec()
        ))
    );
}

#[rstest]
#[case::wildcard(vec![b"refs/heads/*:refs/remotes/o/*".as_slice()])]
#[case::negative_only(vec![b"^refs/heads/missing".as_slice()])]
#[case::empty(vec![])]
fn absent_patterns_produce_empty_plan(#[case] values: Vec<&[u8]>) {
    assert!(
        specs(Direction::Fetch, &values)
            .map(&[])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn deletion_and_source_only_forms() {
    let input = source(b"refs/heads/main");
    let deleted = specs(Direction::Push, &[b":refs/heads/old"])
        .map(&[])
        .unwrap();
    assert_eq!(deleted[0].source, None);
    assert_eq!(
        deleted[0].destination.as_ref().unwrap().as_bytes(),
        b"refs/heads/old"
    );
    let pushed = specs(Direction::Push, &[b"refs/heads/main"])
        .map(std::slice::from_ref(&input))
        .unwrap();
    assert_eq!(pushed[0].destination, Some(input.name.clone()));
    let fetched = specs(Direction::Fetch, &[b"refs/heads/main", b"refs/heads/main:"])
        .map(&[input])
        .unwrap();
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].destination, None);
}

#[test]
fn rejects_ambiguous_inputs_and_zero_ids_even_if_unselected() {
    let input = source(b"HEAD");
    let empty = specs(Direction::Fetch, &[]);
    assert_eq!(
        empty.map(&[input.clone(), input.clone()]),
        Err(MappingError::DuplicateSource(input.name.clone()))
    );
    let zero = RefSource {
        id: ObjectId::from_bytes([0; 20]),
        ..input
    };
    assert_eq!(
        empty.map(std::slice::from_ref(&zero)),
        Err(MappingError::ZeroId(zero.name))
    );
}

#[test]
fn rejects_invalid_substitution() {
    let plan = specs(Direction::Fetch, &[b"refs/heads/p*post:refs/remotes/o/*"])
        .map(&[source(b"refs/heads/ppost")]);
    assert_eq!(plan, Err(MappingError::InvalidDestination));
}

#[test]
fn advertisement_uses_named_tips_not_peeling_or_symbolic_target_names() {
    use crate::fetch::AdvertisedRef;
    let tip = ObjectId::for_blob(b"tag");
    let advertisement = Advertisement {
        refs: vec![
            AdvertisedRef {
                name: RefName::new(b"HEAD").unwrap(),
                id: tip,
                peeled: false,
            },
            AdvertisedRef {
                name: RefName::new(b"refs/tags/v1").unwrap(),
                id: tip,
                peeled: false,
            },
            AdvertisedRef {
                name: RefName::new(b"refs/tags/v1").unwrap(),
                id: ObjectId::for_blob(b"peeled"),
                peeled: true,
            },
        ],
        capabilities: vec![b"symref=HEAD:refs/heads/main".to_vec()],
    };
    let plan = specs(Direction::Fetch, &[b"HEAD", b"refs/tags/*:refs/tags/*"])
        .map_advertisement(&advertisement)
        .unwrap();
    assert_eq!(plan.len(), 2);
    assert_eq!(plan[0].source.as_ref().unwrap().name.as_bytes(), b"HEAD");
    assert_eq!(plan[1].source.as_ref().unwrap().id, tip);
    assert_eq!(
        specs(Direction::Push, &[]).map_advertisement(&advertisement),
        Err(MappingError::Direction)
    );
}
