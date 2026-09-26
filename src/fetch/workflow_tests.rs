use rstest::rstest;

use super::*;
use crate::InitKind;
use crate::fetch::AdvertisedRef;

fn name(value: &str) -> RefName {
    RefName::new(value).unwrap()
}
fn id(byte: u8) -> ObjectId {
    ObjectId::Sha1([byte; 20])
}
fn request(specs: &[&str]) -> (tempfile::TempDir, FetchRequest) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        crate::ObjectFormat::Sha1,
        root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    let specs =
        Refspecs::parse(Direction::Fetch, specs.iter().map(|spec| spec.as_bytes())).unwrap();
    let request = FetchRequest::prepare(repo, specs, BTreeSet::new(), Reflog::Preserve).unwrap();
    (root, request)
}
fn advertisement() -> Advertisement {
    Advertisement {
        refs: vec![
            AdvertisedRef {
                name: name("refs/heads/main"),
                id: id(1),
                peeled: false,
            },
            AdvertisedRef {
                name: name("refs/heads/private"),
                id: id(2),
                peeled: false,
            },
        ],
        capabilities: vec![],
    }
}

#[test]
fn wildcard_exclusion_and_source_only_keep_advertised_ids() {
    let (_root, request) = request(&[
        "refs/heads/*:refs/remotes/origin/*",
        "^refs/heads/private",
        "refs/heads/main",
    ]);
    let plan = request.plan(&advertisement()).unwrap();
    assert_eq!(plan.len(), 2);
    assert_eq!(
        plan[0].mapping.destination,
        Some(name("refs/remotes/origin/main"))
    );
    assert_eq!(plan[0].kind, FetchUpdateKind::Create);
    assert_eq!(plan[1].kind, FetchUpdateKind::SourceOnly);
    assert_eq!(plan[1].mapping.source.as_ref().unwrap().id, id(1));
}

#[rstest]
#[case::branch("refs/heads/main")]
#[case::private("refs/worktree/main")]
#[case::other("refs/custom/main")]
fn rejects_unsupported_destinations(#[case] destination: &str) {
    let spec = format!("+refs/heads/main:{destination}");
    let (_root, request) = request(&[&spec]);
    assert!(matches!(
        request.plan(&advertisement()),
        Err(FetchPlanError::Destination(_))
    ));
}

#[rstest]
#[case::create(None, false, false, FetchUpdateKind::Create)]
#[case::same(Some(id(2)), false, false, FetchUpdateKind::Unchanged)]
#[case::authorized(Some(id(1)), true, true, FetchUpdateKind::ForcedTag)]
fn tag_decisions(
    #[case] old: Option<ObjectId>,
    #[case] force: bool,
    #[case] authorized: bool,
    #[case] expected: FetchUpdateKind,
) {
    assert_eq!(
        update_kind(&name("refs/tags/v1"), old, id(2), force, authorized).unwrap(),
        expected
    );
}

#[rstest]
#[case::neither(false, false)]
#[case::syntax_only(true, false)]
#[case::authorization_only(false, true)]
fn tag_replacement_requires_both(#[case] force: bool, #[case] authorized: bool) {
    assert!(matches!(
        update_kind(&name("refs/tags/v1"), Some(id(1)), id(2), force, authorized),
        Err(FetchPlanError::TagReplacement(_))
    ));
}

#[test]
fn remote_tracking_replacement_defers_object_checks() {
    assert_eq!(
        update_kind(
            &name("refs/remotes/origin/main"),
            Some(id(1)),
            id(2),
            false,
            false
        )
        .unwrap(),
        FetchUpdateKind::Replace
    );
}

#[test]
fn symbolic_destination_cannot_redirect_to_branch() {
    let (_root, mut request) = request(&["refs/heads/main:refs/remotes/origin/main"]);
    request.snapshot.insert(
        name("refs/remotes/origin/main"),
        Target::Symbolic(name("refs/heads/main")),
    );
    assert!(matches!(
        request.plan(&advertisement()),
        Err(FetchPlanError::Symbolic(_))
    ));
}

#[test]
fn changed_advertisement_replans_mapping() {
    let (_root, request) = request(&["refs/heads/main:refs/remotes/origin/main"]);
    let preview = request.plan(&advertisement()).unwrap();
    let mut current = advertisement();
    current.refs[0].id = id(3);
    let actual = request.plan(&current).unwrap();
    assert_eq!(preview[0].mapping.source.as_ref().unwrap().id, id(1));
    assert_eq!(actual[0].mapping.source.as_ref().unwrap().id, id(3));
}

#[test]
fn mapping_error_selects_nothing_and_preserves_error() {
    let mut saved = None;
    let wants = select(
        Err(FetchPlanError::Destination(name("refs/heads/main"))),
        &mut saved,
    );
    let transfer: Result<(), FetchError> = Err(FetchError::Protocol("later error"));
    assert!(wants.is_empty());
    assert!(matches!(
        selected(saved, &transfer),
        Err(FetchWorkflowError::Plan(FetchPlanError::Destination(_)))
    ));
}

#[test]
fn empty_refspec_list_is_a_noop_plan() {
    let (_root, request) = request(&[]);
    assert!(request.plan(&advertisement()).unwrap().is_empty());
}

#[test]
fn mismatched_known_shallow_roots_fail_before_transfer() {
    let (_root, request) = request(&["refs/heads/main:refs/remotes/origin/main"]);
    let mut known = KnownHistory::default();
    known.shallow.push(id(3));
    assert!(matches!(
        request.check_known(&known),
        Err(FetchError::Unsupported(
            "known shallow boundaries differ from destination"
        ))
    ));
}
