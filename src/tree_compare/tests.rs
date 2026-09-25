use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use rstest::rstest;

use super::*;
use crate::{PackLimits, TreeEntry};

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    // Original raw loose fixture writer also permits malformed tree payloads and wrong kinds.
    fn store(&self, kind: &str, data: &[u8]) -> ObjectId {
        let id = ObjectId::for_object(kind, data);
        let hex = id.to_string();
        let path = self.0.path().join(&hex[..2]);
        std::fs::create_dir_all(&path).unwrap();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        write!(encoder, "{kind} {}\0", data.len()).unwrap();
        encoder.write_all(data).unwrap();
        std::fs::write(path.join(&hex[2..]), encoder.finish().unwrap()).unwrap();
        id
    }

    fn tree(&self, entries: Vec<TreeEntry>) -> ObjectId {
        self.store("tree", &Tree::new(entries).unwrap().encode())
    }

    fn objects(&self) -> Objects {
        Objects::open(self.0.path(), PackLimits::default()).unwrap()
    }

    fn compare(&self, old: Option<ObjectId>, new: Option<ObjectId>) -> Vec<TreeChange> {
        self.objects()
            .compare_trees(
                old,
                new,
                TreeCompareLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap()
    }

    fn single(&self, entry: Option<(EntryMode, u8)>) -> ObjectId {
        self.tree(
            entry
                .into_iter()
                .map(|(mode, byte)| named(b"file", mode, identity(byte)))
                .collect(),
        )
    }
}

fn identity(byte: u8) -> ObjectId {
    ObjectId::from_bytes([byte; 20])
}

fn named(name: &[u8], mode: EntryMode, id: ObjectId) -> TreeEntry {
    TreeEntry {
        name: name.to_vec(),
        mode,
        id,
    }
}

fn value(mode: EntryMode, byte: u8) -> TreeValue {
    TreeValue {
        mode,
        id: identity(byte),
    }
}

#[rstest]
#[case::unchanged(Some((EntryMode::Blob,1)), Some((EntryMode::Blob,1)), vec![])]
#[case::added(None, Some((EntryMode::Blob,1)), vec![(None,Some(value(EntryMode::Blob,1)))])]
#[case::removed(Some((EntryMode::Blob,1)), None, vec![(Some(value(EntryMode::Blob,1)),None)])]
#[case::content(Some((EntryMode::Blob,1)), Some((EntryMode::Blob,2)), vec![(Some(value(EntryMode::Blob,1)),Some(value(EntryMode::Blob,2)))])]
#[case::executable(Some((EntryMode::Blob,1)), Some((EntryMode::Executable,1)), vec![(Some(value(EntryMode::Blob,1)),Some(value(EntryMode::Executable,1)))])]
#[case::symlink_same_content(Some((EntryMode::Blob,1)), Some((EntryMode::Symlink,1)), vec![(Some(value(EntryMode::Blob,1)),Some(value(EntryMode::Symlink,1)))])]
#[case::symlink_target(Some((EntryMode::Symlink,1)), Some((EntryMode::Symlink,2)), vec![(Some(value(EntryMode::Symlink,1)),Some(value(EntryMode::Symlink,2)))])]
#[case::gitlink(Some((EntryMode::Gitlink,1)), Some((EntryMode::Gitlink,2)), vec![(Some(value(EntryMode::Gitlink,1)),Some(value(EntryMode::Gitlink,2)))])]
#[case::gitlink_type(Some((EntryMode::Blob,1)), Some((EntryMode::Gitlink,1)), vec![(Some(value(EntryMode::Blob,1)),Some(value(EntryMode::Gitlink,1)))])]
#[case::empty(None,None,vec![])]
fn leaf_changes(
    #[case] old: Option<(EntryMode, u8)>,
    #[case] new: Option<(EntryMode, u8)>,
    #[case] expected: Vec<(Option<TreeValue>, Option<TreeValue>)>,
) {
    let fixture = Fixture::new();
    let changes = fixture.compare(Some(fixture.single(old)), Some(fixture.single(new)));
    let expected: Vec<_> = expected
        .into_iter()
        .map(|(old, new)| TreeChange {
            path: b"file".to_vec(),
            old,
            new,
        })
        .collect();
    assert_eq!(changes, expected);
}

