//! Caller-owned attribute/custom driver composition, checked against isolated Git where defined.
#[path = "support/rewrite_git.rs"]
mod fixture;
#[path = "support/merge_git.rs"]
mod oracle;

use std::sync::atomic::AtomicBool;

use fixture::{Fixture, id};
use girt::attributes::{Attributes, Case, Limits, Source};
use girt::merge::{
    CommitOptions, ContentMerge, ContentOptions, Driver, RenameError, TreeDriver, TreeError,
    TreeMergeOptions,
};
use rstest::rstest;

type DriverResult = Result<ContentMerge, Box<dyn std::error::Error + Send + Sync>>;

struct AttributeDriver(Attributes);
impl TreeDriver for AttributeDriver {
    fn merge(
        &mut self,
        path: &[u8],
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        options: ContentOptions,
        cancel: &AtomicBool,
    ) -> DriverResult {
        let selected = Driver::from_attribute(self.0.get(path, b"merge")?);
        // This fixture has no custom driver definitions in its explicit configuration.
        let selected = match selected {
            Driver::Custom(_) => Driver::Text,
            built_in => built_in,
        };
        Ok(selected.merge(base, ours, theirs, options, cancel)?)
    }
}

#[rstest]
#[case::union(b"file merge=union\n", true)]
#[case::binary(b"file merge=binary\n", false)]
#[case::unset(b"file -merge\n", false)]
#[case::undefined_custom(b"file merge=undefined\n", false)]
fn per_path_attributes_match_git(#[case] rules: &[u8], #[case] clean: bool) {
    let f = Fixture::new("sha1");
    let attr = f.blob(rules);
    let tree = |bytes| {
        f.tree(&[
            ("100644", attr, b".gitattributes"),
            ("100644", f.blob(bytes), b"file"),
        ])
    };
    let base = tree(b"base\n");
    let ours = tree(b"ours\n");
    let theirs = tree(b"theirs\n");
    let b = oracle::commit(f.root.path(), base, None);
    let o = oracle::commit(f.root.path(), ours, Some(b));
    let t = oracle::commit(f.root.path(), theirs, Some(b));
    // A bare merge-tree reads repository attributes from info/attributes.
    std::fs::create_dir_all(f.root.path().join("info")).unwrap();
    std::fs::write(f.root.path().join("info/attributes"), rules).unwrap();
    let expected = oracle::merge_tree(f.root.path(), &o.to_string(), &t.to_string());
    assert_eq!(expected.status.success(), clean);
    let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
    attributes.add(Source::Directory(b""), rules).unwrap();
    let mut driver = AttributeDriver(attributes);
    let result = f
        .objects()
        .merge_trees_with_driver(
            base,
            ours,
            theirs,
            TreeMergeOptions::default(),
            &AtomicBool::new(false),
            &mut driver,
        )
        .unwrap();
    assert_eq!(result.is_clean(), clean);
    let mut markers = girt::merge::MarkerOptions::default();
    markers.ours = o.to_string().into_bytes();
    markers.theirs = t.to_string().into_bytes();
    let prepared = result.prepare_checkout(&f.objects(), &markers).unwrap();
    assert_eq!(
        prepared.tree_id(),
        id(expected.stdout.split(|b| *b == b'\n').next().unwrap())
    );
}

#[derive(Default)]
struct RecordingDriver {
    paths: Vec<Vec<u8>>,
}
impl TreeDriver for RecordingDriver {
    fn merge(
        &mut self,
        path: &[u8],
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        options: ContentOptions,
        cancel: &AtomicBool,
    ) -> DriverResult {
        self.paths.push(path.to_vec());
        Ok(girt::merge::content_with(
            base, ours, theirs, options, cancel,
        )?)
    }
}

