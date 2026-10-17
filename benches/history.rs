//! Benchmarks commit history walks over linear and merge-heavy graphs.

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
                    .walk_with(black_box(&[h.ids[255]]), HistoryLimits::default())
                    .unwrap()
            })
        });
        c.bench_function(
            &format!("history/{shape}/fetch-finish-one-commit-packed-256"),
            |b| {
                b.iter_batched(
                    || prepare_one_commit(&h),
                    |(root, ready)| {
                        let report = ready
                            .finish(
                                girt::fetch::FetchUpdateLimits::default(),
                                &std::sync::atomic::AtomicBool::new(false),
                            )
                            .unwrap();
                        black_box((root, report))
                    },
                    criterion::BatchSize::PerIteration,
                );
            },
        );
        std::fs::write(h.root.path().join("shallow"), format!("{}\n", h.ids[128])).unwrap();
        let shallow = h.objects();
        c.bench_function(&format!("history/{shape}/walk-shallow-packed-256"), |b| {
            b.iter(|| {
                shallow
                    .walk_with(black_box(&[h.ids[255]]), HistoryLimits::default())
                    .unwrap()
            })
        });
        c.bench_function(&format!("history/{shape}/merge-bases-packed-256"), |b| {
            b.iter(|| {
                objects
                    .merge_bases_with(
                        black_box(h.ids[254]),
                        black_box(h.ids[255]),
                        HistoryLimits::default(),
                    )
                    .unwrap()
            })
        });
    }
}
// Git creates the destination's old closure; receive/setup are outside the measured finish.
fn prepare_one_commit(h: &history_git::History) -> (tempfile::TempDir, girt::fetch::FetchReady) {
    use girt::fetch::{FetchLimits, FetchRequest, KnownHistory};
    use girt::refs::Reflog;
    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("dest.git");
    history_git::git(
        root.path(),
        &[
            "clone",
            "--bare",
            "--no-hardlinks",
            "--single-branch",
            "--branch",
            "node-254",
            h.root.path().to_str().unwrap(),
            dest.to_str().unwrap(),
        ],
        b"",
    );
    history_git::git(
        &dest,
        &[
            "update-ref",
            "refs/remotes/origin/main",
            &h.ids[254].to_string(),
        ],
        b"",
    );
    let repo = girt::Repository::open(&dest).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let known = KnownHistory::from_store(
        repo.objects_with(girt::PackLimits::default()).unwrap(),
        &[h.ids[254]],
        FetchLimits::default(),
        &cancel,
    )
    .unwrap();
    let specs = girt::remote::Refspecs::parse(
        girt::remote::Direction::Fetch,
        ["refs/heads/node-255:refs/remotes/origin/main"],
    )
    .unwrap();
    let ready = FetchRequest::prepare(repo, specs, Default::default(), Reflog::Preserve)
        .unwrap()
        .receive_local(
            h.root.path(),
            &known,
            FetchLimits::default(),
            girt::transport::TransportControl::new(&cancel),
            girt::fetch::LocalFetchObserver::default(),
        )
        .unwrap();
    (root, ready)
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(4));
    targets = history
}
criterion_main!(benches);
