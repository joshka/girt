//! Benchmarks index parsing and encoding across versions and entry counts.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use girt::ObjectId;
use girt::index::{Entry, Index, Limits, Mode, Version};

fn benchmark(c: &mut Criterion) {
    let limits = Limits::default();
    for version in [Version::V2, Version::V3, Version::V4] {
        let mut group = c.benchmark_group(format!("index_v{}", version as u32));
        for count in [0, 100, 10_000, 100_000] {
            let entries = (0..count)
                .map(|n| {
                    Entry::new(
                        format!("directory/file-{n:08}").into_bytes(),
                        Mode::Regular,
                        ObjectId::for_blob(girt::ObjectFormat::Sha1, b"content"),
                    )
                })
                .collect();
            let mut index = Index::new(girt::ObjectFormat::Sha1, entries, limits).unwrap();
            index.set_version(version, limits).unwrap();
            let bytes = index.encode(limits).unwrap();
            group.throughput(Throughput::Bytes(bytes.len() as u64));
            group.bench_with_input(BenchmarkId::new("parse", count), &bytes, |b, bytes| {
                b.iter(|| Index::parse(girt::ObjectFormat::Sha1, black_box(bytes), limits).unwrap())
            });
            group.bench_with_input(BenchmarkId::new("encode", count), &index, |b, index| {
                b.iter(|| black_box(index).encode(limits).unwrap())
            });
        }
        group.finish();
    }
}
fn tree_identity(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_tree_id");
    let format = girt::ObjectFormat::Sha1;
    let blob = ObjectId::for_blob(format, b"content");
    for count in [100, 10_000, 100_000] {
        let entries = (0..count)
            .map(|n| {
                Entry::new(
                    format!("dir-{:04}/nested/file-{n:08}", n / 100).into_bytes(),
                    Mode::Regular,
                    blob,
                )
            })
            .collect();
        let index = Index::new(format, entries, Limits::default()).unwrap();
        group.throughput(Throughput::Elements(count));
        group.bench_with_input(BenchmarkId::from_parameter(count), &index, |b, index| {
            b.iter(|| black_box(index).tree_id())
        });
    }
    group.finish();
}

fn stat_reuse(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_stat_reuse");
    for count in [100, 10_000, 100_000] {
        let root = tempfile::tempdir().unwrap();
        let repo = girt::Repository::init(
            girt::ObjectFormat::Sha1,
            root.path().join("repo"),
            girt::InitKind::Bare,
        )
        .unwrap();
        let mut edit = repo.edit_index(Limits::default()).unwrap();
        let entries: Vec<_> = (0..count)
            .map(|n| {
                Entry::new(
                    format!("file-{n:08}").into_bytes(),
                    Mode::Regular,
                    ObjectId::for_blob(girt::ObjectFormat::Sha1, b"content"),
                )
            })
            .collect();
        edit.replace_entries(entries.clone()).unwrap();
        group.throughput(Throughput::Elements(count));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &entries,
            |b, entries| {
                b.iter(|| {
                    edit.replace_entries_reusing_stat(black_box(entries.clone()))
                        .unwrap()
                });
            },
        );
    }
    group.finish();
}
fn write_tree(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_write_tree");
    for count in [100, 10_000, 100_000] {
        let root = tempfile::tempdir().unwrap();
        let repo = girt::Repository::init(
            girt::ObjectFormat::Sha1,
            root.path().join("r"),
            girt::InitKind::Bare,
        )
        .unwrap();
        let blob = repo.loose_objects().write_blob(b"content").unwrap();
        let entries = (0..count)
            .map(|n| {
                Entry::new(
                    format!("dir-{:04}/nested/file-{n:08}", n / 100).into_bytes(),
                    Mode::Regular,
                    blob,
                )
            })
            .collect();
        let index = Index::new(girt::ObjectFormat::Sha1, entries, Limits::default()).unwrap();
        index.write_tree(&repo).unwrap();
        group.throughput(Throughput::Elements(count));
        group.bench_with_input(BenchmarkId::from_parameter(count), &index, |b, index| {
            b.iter(|| black_box(index).write_tree(&repo).unwrap());
        });
    }
    group.finish();
}

criterion_group!(benches, benchmark, tree_identity, stat_reuse, write_tree);
criterion_main!(benches);
