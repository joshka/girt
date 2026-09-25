use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use girt::{PackLimits, ReadLimits};

#[path = "../tests/support/pack_git.rs"]
mod pack_git;

fn packs(c: &mut Criterion) {
    for count in [16, 256] {
        let fixture = pack_git::Fixture::new(girt::ObjectFormat::Sha1, true, count);
        let objects = fixture.repo.objects(PackLimits::default()).unwrap();
        // Retain and inspect the shared fixture metadata outside timed operations.
        assert!(fixture.root.path().exists());
        assert!(fixture.index_path.exists());
        assert_eq!(fixture.records.len(), count + 3);
        let report = pack_git::git(
            fixture.root.path(),
            &["verify-pack", "-v", fixture.index_path.to_str().unwrap()],
            b"",
        );
        let report = std::str::from_utf8(&report).unwrap();
        let delta = report
            .lines()
            .find(|line| line.starts_with(&fixture.delta.to_string()))
            .unwrap();
        let ordinary = fixture
            .records
            .iter()
            .find(|record| record.0 == fixture.ordinary)
            .unwrap();
        eprintln!(
            "workload {count}: ordinary_bytes={}, delta_record={delta}, pack_bytes={}, index_bytes={}",
            ordinary.2.len(),
            std::fs::metadata(fixture.index_path.with_extension("pack"))
                .unwrap()
                .len(),
            std::fs::metadata(&fixture.index_path).unwrap().len()
        );
        c.bench_function(&format!("packs/open-validate-warm-{count}"), |b| {
            b.iter(|| {
                fixture
                    .repo
                    .objects(black_box(PackLimits::default()))
                    .unwrap()
            })
        });
        let mut refreshed = objects.clone();
        c.bench_function(&format!("packs/refresh-warm-{count}"), |b| {
            b.iter(|| {
                refreshed
                    .refresh(PackLimits::default(), girt::AlternateLimits::default())
                    .unwrap()
            })
        });
        c.bench_function(&format!("packs/indexed-miss-{count}"), |b| {
            let absent =
                girt::ObjectId::for_blob(girt::ObjectFormat::Sha1, b"absent benchmark object");
            b.iter(|| {
                objects
                    .read(black_box(absent), ReadLimits::default())
                    .unwrap()
            })
        });
        c.bench_function(&format!("packs/indexed-ordinary-{count}"), |b| {
            b.iter(|| {
                objects
                    .read(black_box(fixture.ordinary), ReadLimits::default())
                    .unwrap()
            })
        });
        c.bench_function(&format!("packs/indexed-delta-{count}"), |b| {
            b.iter(|| {
                objects
                    .read(black_box(fixture.delta), ReadLimits::default())
                    .unwrap()
            })
        });
    }
}

// Optional large-store input is generated separately so fixture construction is never timed.
fn large_storage(c: &mut Criterion) {
    let Ok(path) = std::env::var("GIRT_STORAGE_WORKLOAD") else {
        return;
    };
    let repository = girt::Repository::open(&path).unwrap();
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(std::path::Path::new(&path).join("workload.json")).unwrap(),
    )
    .unwrap();
    let small = report["small"].as_str().unwrap().parse().unwrap();
    let large = report["large"].as_str().unwrap().parse().unwrap();
    let objects = repository.objects(PackLimits::default()).unwrap();
    let mut group = c.benchmark_group("large-storage");
    group
        .sampling_mode(criterion::SamplingMode::Flat)
        .sample_size(10)
        .measurement_time(Duration::from_secs(20));
    group.bench_function("open-validate-warm", |b| {
        b.iter(|| {
            repository
                .objects(black_box(PackLimits::default()))
                .unwrap()
        })
    });
    group.measurement_time(Duration::from_secs(2));
    group.bench_function("small-warm", |b| {
        b.iter(|| {
            objects
                .read(black_box(small), ReadLimits::default())
                .unwrap()
        })
    });
    group.bench_function("32mib-warm", |b| {
        b.iter(|| {
            objects
                .read(black_box(large), ReadLimits::default())
                .unwrap()
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(2));
    targets = packs, large_storage
}
criterion_main!(benches);
