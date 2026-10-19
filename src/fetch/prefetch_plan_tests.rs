//! Pure maintenance planning; the public fetch planner keeps its namespace policy.
use super::*;
use crate::fetch::AdvertisedRef;
use crate::remote::MappingLimits;

fn name(value: &str) -> RefName {
    RefName::new(value).unwrap()
}

fn id(byte: u8) -> ObjectId {
    ObjectId::Sha1([byte; 20])
}

fn request(specs: &[&str]) -> (tempfile::TempDir, FetchRequest) {
    let root = tempfile::tempdir().unwrap();
    let repository = Repository::init(
        crate::ObjectFormat::Sha1,
        root.path().join("r.git"),
        crate::InitKind::Bare,
    )
    .unwrap();
    let specs = Refspecs::parse(Direction::Fetch, specs).unwrap();
    let request =
        FetchRequest::prepare(repository, specs, BTreeSet::new(), Reflog::Preserve).unwrap();
    (root, request)
}

fn advertisement() -> Advertisement {
    Advertisement::new(
        vec![AdvertisedRef::new(
            name("refs/heads/main"),
            id(2),
            super::super::AdvertisementKind::Tip,
        )],
        Vec::new(),
    )
}

#[test]
fn prefetch_replacement_needs_no_force_but_generic_planning_still_refuses() {
    let (_root, mut request) = request(&["refs/heads/*:refs/prefetch/remotes/origin/*"]);
    let destination = name("refs/prefetch/remotes/origin/main");
    request
        .snapshot
        .insert(destination.clone(), Target::Direct(id(1)));
    let updates = request
        .plan_prefetch_controlled(
            &advertisement(),
            PrefetchPlanLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].kind, FetchUpdateKind::PrefetchReplace);
    assert_eq!(updates[0].mapping.destination, Some(destination));
    assert_eq!(updates[0].previous, Some(id(1)));
    assert!(matches!(
        request.plan(&advertisement()),
        Err(FetchPlanError::Destination(_))
    ));
}

#[rstest::rstest]
#[case("refs/heads/main")]
#[case("refs/remotes/origin/main")]
#[case("refs/tags/v1")]
#[case("refs/prefetch")]
#[case("refs/prefetch-escape/main")]
fn maintenance_mapping_cannot_escape_prefetch(#[case] destination: &str) {
    let spec = format!("refs/heads/main:{destination}");
    let (_root, request) = request(&[&spec]);
    assert!(matches!(
        request.plan_prefetch_controlled(
            &advertisement(),
            PrefetchPlanLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(FetchPlanError::Destination(_))
    ));
}

#[test]
fn pruning_keeps_excluded_symbolic_and_outside_destinations() {
    let (_root, mut request) = request(&[
        "refs/heads/*:refs/prefetch/remotes/origin/*",
        "^refs/heads/private",
    ]);
    request.prune = true;
    for suffix in ["main", "stale", "private"] {
        request.snapshot.insert(
            name(&format!("refs/prefetch/remotes/origin/{suffix}")),
            Target::Direct(id(1)),
        );
    }
    request
        .snapshot
        .insert(name("refs/remotes/origin/untouched"), Target::Direct(id(1)));
    request.snapshot.insert(
        name("refs/prefetch/remotes/origin/HEAD"),
        Target::Symbolic(name("refs/prefetch/remotes/origin/main")),
    );
    let updates = request
        .plan_prefetch_controlled(
            &advertisement(),
            PrefetchPlanLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0].kind, FetchUpdateKind::PrefetchReplace);
    assert_eq!(updates[1].kind, FetchUpdateKind::Prune);
    assert_eq!(
        updates[1].mapping.destination,
        Some(name("refs/prefetch/remotes/origin/stale"))
    );
}

#[test]
fn cancelled_planning_precedes_mapping_validation() {
    let (_root, request) = request(&["refs/heads/missing:refs/prefetch/missing"]);
    assert!(matches!(
        request.plan_prefetch_controlled(
            &advertisement(),
            PrefetchPlanLimits::default(),
            &AtomicBool::new(true)
        ),
        Err(FetchPlanError::Mapping(
            crate::remote::MappingError::Cancelled
        ))
    ));
}

#[test]
fn mapping_limits_still_apply_inside_maintenance() {
    let (_root, request) = request(&["refs/heads/*:refs/prefetch/*"]);
    let limits = PrefetchPlanLimits {
        mapping: MappingLimits::default().with_max_output_bytes(0),
        ..PrefetchPlanLimits::default()
    };
    assert!(matches!(
        request.plan_prefetch_controlled(&advertisement(), limits, &AtomicBool::new(false)),
        Err(FetchPlanError::Mapping(crate::remote::MappingError::Limit(
            _
        )))
    ));
}
