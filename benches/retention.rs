//! Retention planning and additive repack over an independently generated packed history.
use std::hint::black_box;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use criterion::{Criterion, criterion_group, criterion_main};
use girt::Repository;
use girt::retention::{MaintenanceIsolation, RepackLimits, RetentionPolicy};
#[path = "../tests/support/history_git.rs"]
mod history_git;

struct IsolatedFixture;

impl MaintenanceIsolation for IsolatedFixture {
    type Guard = ();

    fn acquire(&mut self, _: &Repository) -> std::io::Result<Self::Guard> {
        // This disposable fixture has no independent writers, readers or alternate dependents.
        Ok(())
    }
}

fn retention(c: &mut Criterion) {
    let mut parents = vec![vec![]];
    for index in 1..256 {
        parents.push(vec![index - 1]);
    }
    let history = history_git::History::new(&parents, true);
    assert_eq!(history.query("rev-list", &[history.ids[255]]).len(), 256);
    assert!(history.is_ancestor(history.ids[0], history.ids[255]));
    assert!(
        history
            .objects()
            .read(history.ids[255], girt::ReadLimits::default())
            .unwrap()
            .is_some()
    );
    let repository = Repository::open(history.root.path()).unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let cancel = AtomicBool::new(false);
    assert!(repository.plan_retention(&policy, &cancel).is_complete());
    c.bench_function("retention/packed-linear-256", |bench| {
        bench.iter(|| repository.plan_retention(black_box(&policy), black_box(&cancel)))
    });
    let repack = repository
        .repack_retained(&policy, RepackLimits::default(), &cancel)
        .unwrap();
    assert!(repack.written.objects >= 256);
    c.bench_function("repack/idempotent-packed-linear-256", |bench| {
        bench.iter(|| {
            repository.repack_retained(
                black_box(&policy),
                RepackLimits::default(),
                black_box(&cancel),
            )
        })
    });
    c.bench_function("prune/idempotent-packed-linear-256", |bench| {
        bench.iter(|| {
            repository.prune_unreachable_loose(
                &mut IsolatedFixture,
                black_box(&policy),
                black_box(&cancel),
            )
        })
    });
    c.bench_function("reflog-expire/noop-packed-linear-256", |bench| {
        bench.iter(|| {
            repository.expire_reflogs(&mut IsolatedFixture, black_box(&policy), black_box(&cancel))
        })
    });
    #[cfg(unix)]
    {
        repository
            .retire_old_packs(
                &mut IsolatedFixture,
                &policy,
                RepackLimits::default(),
                &cancel,
            )
            .unwrap();
        c.bench_function("pack-retire/idempotent-packed-linear-256", |bench| {
            bench.iter(|| {
                repository.retire_old_packs(
                    &mut IsolatedFixture,
                    black_box(&policy),
                    RepackLimits::default(),
                    black_box(&cancel),
                )
            })
        });
        c.bench_function("maintenance/idempotent-packed-linear-256", |bench| {
            bench.iter(|| {
                repository.run_maintenance(
                    &mut IsolatedFixture,
                    black_box(&policy),
                    RepackLimits::default(),
                    black_box(&cancel),
                )
            })
        });
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(6));
    targets = retention
}
criterion_main!(benches);
