//! In-memory tree parsing and encoding; see docs/benchmarks.md.
use std::hint::black_box;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use girt::{EntryMode, ObjectId, Tree, TreeEntry};

fn trees(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("trees");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));

    for count in [16, 1024] {
        let modes = [
            EntryMode::Blob,
            EntryMode::Executable,
            EntryMode::Tree,
            EntryMode::Symlink,
            EntryMode::Gitlink,
        ];
        let entries = (0..count)
            .map(|index| TreeEntry {
                mode: modes[index % modes.len()],
                name: format!("entry-{index:06}").into_bytes(),
                id: ObjectId::from_bytes([0x81; 20]),
            })
            .collect();
        let tree = Tree::new(entries).unwrap();
        let payload = tree.encode();

        group.throughput(Throughput::Bytes(payload.len() as u64));

        group.bench_function(BenchmarkId::new("parse", count), |b| {
            b.iter(|| Tree::parse(black_box(&payload)).unwrap());
        });

        group.bench_function(BenchmarkId::new("encode", count), |b| {
            b.iter(|| black_box(&tree).encode());
        });
    }

    group.finish();
}

criterion_group!(benches, trees);
criterion_main!(benches);
