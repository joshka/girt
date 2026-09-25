use std::sync::atomic::AtomicBool;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use girt::refs::reftable::{Snapshot, StackLimits, compact};
use girt::refs::{Backend, Expected, RefEdit, RefName, Reflog, Target};
use girt::{InitKind, ObjectFormat, ObjectId, Repository};

fn fixture(count: usize, layers: usize) -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        ObjectFormat::Sha1,
        root.path().join("repo"),
        InitKind::Bare,
        Backend::Reftable,
    )
    .unwrap();
    let edits: Vec<_> = (0..count)
        .map(|index| RefEdit {
            name: RefName::new(format!("refs/tags/fixture-{index:06}")).unwrap(),
            target: Some(Target::Direct(ObjectId::Sha1([1; 20]))),
            expected: Expected::Any,
            dereference: false,
            reflog: Reflog::Preserve,
        })
        .collect();
    for _ in 0..layers {
        repo.references().unwrap().transaction(&edits).unwrap();
    }
    (root, repo)
}

fn benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("reftable");
    group.sample_size(30);
    for count in [16, 256] {
        let (_root, repo) = fixture(count, 1);
        let directory = repo.git_dir().join("reftable");
        group.bench_with_input(BenchmarkId::new("snapshot-warm", count), &count, |b, _| {
            b.iter(|| {
                Snapshot::read(
                    &directory,
                    ObjectFormat::Sha1,
                    StackLimits::default(),
                    &AtomicBool::new(false),
                )
                .unwrap()
            });
        });
        group.bench_with_input(
            BenchmarkId::new("publish-warm", count),
            &count,
            |b, &count| {
                b.iter_batched(
                    || fixture(count, 1),
                    |(_root, repo)| {
                        repo.references()
                            .unwrap()
                            .update_without_reflog(
                                &RefName::new(b"refs/tags/new").unwrap(),
                                Target::Direct(ObjectId::Sha1([2; 20])),
                                Expected::Absent,
                            )
                            .unwrap();
                        (_root, repo)
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("compact-16-layers-warm", count),
            &count,
            |b, &count| {
                b.iter_batched(
                    || fixture(count, 16),
                    |(_root, repo)| {
                        compact(
                            &repo.git_dir().join("reftable"),
                            ObjectFormat::Sha1,
                            StackLimits::default(),
                            &AtomicBool::new(false),
                        )
                        .unwrap();
                        (_root, repo)
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}
criterion_group!(benches, benchmarks);
criterion_main!(benches);
