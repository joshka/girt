//! Commit processing and loose storage; methodology is in docs/benchmarks.md.
use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use girt::{
    Commit, CommitFields, CommitHeader, CommitPayload, LooseObjects, ObjectFormat, ObjectId,
    Signature,
};
use tempfile::TempDir;

fn fixture(size: usize) -> Commit {
    let pattern = b"Record snapshot: original benchmark content.\n";
    let person = Signature {
        name: b"A. Writer".to_vec(),
        email: b"writer@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: -420,
    };
    let commit = Commit::new(CommitFields {
        tree: ObjectId::Sha1([1; 20]),
        parents: vec![ObjectId::Sha1([2; 20]), ObjectId::Sha1([3; 20])],
        author: person.clone(),
        committer: person,
        extra_headers: vec![CommitHeader {
            name: b"gpgsig".to_vec(),
            value: b"first line\n continuation\nlast line".to_vec(),
        }],
        message: (0..size)
            .map(|index| pattern[index % pattern.len()])
            .collect(),
    });
    commit.unwrap()
}

fn commits(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("commits");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    for size in [128, 65536] {
        let commit = fixture(size);
        let payload = commit.as_bytes();
        group.throughput(Throughput::Bytes(payload.len() as u64));
        let fields = commit.to_fields().unwrap();
        group.bench_function(BenchmarkId::new("construct_with_clone", size), |b| {
            b.iter(|| Commit::new(black_box(fields.clone())).unwrap());
        });
        group.bench_function(BenchmarkId::new("parse", size), |b| {
            b.iter(|| Commit::parse(girt::ObjectFormat::Sha1, black_box(payload)).unwrap())
        });
        group.bench_function(BenchmarkId::new("payload_view", size), |b| {
            b.iter(|| CommitPayload::parse(black_box(payload)).unwrap())
        });
        let view = CommitPayload::parse(payload).unwrap();
        group.bench_function(BenchmarkId::new("signature_payload", size), |b| {
            b.iter(|| black_box(&view).without_headers(&[5]).unwrap())
        });
        group.bench_function(BenchmarkId::new("encode", size), |b| {
            b.iter(|| black_box(&commit).encode())
        });
        group.bench_function(BenchmarkId::new("identity", size), |b| {
            b.iter(|| black_box(&commit).id())
        });
        group.bench_function(BenchmarkId::new("read_cached", size), |b| {
            let (_directory, objects, id) = populated_store(&commit);
            black_box(objects.read_commit(id, payload.len()).unwrap());
            b.iter(|| objects.read_commit(black_box(id), payload.len()).unwrap());
        });
        group.bench_function(BenchmarkId::new("write_new", size), |b| {
            b.iter_batched_ref(
                empty_store,
                |(_, objects)| objects.write_commit(black_box(&commit)).unwrap(),
                BatchSize::PerIteration,
            );
        });
        group.bench_function(BenchmarkId::new("write_existing", size), |b| {
            b.iter_batched_ref(
                || populated_store(&commit),
                |(_, objects, _)| objects.write_commit(black_box(&commit)).unwrap(),
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

fn populated_store(commit: &Commit) -> (TempDir, LooseObjects, ObjectId) {
    let (directory, objects) = empty_store();
    let id = objects.write_commit(commit).unwrap();
    (directory, objects, id)
}

criterion_group!(benches, commits);
criterion_main!(benches);
