use std::hint::black_box;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use girt::content_diff::{BinaryMode, ContentDiff, DiffError, DiffLimits, diff};

fn content_diff(c: &mut Criterion) {
    let cancel = AtomicBool::new(false);
    let old: Vec<_> = (0..1000)
        .flat_map(|n| format!("line {n:04}\n").into_bytes())
        .collect();
    let mut new = old.clone();
    new.splice(4500..4510, b"new line\n".iter().copied());
    let repeated = b"repeat\n\n".repeat(500);
    let mut changed = repeated.clone();
    changed.splice(2000..2000, b"inserted\n".iter().copied());
    let unrelated_old = b"old\n".repeat(200);
    let unrelated_new = b"new\n".repeat(200);
    let workloads = [
        ("small_edit_1000_lines", old, new),
        ("repeated_1000_lines", repeated, changed),
        ("unrelated_200_lines", unrelated_old, unrelated_new),
    ];
    let mut group = c.benchmark_group("content_diff");
    for (name, old, new) in workloads {
        assert!(matches!(
            diff(&old, &new, BinaryMode::Auto, DiffLimits::default(), &cancel),
            Ok(ContentDiff::Text(_))
        ));
        group.bench_function(name, |b| {
            b.iter(|| {
                diff(
                    black_box(&old),
                    black_box(&new),
                    BinaryMode::Auto,
                    DiffLimits::default(),
                    &cancel,
                )
                .unwrap()
            })
        });
    }
    let old = b"old\n".repeat(100_000);
    let new = b"new\n".repeat(100_000);
    assert!(matches!(
        diff(&old, &new, BinaryMode::Auto, DiffLimits::default(), &cancel),
        Err(DiffError::Limit("trace"))
    ));
    group.bench_function("unrelated_100000_lines_limit", |b| {
        b.iter(|| {
            diff(
                black_box(&old),
                black_box(&new),
                BinaryMode::Auto,
                DiffLimits::default(),
                &cancel,
            )
            .unwrap_err()
        })
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(3));
    targets = content_diff
}
criterion_main!(benches);