#[test]
fn renamed_destination_is_the_driver_path() {
    let f = Fixture::new("sha1");
    let b = f.blob(b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n");
    let o = f.blob(b"ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n");
    let t = f.blob(b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nTEN\n");
    let base = f.tree(&[("100644", b, b"old")]);
    let ours = f.tree(&[("100644", o, b"new")]);
    let theirs = f.tree(&[("100644", t, b"old")]);
    let mut driver = RecordingDriver::default();
    let result = f
        .objects()
        .merge_trees_with_driver(
            base,
            ours,
            theirs,
            TreeMergeOptions::default(),
            &AtomicBool::new(false),
            &mut driver,
        )
        .unwrap();
    assert!(result.is_clean());
    assert_eq!(driver.paths, [b"new".to_vec()]);
}

struct CustomDriver;
impl TreeDriver for CustomDriver {
    fn merge(
        &mut self,
        _: &[u8],
        _: &[u8],
        _: &[u8],
        _: &[u8],
        _: ContentOptions,
        _: &AtomicBool,
    ) -> DriverResult {
        Ok(ContentMerge::resolved(b"custom output\n".to_vec()))
    }
}

#[test]
fn custom_driver_output_stays_in_memory() {
    let f = Fixture::new("sha1");
    let tree = |bytes| f.tree(&[("100644", f.blob(bytes), b"file")]);
    let base = tree(b"base\n");
    let ours = tree(b"ours\n");
    let theirs = tree(b"theirs\n");
    let objects = f.objects();
    let result = objects
        .merge_trees_with_driver(
            base,
            ours,
            theirs,
            TreeMergeOptions::default(),
            &AtomicBool::new(false),
            &mut CustomDriver,
        )
        .unwrap();
    assert!(result.is_clean());
    assert_eq!(result.blobs[0].1, b"custom output\n");
    assert!(!objects.contains(result.blobs[0].0).unwrap());
}

struct FailedDriver;
impl TreeDriver for FailedDriver {
    fn merge(
        &mut self,
        _: &[u8],
        _: &[u8],
        _: &[u8],
        _: &[u8],
        _: ContentOptions,
        _: &AtomicBool,
    ) -> DriverResult {
        Err(std::io::Error::other("driver failed").into())
    }
}

#[test]
fn driver_failure_retains_path_and_original_error_type() {
    let f = Fixture::new("sha1");
    let tree = |bytes| f.tree(&[("100644", f.blob(bytes), b"file")]);
    let base = tree(b"base\n");
    let ours = tree(b"ours\n");
    let theirs = tree(b"theirs\n");
    let result = f.objects().merge_trees_with_driver(
        base,
        ours,
        theirs,
        TreeMergeOptions::default(),
        &AtomicBool::new(false),
        &mut FailedDriver,
    );
    assert!(
        matches!(result, Err(RenameError::Tree(TreeError::Driver { path, source })) if path == b"file" && source.is::<std::io::Error>())
    );
}

#[test]
fn one_driver_instance_serves_virtual_base_and_final_commit_content() {
    let f = Fixture::new("sha1");
    let tree = |bytes| f.tree(&[("100644", f.blob(bytes), b"file")]);
    let root_tree = tree(b"a\nb\nc\n");
    let a_tree = tree(b"A\nb\nc\n");
    let b_tree = tree(b"a\nb\nC\n");
    let root = oracle::commit(f.root.path(), root_tree, None);
    let a = oracle::commit(f.root.path(), a_tree, Some(root));
    let b = oracle::commit(f.root.path(), b_tree, Some(root));
    let ours = oracle::commit_parents(f.root.path(), a_tree, &[a, b]);
    let theirs = oracle::commit_parents(f.root.path(), b_tree, &[b, a]);
    let mut driver = RecordingDriver::default();
    let result = f
        .objects()
        .merge_commits_with_driver(
            ours,
            theirs,
            CommitOptions::default(),
            &AtomicBool::new(false),
            &mut driver,
        )
        .unwrap();
    assert!(result.result.is_clean());
    assert_eq!(result.result.tree.id(), root_tree);
    assert_eq!(driver.paths, [b"file".to_vec(), b"file".to_vec()]);
}

struct CancelDriver;
impl TreeDriver for CancelDriver {
    fn merge(
        &mut self,
        _: &[u8],
        _: &[u8],
        _: &[u8],
        _: &[u8],
        _: ContentOptions,
        cancel: &AtomicBool,
    ) -> DriverResult {
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(ContentMerge::resolved(b"unpublished\n".to_vec()))
    }
}

#[test]
fn cancellation_from_driver_prevents_a_successful_tree_result() {
    let f = Fixture::new("sha1");
    let tree = |bytes| f.tree(&[("100644", f.blob(bytes), b"file")]);
    let base = tree(b"base\n");
    let ours = tree(b"ours\n");
    let theirs = tree(b"theirs\n");
    assert!(matches!(
        f.objects().merge_trees_with_driver(
            base,
            ours,
            theirs,
            TreeMergeOptions::default(),
            &AtomicBool::new(false),
            &mut CancelDriver
        ),
        Err(RenameError::Tree(TreeError::Cancelled))
    ));
}
