use rstest::rstest;

use super::*;
use crate::fetch::AdvertisedRef;

fn name(n: &str) -> RefName {
    RefName::new(n).unwrap()
}
fn id(n: u8) -> ObjectId {
    ObjectId::Sha1([n; 20])
}
fn advertisement(refs: &[(&str, u8)], caps: &[&str]) -> Advertisement {
    Advertisement {
        refs: refs
            .iter()
            .map(|(n, i)| AdvertisedRef {
                name: name(n),
                id: id(*i),
                peeled: false,
            })
            .collect(),
        capabilities: caps.iter().map(|c| c.as_bytes().to_vec()).collect(),
    }
}

#[rstest]
#[case::symbolic(&[("HEAD", 1), ("refs/heads/main", 1)], &["symref=HEAD:refs/heads/main"], CloneHead::Branch { name: name("refs/heads/main"), id: id(1) })]
#[case::detached_equal_branches(&[("HEAD", 1), ("refs/heads/main", 1), ("refs/heads/topic", 1)], &[], CloneHead::Detached(id(1)))]
#[case::detached_only(&[("HEAD", 1)], &[], CloneHead::Detached(id(1)))]
#[case::empty(&[], &[], CloneHead::Unborn(name("refs/heads/main")))]
#[case::unborn_hint(&[], &["symref=HEAD:refs/heads/topic"], CloneHead::Unborn(name("refs/heads/topic")))]
fn default_selection(
    #[case] refs: &[(&str, u8)],
    #[case] caps: &[&str],
    #[case] expected: CloneHead,
) {
    let result = plan(&BranchSelection::Default, &advertisement(refs, caps)).unwrap();
    assert_eq!(result.head, expected);
}

#[rstest]
#[case::missing_head(&[("refs/heads/main", 1)], &[])]
#[case::mismatched_head(&[("HEAD", 1), ("refs/heads/main", 2)], &["symref=HEAD:refs/heads/main"])]
#[case::missing_target(&[("HEAD", 1)], &["symref=HEAD:refs/heads/main"])]
#[case::missing_head_id(&[("refs/heads/main", 1)], &["symref=HEAD:refs/heads/main"])]
#[case::duplicate_hint(&[], &["symref=HEAD:refs/heads/main", "symref=HEAD:refs/heads/main"])]
#[case::nonbranch_hint(&[], &["symref=HEAD:refs/tags/v1"])]
#[case::invalid_hint(&[], &["symref=HEAD:bad"])]
#[case::duplicate_ref(&[("refs/heads/main", 1), ("refs/heads/main", 2)], &[])]
fn rejects_invalid_defaults(#[case] refs: &[(&str, u8)], #[case] caps: &[&str]) {
    assert!(plan(&BranchSelection::Default, &advertisement(refs, caps)).is_err());
}

#[rstest]
#[case::existing(&[("refs/heads/topic", 2)], CloneHead::Branch { name: name("refs/heads/topic"), id: id(2) })]
#[case::empty(&[], CloneHead::Unborn(name("refs/heads/topic")))]
fn explicit_branch(#[case] refs: &[(&str, u8)], #[case] expected: CloneHead) {
    let result = plan(
        &BranchSelection::Branch(name("refs/heads/topic")),
        &advertisement(refs, &[]),
    )
    .unwrap();
    assert_eq!(result.head, expected);
}

#[test]
fn missing_selected_branch_is_not_treated_as_empty() {
    let result = plan(
        &BranchSelection::Branch(name("refs/heads/topic")),
        &advertisement(&[("refs/tags/v1", 1)], &[]),
    );
    assert!(matches!(result, Err(ClonePlanError::MissingBranch(_))));
}

#[test]
fn remaps_actual_advertisement_after_preview() {
    let first = plan(
        &BranchSelection::Default,
        &advertisement(&[("HEAD", 1)], &[]),
    )
    .unwrap();
    let second = plan(
        &BranchSelection::Default,
        &advertisement(&[("HEAD", 2)], &[]),
    )
    .unwrap();
    assert_ne!(first.head, second.head);
    assert_eq!(second.mappings[0].source.as_ref().unwrap().id, id(2));
}
