//! Pure refspec mapping refuses bounded work without returning a partial plan.
use std::sync::atomic::AtomicBool;

use girt::refs::RefName;
use girt::remote::{Direction, MappingError, MappingLimits, MappingOptions, RefSource, Refspecs};
use girt::{ObjectFormat, ObjectKind};

fn sources(format: ObjectFormat) -> Vec<RefSource> {
    ["refs/heads/main", "refs/heads/wip/topic", "refs/tags/v1"]
        .into_iter()
        .map(|name| RefSource {
            name: RefName::new(name).unwrap(),
            id: format.hash_object(ObjectKind::Blob, name.as_bytes()),
        })
        .collect()
}

fn specs() -> Refspecs {
    Refspecs::parse(
        Direction::Fetch,
        [
            "+refs/heads/*:refs/remotes/origin/*",
            "^refs/heads/wip/*",
            "refs/heads/gone:refs/remotes/origin/gone",
        ],
    )
    .unwrap()
}

#[test]
fn controlled_mapping_matches_trusted_results() {
    let specs = specs();
    let cancel = AtomicBool::new(false);
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let sources = sources(format);
        let expected = specs.map_with_missing(&sources).unwrap();
        assert_eq!(
            MappingOptions::new(&specs)
                .map_with_missing(&sources, &cancel)
                .unwrap(),
            expected
        );
        assert!(matches!(
            MappingOptions::new(&specs).map(&sources, &cancel),
            Err(MappingError::UnmatchedSource(_))
        ));
    }
}

