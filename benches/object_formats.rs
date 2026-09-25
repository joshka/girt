//! Dual-format hashing, owned codecs and warm loose storage; fixture setup is outside sampling.
use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use girt::{
    Commit, EntryMode, LooseObjects, ObjectFormat, ObjectId, ObjectKind, Tag, Tree, TreeEntry,
};

fn formats(c: &mut Criterion) {
    let mut group = c.benchmark_group("object_formats");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(1));
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let label = format.to_string();
        let bytes: Vec<_> = (0..65536)
            .map(|i| ((i * 37 + i / 251) % 256) as u8)
            .collect();
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_function(BenchmarkId::new("hash_blob_64KiB", &label), |b| {
            b.iter(|| format.hash_object(ObjectKind::Blob, black_box(&bytes)));
        });
        let entries = (0u32..256)
            .map(|i| TreeEntry {
                mode: EntryMode::Blob,
                name: format!("file-{i:04}").into_bytes(),
                id: ObjectId::for_blob(format, &i.to_le_bytes()),
            })
            .collect();
        let tree = Tree::new(format, entries).unwrap();
        let payload = tree.encode();
        group.throughput(Throughput::Bytes(payload.len() as u64));
        group.bench_function(BenchmarkId::new("parse_tree_256", &label), |b| {
            b.iter(|| Tree::parse(format, black_box(&payload)).unwrap());
        });
        group.bench_function(BenchmarkId::new("encode_tree_256", &label), |b| {
            b.iter(|| black_box(&tree).encode());
        });
        let commit = format!(
            "tree {}\nauthor A <a> -1 +0000\ncommitter C <c> 1 +0000\ngpgsig opaque\n folded\n\nmessage\n",
            tree.id()
        );
        group.throughput(Throughput::Bytes(commit.len() as u64));
        group.bench_function(BenchmarkId::new("parse_commit", &label), |b| {
            b.iter(|| Commit::parse(format, black_box(commit.as_bytes())).unwrap());
        });
        let tag = format!(
            "object {}\ntype tree\ntag v1\ntagger A <a> 1 +0000\n\nmessage\n",
            tree.id()
        );
        group.throughput(Throughput::Bytes(tag.len() as u64));
        group.bench_function(BenchmarkId::new("parse_tag", &label), |b| {
            b.iter(|| Tag::parse(format, black_box(tag.as_bytes())).unwrap());
        });
        let directory = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(directory.path(), format);
        let id = objects.write_blob(&bytes).unwrap();
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_function(BenchmarkId::new("read_warm_blob_64KiB", &label), |b| {
            b.iter(|| objects.read_blob(black_box(id), bytes.len()).unwrap());
        });
        group.bench_function(BenchmarkId::new("write_existing_blob_64KiB", &label), |b| {
            b.iter(|| objects.write_blob(black_box(&bytes)).unwrap());
        });
        group.bench_function(BenchmarkId::new("publish_new_blob_64KiB", &label), |b| {
            b.iter_batched(
                || tempfile::tempdir().unwrap(),
                |directory| {
                    let objects = LooseObjects::new(directory.path(), format);
                    let id = objects.write_blob(black_box(&bytes)).unwrap();
                    (directory, id)
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}
criterion_group!(benches, formats);
criterion_main!(benches);
