use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use girt::ObjectId;
use girt::index::{Entry, Index, Limits, Mode};

fn benchmark(c: &mut Criterion) {
    let limits = Limits::default();
    let mut group = c.benchmark_group("index_v2");
    for count in [0, 100, 10_000, 100_000] {
        let entries = (0..count)
            .map(|n| {
                Entry::new(
                    format!("directory/file-{n:08}").into_bytes(),
                    Mode::Regular,
                    ObjectId::for_blob(b"content"),
                )
            })
            .collect();
        let index = Index::new(entries, limits).unwrap();
        let bytes = index.encode(limits).unwrap();
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(BenchmarkId::new("parse", count), &bytes, |b, bytes| {
            b.iter(|| Index::parse(black_box(bytes), limits).unwrap())
        });
        group.bench_with_input(BenchmarkId::new("encode", count), &index, |b, index| {
            b.iter(|| black_box(index).encode(limits).unwrap())
        });
    }
    group.finish();
}
criterion_group!(benches, benchmark);
criterion_main!(benches);
