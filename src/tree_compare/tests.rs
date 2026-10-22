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
        self.store(
            "tree",
            &Tree::new(crate::ObjectFormat::Sha1, entries)
                .unwrap()
                .encode(),
        )
    }

    fn objects(&self) -> Objects {
        Objects::open(
            crate::ObjectFormat::Sha1,
            self.0.path(),
            PackLimits::default(),
            crate::AlternateLimits::default(),
        )
        .unwrap()
    }

    fn compare(&self, old: Option<ObjectId>, new: Option<ObjectId>) -> Vec<TreeChange> {
        self.objects()
            .compare_trees_with(
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
    ObjectId::Sha1([byte; 20])
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
            .compare_trees_with(Some(id), Some(id), limits, &AtomicBool::new(false))
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
        .compare_trees_with(
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
        .compare_trees_with(
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
        .compare_trees_with(
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
    let error = f
        .objects()
        .compare_trees_with(
            None,
            Some(root),
            TreeCompareLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    let source = std::error::Error::source(&error).unwrap();
    assert!(matches!(
        source
            .downcast_ref::<Box<ObjectReadError>>()
            .map(Box::as_ref),
        Some(ObjectReadError::Loose(_))
    ));
    assert!(
        matches!(error, TreeCompareError::Read { id, path, .. } if id == root && path.is_empty())
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
        matches!(f.objects().compare_trees_with(None,Some(root),limits,&AtomicBool::new(false)),Err(TreeCompareError::Limit(name)) if name == bound)
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
            .compare_trees_with(None, Some(root), limits, &AtomicBool::new(false))
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
            .compare_trees_with(None, Some(root), limits, &AtomicBool::new(false)),
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
        matches!(f.objects().compare_trees_with(Some(old),Some(new),limits,&AtomicBool::new(false)),Err(TreeCompareError::Read {id,..}) if id == new)
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
            .compare_trees_with(None, Some(root), limits, &AtomicBool::new(false)),
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
        f.objects().compare_trees_with(
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
        local: &std::collections::HashMap::new(),
    };
    let objects = f.objects();
    let store = InlineObjects(&objects);
    assert!(block_on(budget.read(&store, None, b"")).is_ok());
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(
        block_on(budget.read(&store, Some(identity(1)), b"next")),
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
        .compare_trees_with(None, Some(root), limits, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path.len(), 4004);
}

#[test]
fn equal_sha256_roots_do_not_bypass_format_validation() {
    let fixture = Fixture::new();
    let id = ObjectId::Sha256([1; 32]);
    let result = fixture.objects().compare_trees_with(
        Some(id),
        Some(id),
        TreeCompareLimits::default(),
        &AtomicBool::new(false),
    );
    assert!(matches!(result, Err(TreeCompareError::ObjectFormat(_))));
}

/// Writes each object to both a loose directory and a memory store.
struct BothStores {
    fixture: Fixture,
    memory: crate::storage::MemoryObjects,
}

impl BothStores {
    fn new() -> Self {
        Self {
            fixture: Fixture::new(),
            memory: crate::storage::MemoryObjects::new(crate::ObjectFormat::Sha1),
        }
    }

    fn store(&self, kind: ObjectKind, data: &[u8]) -> ObjectId {
        use crate::storage::ObjectWrite;

        let id = self.fixture.store(kind.as_str(), data);
        assert_eq!(block_on(self.memory.write(kind, data)).unwrap(), id);
        id
    }

    fn tree(&self, entries: Vec<TreeEntry>) -> ObjectId {
        let tree = Tree::new(crate::ObjectFormat::Sha1, entries).unwrap();
        self.store(ObjectKind::Tree, &tree.encode())
    }

    /// Compares in the loose directory, the memory store and the memory store behind `dyn`.
    fn compare(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        paths: &Pathspec,
    ) -> Result<Vec<TreeChange>, TreeCompareError> {
        let limits = TreeCompareLimits::default();
        let cancel = AtomicBool::new(false);
        let objects = self.fixture.objects();
        let erased: &dyn crate::storage::DynObjectRead = &self.memory;

        let memory = block_on(compare_trees_in(&self.memory, old, new, paths, limits));
        let dynamic = block_on(compare_trees_in(erased, old, new, paths, limits));
        let files = objects.compare_trees_matching(old, new, paths, limits, &cancel);

        match (&memory, &files) {
            (Ok(memory), Ok(files)) => {
                assert_eq!(memory, files);
                assert_eq!(dynamic.as_ref().unwrap(), files);
            }
            (Err(memory), Err(files)) => assert_eq!(memory.to_string(), files.to_string()),
            _ => panic!("stores disagree: memory {memory:?}, files {files:?}"),
        }
        memory
    }
}

#[test]
fn memory_and_filesystem_stores_compare_identically() {
    let stores = BothStores::new();
    let one = stores.store(ObjectKind::Blob, b"one\n");
    let two = stores.store(ObjectKind::Blob, b"two\n");
    let inner = stores.tree(vec![named(b"leaf", EntryMode::Blob, one)]);
    let changed = stores.tree(vec![named(b"leaf", EntryMode::Blob, two)]);
    let old = stores.tree(vec![
        named(b"a", EntryMode::Blob, one),
        named(b"dir", EntryMode::Tree, inner),
        named(b"same", EntryMode::Tree, inner),
    ]);
    let new = stores.tree(vec![
        named(b"a", EntryMode::Tree, changed),
        named(b"dir", EntryMode::Tree, changed),
        named(b"same", EntryMode::Tree, inner),
    ]);

    let all = stores
        .compare(Some(old), Some(new), &Pathspec::all())
        .unwrap();
    let paths: Vec<_> = all.iter().map(|change| change.path.as_slice()).collect();
    assert_eq!(paths, [&b"a"[..], b"a/leaf", b"dir/leaf"]);

    let context = crate::pathspec::Context::new();
    let narrow = Pathspec::parse(["dir"], &context).unwrap();
    let selected = stores.compare(Some(old), Some(new), &narrow).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].path, b"dir/leaf");

    assert!(
        stores
            .compare(None, None, &Pathspec::all())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn memory_store_reports_missing_and_wrong_kind_trees() {
    let stores = BothStores::new();
    let blob = stores.store(ObjectKind::Blob, b"not a tree\n");
    let absent = identity(0x42);

    let error = stores
        .compare(None, Some(absent), &Pathspec::all())
        .unwrap_err();
    assert!(matches!(error, TreeCompareError::Missing { id, .. } if id == absent));

    let error = stores
        .compare(None, Some(blob), &Pathspec::all())
        .unwrap_err();
    assert!(matches!(
        error,
        TreeCompareError::NotTree {
            actual: ObjectKind::Blob,
            ..
        }
    ));
}

#[test]
fn store_failures_keep_their_classification() {
    let stores = BothStores::new();
    let blob = stores.store(ObjectKind::Blob, b"leaf\n");
    let root = stores.tree(vec![named(b"leaf", EntryMode::Blob, blob)]);
    let limits = TreeCompareLimits::default().with_max_tree_bytes(1);

    let error = block_on(compare_trees_in(
        &stores.memory,
        None,
        Some(root),
        &Pathspec::all(),
        limits,
    ))
    .unwrap_err();
    let TreeCompareError::Store { id, source, .. } = error else {
        panic!("expected a store error, got {error:?}");
    };
    assert_eq!(id, root);
    assert_eq!(source.kind(), crate::storage::StoreErrorKind::Limit);

    // A repository keeps reporting its own read error type.
    let error = stores
        .fixture
        .objects()
        .compare_trees_with(None, Some(root), limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error, TreeCompareError::Read { .. }), "{error:?}");
}
