//! Attribute/configuration fixtures are original and Git commands isolate global/system config.
#[path = "support/rewrite_git.rs"]
mod fixture;

use std::sync::atomic::AtomicBool;

use fixture::{Fixture, git};
use girt::Config;
use girt::attributes::{Attributes, Case, Limits, Source, State};
use girt::merge::{
    ConflictStyle, ContentMerge, ContentOptions, Driver, DriverError, MarkerOptions,
};
use rstest::rstest;

#[rstest]
#[case::set(b"file merge\n", Driver::Text, b"set".as_slice())]
#[case::unset(b"file -merge\n", Driver::Binary, b"unset".as_slice())]
#[case::unspecified(b"file !merge\n", Driver::Automatic, b"unspecified".as_slice())]
#[case::text(b"file merge=text\n", Driver::Text, b"text".as_slice())]
#[case::binary(b"file merge=binary\n", Driver::Binary, b"binary".as_slice())]
#[case::union(b"file merge=union\n", Driver::Union, b"union".as_slice())]
#[case::custom(b"file merge=custom\n", Driver::Custom(b"custom"), b"custom".as_slice())]
fn selects_the_driver_git_attributes_report(
    #[case] rules: &[u8],
    #[case] expected: Driver<'_>,
    #[case] git_value: &[u8],
) {
    let f = Fixture::new("sha1");
    let attr = f.blob(rules);
    let file = f.blob(b"payload\n");
    let tree = f.tree(&[
        ("100644", attr, b".gitattributes"),
        ("100644", file, b"file"),
    ]);
    git(f.root.path(), &["read-tree", &tree.to_string()], b"");
    let output = git(
        f.root.path(),
        &["check-attr", "--cached", "merge", "--", "file"],
        b"",
    );
    let mut expected_output = b"file: merge: ".to_vec();
    expected_output.extend_from_slice(git_value);
    expected_output.push(b'\n');
    assert_eq!(output, expected_output);
    let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
    attributes.add(Source::Directory(b""), rules).unwrap();
    assert_eq!(
        Driver::from_attribute(attributes.get(b"file", b"merge").unwrap()),
        expected
    );
    assert_eq!(f.objects().read_tree(tree).unwrap().entries().len(), 2);
}

#[rstest]
#[case::default(b"", ConflictStyle::Merge)]
#[case::merge(b"[merge]\nconflictStyle=merge\n", ConflictStyle::Merge)]
#[case::diff3(b"[merge]\nconflictStyle=diff3\n", ConflictStyle::Diff3)]
#[case::zdiff3(b"[merge]\nconflictStyle=zdiff3\n", ConflictStyle::Zdiff3)]
#[case::last_wins(
    b"[merge]\nconflictStyle=diff3\nconflictStyle=merge\n",
    ConflictStyle::Merge
)]
fn configured_styles(#[case] bytes: &[u8], #[case] expected: ConflictStyle) {
    let config = Config::parse(bytes).unwrap();
    assert_eq!(MarkerOptions::from_config(&config).unwrap().style, expected);
}

#[rstest]
#[case::implicit(b"[merge]\nconflictStyle\n")]
#[case::empty(b"[merge]\nconflictStyle=\n")]
#[case::case_sensitive(b"[merge]\nconflictStyle=Diff3\n")]
#[case::unknown(b"[merge]\nconflictStyle=unknown\n")]
fn rejects_unknown_conflict_styles(#[case] bytes: &[u8]) {
    let config = Config::parse(bytes).unwrap();
    assert!(matches!(
        MarkerOptions::from_config(&config),
        Err(DriverError::ConflictStyle(_))
    ));
}

#[test]
fn merge_default_only_applies_to_unspecified_attributes() {
    let config = Config::parse(b"[merge]\ndefault=custom\n").unwrap();
    assert_eq!(
        Driver::configured(State::Unspecified, &config),
        Driver::Custom(b"custom")
    );
    assert_eq!(Driver::configured(State::Set, &config), Driver::Text);
    assert_eq!(Driver::configured(State::Unset, &config), Driver::Binary);
}

#[rstest]
fn text_and_union_keep_binary_conflicts(#[values(Driver::Text, Driver::Union)] driver: Driver<'_>) {
    let result = driver
        .merge(
            b"base\0\n",
            b"ours\0\n",
            b"theirs\0\n",
            ContentOptions::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(result.is_binary());
    assert!(!result.is_clean());
    assert_eq!(result.render(&MarkerOptions::default()), b"ours\0\n");
}

#[test]
fn custom_driver_requires_explicit_caller_dispatch() {
    assert!(
        matches!(Driver::Custom(b"custom").merge(b"base", b"ours", b"theirs", ContentOptions::default(), &AtomicBool::new(false)), Err(DriverError::CustomRequired(name)) if name == b"custom")
    );
    let resolved = ContentMerge::resolved(b"custom output".to_vec());
    assert!(resolved.is_clean());
    assert_eq!(resolved.render(&MarkerOptions::default()), b"custom output");
    let unresolved = ContentMerge::conflicted(b"b".to_vec(), b"o".to_vec(), b"t".to_vec(), true);
    assert!(!unresolved.is_clean());
    assert_eq!(unresolved.render(&MarkerOptions::default()), b"o");
}