#[rstest]
#[case::add(false)]
#[case::remove(true)]
fn empty_side_and_nested_byte_paths(#[case] reverse: bool) {
    let f = Fixture::new();
    let subtree = f.tree(vec![named(b"\xff\n", EntryMode::Symlink, identity(1))]);
    let root = f.tree(vec![named(b"directory", EntryMode::Tree, subtree)]);
    let sides = [None, Some(root)];
    let change = TreeChange {
        path: b"directory/\xff\n".to_vec(),
        old: reverse.then_some(value(EntryMode::Symlink, 1)),
        new: (!reverse).then_some(value(EntryMode::Symlink, 1)),
    };
    assert_eq!(
        f.compare(sides[usize::from(reverse)], sides[usize::from(!reverse)]),
        vec![change]
    );
}

#[rstest]
#[case::forward(false)]
#[case::reverse(true)]
fn replacements_pair_names_across_git_order(#[case] reverse: bool) {
    let f = Fixture::new();
    let subtree = f.tree(vec![named(b"z", EntryMode::Blob, identity(2))]);
    let old = f.tree(vec![
        named(b"a", EntryMode::Blob, identity(1)),
        named(b"a.c", EntryMode::Blob, identity(1)),
    ]);
    let new = f.tree(vec![
        named(b"a", EntryMode::Tree, subtree),
        named(b"a.c", EntryMode::Blob, identity(2)),
        named(b"a0", EntryMode::Blob, identity(2)),
    ]);
    let roots = [Some(old), Some(new)];
    let changes = f.compare(roots[usize::from(reverse)], roots[usize::from(!reverse)]);
    let expected = vec![
        TreeChange {
            path: b"a".to_vec(),
            old: Some(value(EntryMode::Blob, 1)),
            new: None,
        },
        TreeChange {
            path: b"a.c".to_vec(),
            old: Some(value(EntryMode::Blob, 1)),
            new: Some(value(EntryMode::Blob, 2)),
        },
        TreeChange {
            path: b"a/z".to_vec(),
            old: None,
            new: Some(value(EntryMode::Blob, 2)),
        },
        TreeChange {
            path: b"a0".to_vec(),
            old: None,
            new: Some(value(EntryMode::Blob, 2)),
        },
    ];
    let expected: Vec<_> = expected
        .into_iter()
        .map(|c| TreeChange {
            path: c.path,
            old: [c.old, c.new][usize::from(reverse)],
            new: [c.new, c.old][usize::from(reverse)],
        })
        .collect();
    assert_eq!(changes, expected);
}

#[test]
fn empty_directories_have_no_leaf_record() {
    let f = Fixture::new();
    let empty = f.tree(vec![]);
    let root = f.tree(vec![named(b"empty", EntryMode::Tree, empty)]);
    assert!(f.compare(None, Some(root)).is_empty());
}

