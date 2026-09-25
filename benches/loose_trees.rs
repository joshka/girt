//! Loose-tree storage measurements; methodology is in docs/benchmarks.md.
use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use girt::{EntryMode, LooseObjects, ObjectFormat, ObjectId, Tree, TreeEntry};
use tempfile::TempDir;

fn loose_trees(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("loose_trees");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));

    for count in [16, 1024] {
        let entries = (0..count)
            .map(|index| TreeEntry {
                mode: EntryMode::Blob,
                name: format!("entry-{index:06}").into_bytes(),
                id: ObjectId::for_blob(
                    girt::ObjectFormat::Sha1,
                    format!("contents-{index}").as_bytes(),
                ),
            })
            .collect();
        let tree = Tree::new(girt::ObjectFormat::Sha1, entries).unwrap();
        let size = tree.encode().len();
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_function(BenchmarkId::new("read_cached", count), |b| {
            let (_directory, objects, id) = populated_store(&tree);
            black_box(objects.read_tree(id, size).unwrap());
            b.iter(|| objects.read_tree(black_box(id), size).unwrap());
        });
        group.bench_function(BenchmarkId::new("write_new", count), |b| {
            b.iter_batched_ref(
                empty_store,
                |(_, objects)| objects.write_tree(black_box(&tree)).unwrap(),
                BatchSize::PerIteration,
            );
        });
        group.bench_function(BenchmarkId::new("write_existing", count), |b| {
            b.iter_batched_ref(
                || populated_store(&tree),
                |(_, objects, _)| objects.write_tree(black_box(&tree)).unwrap(),
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn empty_store() -> (TempDir, LooseObjects) {
    let directory = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(directory.path(), ObjectFormat::Sha1);
    (directory, objects)
}

fn populated_store(tree: &Tree) -> (TempDir, LooseObjects, ObjectId) {
    let (directory, objects) = empty_store();
    let id = objects.write_tree(tree).unwrap();
    (directory, objects, id)
}

criterion_group!(benches, loose_trees);
criterion_main!(benches);
