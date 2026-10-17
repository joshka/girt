//! Warm prefix lookup and abbreviation over an original 65,536-object pack.
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use girt::{InitKind, ObjectFormat, ObjectId, ObjectIdPrefix, PackLimits, Repository};
#[path = "../tests/support/short_ids_git.rs"]
mod short_ids_git;
fn short_ids(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let format = ObjectFormat::Sha1;
    let repo = Repository::init(format, dir.path().join("repo"), InitKind::Bare).unwrap();
    short_ids_git::populate(&dir.path().join("repo"), 65536);
    let objects = repo.objects(PackLimits::default()).unwrap();
    let id = ObjectId::for_blob(format, &0u32.to_le_bytes());
    let prefix = ObjectIdPrefix::from_hex(format, &id.to_string()[..7]).unwrap();
    c.bench_function("short_ids/resolve-packed-65536", |b| {
        b.iter(|| objects.resolve_prefix(black_box(prefix)).unwrap())
    });
    c.bench_function("short_ids/abbreviate-first-bucket-packed-65536", |b| {
        b.iter_batched(
            || repo.objects(PackLimits::default()).unwrap(),
            |fresh| fresh.abbreviate(black_box(id), 7).unwrap(),
            criterion::BatchSize::SmallInput,
        );
    });
    c.bench_function("short_ids/abbreviate-packed-65536", |b| {
        b.iter(|| objects.abbreviate(black_box(id), 7).unwrap())
    });
}
criterion_group!(benches, short_ids);
criterion_main!(benches);
