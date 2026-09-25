//! Criterion measurements; methodology is in docs/benchmarks.md.
use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use girt::{LooseObjects, ObjectFormat, ObjectId};
use tempfile::TempDir;

fn blobs(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("blobs");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    for size in [64, 4096, 65536, 1048576] {
        group.throughput(Throughput::Bytes(size as u64));
        for (content, bytes) in [
            ("repeated", vec![b'x'; size]),
            ("pseudorandom", pseudorandom_bytes(size)),
        ] {
            group.bench_function(BenchmarkId::new(format!("hash/{content}"), size), |b| {
                b.iter(|| ObjectId::for_blob(girt::ObjectFormat::Sha1, black_box(&bytes)));
            });
            group.bench_function(
                BenchmarkId::new(format!("hash_sha256/{content}"), size),
                |b| {
                    b.iter(|| ObjectId::for_blob(ObjectFormat::Sha256, black_box(&bytes)));
                },
            );
            group.bench_function(
                BenchmarkId::new(format!("read_cached/{content}"), size),
                |b| {
                    let (_directory, objects, id) = populated_store(&bytes);
                    black_box(objects.read_blob(id, bytes.len()).unwrap());
                    b.iter(|| objects.read_blob(black_box(id), bytes.len()).unwrap());
                },
            );
            group.bench_function(
                BenchmarkId::new(format!("write_new/{content}"), size),
                |b| {
                    b.iter_batched_ref(
                        empty_store,
                        |(_, objects)| objects.write_blob(black_box(&bytes)).unwrap(),
                        BatchSize::PerIteration,
                    );
                },
            );
            group.bench_function(
                BenchmarkId::new(format!("write_existing/{content}"), size),
                |b| {
                    b.iter_batched_ref(
                        || populated_store(&bytes),
                        |(_, objects, _)| objects.write_blob(black_box(&bytes)).unwrap(),
                        BatchSize::PerIteration,
                    );
                },
            );
        }
    }
    group.finish();
}

// Return the directory owner alongside the store so cleanup happens after each measurement.
fn empty_store() -> (TempDir, LooseObjects) {
    let directory = tempfile::tempdir().unwrap();
    let objects = LooseObjects::new(directory.path(), ObjectFormat::Sha1).unwrap();
    (directory, objects)
}

fn populated_store(bytes: &[u8]) -> (TempDir, LooseObjects, ObjectId) {
    let (directory, objects) = empty_store();
    let id = objects.write_blob(bytes).unwrap();
    (directory, objects, id)
}

// Fixed xorshift64 seed and recurrence make the byte workload repeatable without a random
// dependency.
fn pseudorandom_bytes(size: usize) -> Vec<u8> {
    let mut state = 0x4d59_5df4_d0f3_3173_u64;
    (0..size)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

criterion_group!(benches, blobs);
criterion_main!(benches);
