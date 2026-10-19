//! Pure maintenance planning; the public fetch planner keeps its namespace policy.
use super::prefetch_plan::PrefetchPlanLimits;
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

#[rstest::rstest]
#[case::snapshot_count("prefetch snapshot entries", 0)]
#[case::snapshot_bytes("prefetch snapshot bytes", 1)]
#[case::result_count("prefetch updates", 2)]
#[case::result_bytes("prefetch output bytes", 3)]
#[case::work("prefetch planning work", 4)]
fn destination_and_result_admission(#[case] resource: &str, #[case] limit: usize) {
    let (_root, mut request) = request(&["refs/heads/*:refs/prefetch/*"]);
    request.prune = true;
    request
        .snapshot
        .insert(name("refs/prefetch/stale"), Target::Direct(id(1)));
    let mut limits = PrefetchPlanLimits::default();
    match limit {
        0 => limits.max_snapshot_entries = 0,
        1 => limits.max_snapshot_bytes = 0,
        2 => limits.max_updates = 0,
        3 => limits.max_output_bytes = 0,
        4 => limits.max_work = 0,
        _ => unreachable!(),
    }
    let empty = Advertisement::new(Vec::new(), Vec::new());
    assert!(matches!(
        request.plan_prefetch_controlled(&empty, limits, &AtomicBool::new(false)),
        Err(FetchPlanError::Mapping(crate::remote::MappingError::Limit(name))) if name == resource
    ));
}

#[test]
fn reverse_candidate_bytes_are_limited_even_with_no_forward_mappings() {
    let (_root, mut request) = request(&["refs/heads/*:refs/prefetch/*"]);
    request.prune = true;
    request
        .snapshot
        .insert(name("refs/prefetch/stale"), Target::Direct(id(1)));
    let limits = PrefetchPlanLimits {
        mapping: MappingLimits::default().with_max_output_bytes(0),
        ..PrefetchPlanLimits::default()
    };
    let empty = Advertisement::new(Vec::new(), Vec::new());
    assert!(matches!(
        request.plan_prefetch_controlled(&empty, limits, &AtomicBool::new(false)),
        Err(FetchPlanError::Mapping(crate::remote::MappingError::Limit(
            "output bytes"
        )))
    ));
}

#[test]
fn retained_missing_sources_protect_their_prefetch_destinations() {
    let (_root, mut request) = request(&["refs/heads/*:refs/prefetch/*"]);
    request.prune = true;
    request
        .snapshot
        .insert(name("refs/prefetch/stale"), Target::Direct(id(1)));
    request
        .retained_missing_sources
        .insert(b"refs/heads/stale".to_vec());
    let updates = request
        .plan_prefetch_controlled(
            &Advertisement::new(Vec::new(), Vec::new()),
            PrefetchPlanLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(updates.is_empty());
}

#[test]
fn missing_exact_source_refuses_like_git_maintenance_prefetch() {
    let (_root, request) = request(&["refs/heads/deleted:refs/prefetch/deleted"]);
    assert!(matches!(
        request.plan_prefetch_controlled(&advertisement(), PrefetchPlanLimits::default(), &AtomicBool::new(false)),
        Err(FetchPlanError::Mapping(crate::remote::MappingError::UnmatchedSource(source)))
            if source == b"refs/heads/deleted"
    ));
}