#[rstest]
#[case::missing("tree", None)]
#[case::malformed("tree",Some(b"bad".as_slice()))]
#[case::wrong_kind("blob",Some(b"content".as_slice()))]
fn identical_roots_are_not_validated(#[case] kind: &str, #[case] bytes: Option<&[u8]>) {
    let f = Fixture::new();
    let id = bytes.map(|data| f.store(kind, data)).unwrap_or(identity(3));
    let limits = TreeCompareLimits {
        max_trees: 0,
        max_entries: 0,
        max_tree_bytes: 0,
        max_changes: 0,
        max_path_bytes: 0,
        max_depth: 0,
        ..TreeCompareLimits::default()
    };
    assert!(
        f.objects()
            .compare_trees(Some(id), Some(id), limits, &AtomicBool::new(false))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn identical_missing_subtree_is_skipped_in_changed_roots() {
    let f = Fixture::new();
    let old = f.tree(vec![named(b"skip", EntryMode::Tree, identity(5))]);
    let new = f.tree(vec![
        named(b"skip", EntryMode::Tree, identity(5)),
        named(b"new", EntryMode::Blob, identity(1)),
    ]);
    assert_eq!(
        f.compare(Some(old), Some(new)),
        vec![TreeChange {
            path: b"new".to_vec(),
            old: None,
            new: Some(value(EntryMode::Blob, 1))
        }]
    );
}

#[test]
fn missing_nested_tree_has_identity_and_byte_path() {
    let f = Fixture::new();
    let root = f.tree(vec![named(b"\xff", EntryMode::Tree, identity(5))]);
    let error = f
        .objects()
        .compare_trees(
            None,
            Some(root),
            TreeCompareLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(
        matches!(error,TreeCompareError::Missing {id,path} if id == identity(5) && path == b"\xff")
    );
}

#[test]
fn wrong_kind_nested_tree_is_contextual() {
    let f = Fixture::new();
    let blob = f.store("blob", b"content");
    let root = f.tree(vec![named(b"nested", EntryMode::Tree, blob)]);
    let error = f
        .objects()
        .compare_trees(
            None,
            Some(root),
            TreeCompareLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(
        matches!(error,TreeCompareError::NotTree {id,path,actual:ObjectKind::Blob} if id == blob && path == b"nested")
    );
}

fn record(name: &[u8]) -> Vec<u8> {
    [b"100644 ".as_slice(), name, b"\0", &[1; 20]].concat()
}

#[rstest]
#[case::syntax(b"bad".to_vec(),TreeError::MissingModeDelimiter)]
#[case::name(record(b"a/b"), TreeError::InvalidName)]
#[case::duplicate([record(b"a"),record(b"a")].concat(),TreeError::DuplicateName)]
#[case::order([record(b"b"),record(b"a")].concat(),TreeError::Unsorted)]
fn invalid_trees_are_not_silently_normalized(
    #[case] payload: Vec<u8>,
    #[case] expected: TreeError,
) {
    let f = Fixture::new();
    let bad = f.store("tree", &payload);
    let root = f.tree(vec![named(b"nested", EntryMode::Tree, bad)]);
    let error = f
        .objects()
        .compare_trees(
            None,
            Some(root),
            TreeCompareLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(
        matches!(error,TreeCompareError::Invalid {id,path,source} if id == bad && path == b"nested" && source == expected)
    );
}

#[test]
fn corrupt_storage_retains_source() {
    let f = Fixture::new();
    let root = f.tree(vec![]);
    let hex = root.to_string();
    std::fs::write(f.0.path().join(&hex[..2]).join(&hex[2..]), b"corrupt").unwrap();
    assert!(
        matches!(f.objects().compare_trees(None,Some(root),TreeCompareLimits::default(),&AtomicBool::new(false)),Err(TreeCompareError::Read {id,path,..}) if id == root && path.is_empty())
    );
}

#[rstest]
#[case::trees(TreeCompareLimits { max_trees: 0, ..TreeCompareLimits::default() }, "trees")]
#[case::entries(TreeCompareLimits { max_entries: 0, ..TreeCompareLimits::default() }, "entries")]
#[case::paths(TreeCompareLimits { max_path_bytes: 0, ..TreeCompareLimits::default() }, "path bytes")]
#[case::changes(TreeCompareLimits { max_changes: 0, ..TreeCompareLimits::default() }, "changes")]
fn zero_budgets_reject_before_result_growth(
    #[case] limits: TreeCompareLimits,
    #[case] bound: &str,
) {
    let f = Fixture::new();
    let root = f.single(Some((EntryMode::Blob, 1)));
    assert!(
        matches!(f.objects().compare_trees(None,Some(root),limits,&AtomicBool::new(false)),Err(TreeCompareError::Limit(name)) if name == bound)
    );
}

#[test]
fn exact_limits_succeed() {
    let f = Fixture::new();
    let root = f.single(Some((EntryMode::Blob, 1)));
    let limits = TreeCompareLimits {
        max_trees: 1,
        max_entries: 1,
        max_tree_bytes: 32,
        max_path_bytes: 4,
        max_changes: 1,
        max_depth: 0,
        ..TreeCompareLimits::default()
    };
    assert_eq!(
        f.objects()
            .compare_trees(None, Some(root), limits, &AtomicBool::new(false))
            .unwrap()
            .len(),
        1
    );
}

#[rstest]
#[case::per_read(TreeCompareLimits { read: ReadLimits { max_object_bytes: 31, ..ReadLimits::default() }, ..TreeCompareLimits::default() })]
#[case::cumulative(TreeCompareLimits { max_tree_bytes: 31, ..TreeCompareLimits::default() })]
fn bytes_are_bounded_during_storage_read(#[case] limits: TreeCompareLimits) {
    let f = Fixture::new();
    let root = f.single(Some((EntryMode::Blob, 1)));
    assert!(matches!(
        f.objects()
            .compare_trees(None, Some(root), limits, &AtomicBool::new(false)),
        Err(TreeCompareError::Read { .. })
    ));
}

#[test]
fn cumulative_bytes_include_both_sides() {
    let f = Fixture::new();
    let old = f.single(Some((EntryMode::Blob, 1)));
    let new = f.single(Some((EntryMode::Blob, 2)));
    let limits = TreeCompareLimits {
        max_tree_bytes: 63,
        ..TreeCompareLimits::default()
    };
    assert!(
        matches!(f.objects().compare_trees(Some(old),Some(new),limits,&AtomicBool::new(false)),Err(TreeCompareError::Read {id,..}) if id == new)
    );
}

#[test]
fn leaf_targets_are_not_type_checked() {
    let f = Fixture::new();
    let tree = f.tree(vec![]);
    let root = f.tree(vec![named(b"claimed-blob", EntryMode::Blob, tree)]);
    assert_eq!(
        f.compare(None, Some(root))[0].new,
        Some(TreeValue {
            id: tree,
            mode: EntryMode::Blob
        })
    );
}

#[test]
fn depth_limit_precedes_queue_growth() {
    let f = Fixture::new();
    let root = f.tree(vec![named(b"sub", EntryMode::Tree, identity(5))]);
    let limits = TreeCompareLimits {
        max_depth: 0,
        ..TreeCompareLimits::default()
    };
    assert!(matches!(
        f.objects()
            .compare_trees(None, Some(root), limits, &AtomicBool::new(false)),
        Err(TreeCompareError::Limit("depth"))
    ));
}

#[rstest]
#[case::empty(None, None)]
#[case::equal(Some(identity(1)), Some(identity(1)))]
#[case::different(None, Some(identity(1)))]
fn cancellation_precedes_identity_skips_and_reads(
    #[case] old: Option<ObjectId>,
    #[case] new: Option<ObjectId>,
) {
    let f = Fixture::new();
    assert!(matches!(
        f.objects().compare_trees(
            old,
            new,
            TreeCompareLimits::default(),
            &AtomicBool::new(true)
        ),
        Err(TreeCompareError::Cancelled)
    ));
}

#[test]
fn cancellation_between_reads_stops_before_missing_object() {
    let f = Fixture::new();
    let cancel = AtomicBool::new(false);
    let mut budget = Budget {
        remaining: TreeCompareLimits::default(),
        cancel: &cancel,
    };
    assert!(budget.read(&f.objects(), None, b"").is_ok());
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(
        budget.read(&f.objects(), Some(identity(1)), b"next"),
        Err(TreeCompareError::Cancelled)
    ));
}

// Constructing a deep tree is iterative too; no native filesystem directory depth is involved.
fn deep_tree(f: &Fixture, depth: usize) -> ObjectId {
    let mut id = f.single(Some((EntryMode::Blob, 1)));
    for _ in 0..depth {
        id = f.tree(vec![named(b"a", EntryMode::Tree, id)]);
    }
    id
}

#[test]
fn deep_tree_uses_an_explicit_stack() {
    let f = Fixture::new();
    let root = deep_tree(&f, 2000);
    let limits = TreeCompareLimits {
        max_depth: 2000,
        ..TreeCompareLimits::default()
    };
    let changes = f
        .objects()
        .compare_trees(None, Some(root), limits, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path.len(), 4004);
}
