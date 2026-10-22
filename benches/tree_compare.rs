//! Benchmarks tree comparison across tree shapes.

use std::hint::black_box;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use girt::pathspec::Pathspec;
use girt::storage::{MemoryObjects, ObjectWrite, block_on};
use girt::{
    EntryMode, InitKind, ObjectId, ObjectKind, PackLimits, Repository, Tree, TreeCompareLimits,
    TreeEntry, compare_trees_in,
};

fn entry(name: String, mode: EntryMode, id: ObjectId) -> TreeEntry {
    TreeEntry {
        name: name.into_bytes(),
        mode,
        id,
    }
}

fn compare(c: &mut Criterion) {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        girt::ObjectFormat::Sha1,
        temporary.path().join("benchmark"),
        InitKind::Bare,
    )
    .unwrap();
    let store = repo.loose_objects();
    let memory = MemoryObjects::new(girt::ObjectFormat::Sha1);
    let a = store.write_blob(b"first").unwrap();
    let b = store.write_blob(b"second").unwrap();
    block_on(memory.write(ObjectKind::Blob, b"first")).unwrap();
    block_on(memory.write(ObjectKind::Blob, b"second")).unwrap();
    let write = |entries| {
        let tree = Tree::new(girt::ObjectFormat::Sha1, entries).unwrap();
        block_on(memory.write(ObjectKind::Tree, &tree.encode())).unwrap();
        store.write_tree(&tree).unwrap()
    };
    let leaves = |id| {
        (0..100)
            .map(|i| entry(format!("file-{i:03}"), EntryMode::Blob, id))
            .collect()
    };
    let old_subtree = write(leaves(a));
    let new_subtree = write(leaves(b));
    let old = write(
        (0..100)
            .map(|i| entry(format!("dir-{i:03}"), EntryMode::Tree, old_subtree))
            .collect(),
    );
    let sparse = write(
        (0..100)
            .map(|i| {
                entry(
                    format!("dir-{i:03}"),
                    EntryMode::Tree,
                    if i == 50 { new_subtree } else { old_subtree },
                )
            })
            .collect(),
    );
    let broad = write(
        (0..100)
            .map(|i| entry(format!("dir-{i:03}"), EntryMode::Tree, new_subtree))
            .collect(),
    );
    let mut deep_old = write(vec![entry("leaf".into(), EntryMode::Blob, a)]);
    let mut deep_new = write(vec![entry("leaf".into(), EntryMode::Blob, b)]);
    for _ in 0..512 {
        deep_old = write(vec![entry("d".into(), EntryMode::Tree, deep_old)]);
        deep_new = write(vec![entry("d".into(), EntryMode::Tree, deep_new)]);
    }
    let objects = repo.objects_with(PackLimits::default()).unwrap();
    let cancel = AtomicBool::new(false);
    for (name, old, new, count) in [
        ("sparse-100-of-10000", old, sparse, 100),
        ("broad-10000", old, broad, 10000),
        ("deep-512", deep_old, deep_new, 1),
    ] {
        assert_eq!(
            objects
                .compare_trees_with(Some(old), Some(new), TreeCompareLimits::default(), &cancel)
                .unwrap()
                .len(),
            count
        );
        c.bench_function(&format!("tree-compare/{name}"), |bench| {
            bench.iter(|| {
                objects
                    .compare_trees_with(
                        black_box(Some(old)),
                        black_box(Some(new)),
                        TreeCompareLimits::default(),
                        &cancel,
                    )
                    .unwrap()
            });
        });
        c.bench_function(&format!("tree-compare/memory/{name}"), |bench| {
            bench.iter(|| {
                block_on(compare_trees_in(
                    &memory,
                    black_box(Some(old)),
                    black_box(Some(new)),
                    &Pathspec::all(),
                    TreeCompareLimits::default(),
                ))
                .unwrap()
            });
        });
    }
}
criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(3));
    targets = compare
}
criterion_main!(benches);
