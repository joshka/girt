//! Tag processing and loose storage; methodology is in docs/benchmarks.md.
use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use girt::{LooseObjects, ObjectFormat, ObjectId, ObjectKind, Signature, Tag, TagFields};
use tempfile::TempDir;

fn fixture(size: usize) -> Tag {
    let pattern = b"Record snapshot: original benchmark content.\n";
    let person = Signature {
        name: b"A. Writer".to_vec(),
        email: b"writer@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: -420,
    };
    let tag = Tag::new(TagFields {
        target: ObjectId::Sha1([1; 20]),
        target_kind: ObjectKind::Commit,
        name: b"v1".to_vec(),
        tagger: Some(person),
        extra_headers: vec![b"x-metadata opaque".to_vec()],
        message: (0..size)
            .map(|index| pattern[index % pattern.len()])
            .collect(),
    });
    tag.unwrap()
}

fn tags(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("tags");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    for size in [128, 65536] {
        let tag = fixture(size);
        let payload = tag.as_bytes();
        group.throughput(Throughput::Bytes(payload.len() as u64));
        group.bench_function(BenchmarkId::new("construct_with_clone", size), |b| {
            b.iter(|| Tag::new(black_box(tag.fields().clone())).unwrap());
        });
        group.bench_function(BenchmarkId::new("parse", size), |b| {
            b.iter(|| Tag::parse(girt::ObjectFormat::Sha1, black_box(payload)).unwrap())
        });
        group.bench_function(BenchmarkId::new("encode", size), |b| {
            b.iter(|| black_box(&tag).encode())
        });
        group.bench_function(BenchmarkId::new("identity", size), |b| {
            b.iter(|| black_box(&tag).id())
        });
        group.bench_function(BenchmarkId::new("read_cached", size), |b| {
            let (_directory, objects, id) = populated_store(&tag);
            black_box(objects.read_tag(id, payload.len()).unwrap());
            b.iter(|| objects.read_tag(black_box(id), payload.len()).unwrap());
        });
        group.bench_function(BenchmarkId::new("write_new", size), |b| {
            b.iter_batched_ref(
                empty_store,
                |(_, objects)| objects.write_tag(black_box(&tag)).unwrap(),
                BatchSize::PerIteration,
            );
        });
        group.bench_function(BenchmarkId::new("write_existing", size), |b| {
            b.iter_batched_ref(
                || populated_store(&tag),
                |(_, objects, _)| objects.write_tag(black_box(&tag)).unwrap(),
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

fn populated_store(tag: &Tag) -> (TempDir, LooseObjects, ObjectId) {
    let (directory, objects) = empty_store();
    let id = objects.write_tag(tag).unwrap();
    (directory, objects, id)
}

criterion_group!(benches, tags);
criterion_main!(benches);
