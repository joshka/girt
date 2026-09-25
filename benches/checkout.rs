use std::hint::black_box;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use girt::{EntryMode, InitKind, ObjectId, Repository, Tree, TreeEntry};

struct Fixture {
    _temp: tempfile::TempDir,
    repo: Repository,
    old: Option<ObjectId>,
    target: ObjectId,
}
impl Fixture {
    fn new(count: usize, update: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            girt::ObjectFormat::Sha1,
            temp.path().join("repo"),
            InitKind::Worktree,
        )
        .unwrap();
        let old_tree = make_tree(&repo, count, b'o');
        let target = make_tree(&repo, count, b'n');
        let old = update.then_some(old_tree);
        if update {
            repo.checkout_tree(None, old, Default::default(), &AtomicBool::new(false))
                .unwrap();
        }
        Self {
            _temp: temp,
            repo,
            old,
            target,
        }
    }
}
fn make_tree(repo: &Repository, count: usize, byte: u8) -> ObjectId {
    let objects = repo.loose_objects();
    let id = objects.write_blob(&vec![byte; 1024]).unwrap();
    let entries = (0..count)
        .map(|n| TreeEntry {
            name: format!("file-{n:06}").into_bytes(),
            mode: EntryMode::Blob,
            id,
        })
        .collect();
    objects
        .write_tree(&Tree::new(girt::ObjectFormat::Sha1, entries).unwrap())
        .unwrap()
}
fn benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("raw_checkout");
    group
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    for (count, update) in [(10, false), (10, true), (100, false), (100, true)] {
        let label = if update { "tracked_update" } else { "initial" };
        group.bench_function(BenchmarkId::new(label, count), |b| {
            b.iter_batched_ref(
                || Fixture::new(count, update),
                |f| {
                    black_box(
                        f.repo
                            .checkout_tree(
                                f.old,
                                Some(f.target),
                                Default::default(),
                                &AtomicBool::new(false),
                            )
                            .unwrap(),
                    );
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}
criterion_group!(benches, benchmark);
criterion_main!(benches);
