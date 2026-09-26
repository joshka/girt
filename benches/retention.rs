//! Read-only retention planning over an independently generated packed history.
use std::hint::black_box;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use criterion::{Criterion, criterion_group, criterion_main};
use girt::Repository;
use girt::retention::RetentionPolicy;
#[path = "../tests/support/history_git.rs"]
mod history_git;

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
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(6));
    targets = retention
}
criterion_main!(benches);