#[test]
fn input_limits_refuse_before_mapping() {
    let specs = specs();
    let sources = sources(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    for limits in [
        MappingLimits::default().with_max_sources(2),
        MappingLimits::default().with_max_specs(2),
        MappingLimits::default().with_max_input_bytes(1),
    ] {
        assert!(matches!(
            MappingOptions::new(&specs)
                .with_limits(limits)
                .map_with_missing(&sources, &cancel),
            Err(MappingError::Limit(_))
        ));
    }
}

#[test]
fn output_and_comparison_limits_refuse_whole_plan() {
    let specs = specs();
    let sources = sources(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    for limits in [
        MappingLimits::default().with_max_mappings(0),
        MappingLimits::default().with_max_output_bytes(1),
        MappingLimits::default().with_max_work(0),
    ] {
        assert!(matches!(
            MappingOptions::new(&specs)
                .with_limits(limits)
                .map_with_missing(&sources, &cancel),
            Err(MappingError::Limit(_))
        ));
    }
}

#[test]
fn cancellation_precedes_input_validation() {
    let specs = specs();
    let sources = sources(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(true);
    let duplicate = [sources[0].clone(), sources[0].clone()];
    assert!(matches!(
        MappingOptions::new(&specs).map_with_missing(&duplicate, &cancel),
        Err(MappingError::Cancelled)
    ));
}

#[test]
fn unused_sources_keep_validation_and_collisions_keep_refusal() {
    let specs = specs();
    let sources = sources(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let duplicate = [sources[2].clone(), sources[2].clone()];
    assert!(matches!(
        MappingOptions::new(&specs).map_with_missing(&duplicate, &cancel),
        Err(MappingError::DuplicateSource(_))
    ));
    let collision = Refspecs::parse(
        Direction::Fetch,
        [
            "refs/heads/main:refs/remotes/origin/same",
            "refs/tags/v1:refs/remotes/origin/same",
        ],
    )
    .unwrap();
    assert!(matches!(
        MappingOptions::new(&collision).map_with_missing(&sources, &cancel),
        Err(MappingError::Collision(_))
    ));
}

#[test]
fn exact_input_bounds_admit_and_one_less_refuses() {
    let specs = specs();
    let sources = sources(ObjectFormat::Sha1);
    let bytes = specs
        .specs()
        .iter()
        .map(|spec| spec.as_bytes().len())
        .sum::<usize>()
        + sources
            .iter()
            .map(|source| source.name.as_bytes().len())
            .sum::<usize>();
    let cancel = AtomicBool::new(false);
    let limits = MappingLimits::default()
        .with_max_specs(specs.specs().len())
        .with_max_sources(sources.len())
        .with_max_input_bytes(bytes);
    assert!(
        MappingOptions::new(&specs)
            .with_limits(limits)
            .map_with_missing(&sources, &cancel)
            .is_ok()
    );
    assert!(matches!(
        MappingOptions::new(&specs)
            .with_limits(limits.with_max_input_bytes(bytes - 1))
            .map_with_missing(&sources, &cancel),
        Err(MappingError::Limit(_))
    ));
}

#[test]
fn advertisement_limits_cover_hints_before_tip_cloning() {
    use girt::fetch::{AdvertisedRef, Advertisement, AdvertisementKind};

    let sources = sources(ObjectFormat::Sha1);
    let specs = Refspecs::parse(Direction::Fetch, ["refs/tags/*:refs/tags/*"]).unwrap();
    let tip = &sources[2];
    let advertisement = Advertisement::new(
        vec![
            AdvertisedRef::new(tip.name.clone(), tip.id, AdvertisementKind::Tip),
            AdvertisedRef::new(tip.name.clone(), tip.id, AdvertisementKind::PeeledHint),
        ],
        Vec::new(),
    );
    let cancel = AtomicBool::new(false);
    assert_eq!(
        MappingOptions::new(&specs)
            .map_advertisement(&advertisement, &cancel)
            .unwrap(),
        specs.map_advertisement(&advertisement).unwrap()
    );
    assert!(matches!(
        MappingOptions::new(&specs)
            .with_limits(MappingLimits::default().with_max_sources(1))
            .map_advertisement(&advertisement, &cancel),
        Err(MappingError::Limit(_))
    ));
    let missing = MappingOptions::new(&specs)
        .map_advertisement_with_missing(&advertisement, &cancel)
        .unwrap();
    assert!(missing.1.is_empty());
    assert_eq!(missing.0.len(), 1);
}

#[test]
fn duplicate_mapping_attempts_and_missing_names_still_consume_budget() {
    let sources = sources(ObjectFormat::Sha1);
    let specs = Refspecs::parse(
        Direction::Fetch,
        ["refs/heads/main:refs/remotes/origin/main"; 3],
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    assert_eq!(specs.map(&sources).unwrap().len(), 1);
    assert!(matches!(
        MappingOptions::new(&specs)
            .with_limits(MappingLimits::default().with_max_mappings(2))
            .map(&sources, &cancel),
        Err(MappingError::Limit(_))
    ));
    let missing = Refspecs::parse(Direction::Fetch, ["refs/heads/absent"; 3]).unwrap();
    assert!(matches!(
        MappingOptions::new(&missing)
            .with_limits(MappingLimits::default().with_max_mappings(2))
            .map_with_missing(&sources, &cancel),
        Err(MappingError::Limit(_))
    ));
}

#[test]
fn output_name_budget_admits_exact_bytes() {
    let sources = sources(ObjectFormat::Sha256);
    let specs = Refspecs::parse(
        Direction::Fetch,
        ["refs/heads/main:refs/remotes/origin/main"],
    )
    .unwrap();
    let bytes = b"refs/heads/main".len() + b"refs/remotes/origin/main".len();
    let cancel = AtomicBool::new(false);
    let options = MappingOptions::new(&specs)
        .with_limits(MappingLimits::default().with_max_output_bytes(bytes));
    assert_eq!(
        options.map(&sources, &cancel).unwrap(),
        specs.map(&sources).unwrap()
    );
    assert!(matches!(
        MappingOptions::new(&specs)
            .with_limits(MappingLimits::default().with_max_output_bytes(bytes - 1))
            .map(&sources, &cancel),
        Err(MappingError::Limit(_))
    ));
}

#[test]
fn empty_input_is_valid_and_push_deletion_needs_no_source() {
    let cancel = AtomicBool::new(false);
    let empty = Refspecs::parse(Direction::Fetch, std::iter::empty::<&str>()).unwrap();
    assert!(
        MappingOptions::new(&empty)
            .map(&[], &cancel)
            .unwrap()
            .is_empty()
    );
    let deletion = Refspecs::parse(Direction::Push, [":refs/heads/old"]).unwrap();
    assert_eq!(
        MappingOptions::new(&deletion).map(&[], &cancel).unwrap(),
        deletion.map(&[]).unwrap()
    );
    assert!(matches!(
        MappingOptions::new(&deletion)
            .with_limits(MappingLimits::default().with_max_mappings(0))
            .map(&[], &cancel),
        Err(MappingError::Limit(_))
    ));
}
