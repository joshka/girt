use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use girt::HistoryLimits;
#[path = "../tests/support/history_git.rs"]
mod history_git;

fn history(c: &mut Criterion) {
    for merge_heavy in [false, true] {
        let mut parents = vec![vec![]];
        if merge_heavy {
            for _ in 0..85 {
                let base = parents.len() - 1;
                parents.push(vec![base]);
                parents.push(vec![base]);
                parents.push(vec![base + 1, base + 2]);
            }
        } else {
            for i in 1..256 {
                parents.push(vec![i - 1]);
            }
        }
        let h = history_git::History::new(&parents, true);
        let objects = h.objects();
        assert_eq!(h.query("rev-list", &[h.ids[255]]).len(), 256);
        assert!(h.is_ancestor(h.ids[254], h.ids[255]));
        let shape = if merge_heavy { "merge-heavy" } else { "linear" };
        c.bench_function(&format!("history/{shape}/walk-packed-256"), |b| {
            b.iter(|| {
                objects
                    .walk(black_box(&[h.ids[255]]), HistoryLimits::default())
                    .unwrap()
            })
        });
        std::fs::write(h.root.path().join("shallow"), format!("{}\n", h.ids[128])).unwrap();
        let shallow = h.objects();
        c.bench_function(&format!("history/{shape}/walk-shallow-packed-256"), |b| {
            b.iter(|| {
                shallow
                    .walk(black_box(&[h.ids[255]]), HistoryLimits::default())
                    .unwrap()
            })
        });
        c.bench_function(&format!("history/{shape}/merge-bases-packed-256"), |b| {
            b.iter(|| {
                objects
                    .merge_bases(
                        black_box(h.ids[254]),
                        black_box(h.ids[255]),
                        HistoryLimits::default(),
                    )
                    .unwrap()
            })
        });
    }
}
criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(4));
    targets = history
}
criterion_main!(benches);
