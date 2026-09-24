use std::hint::black_box;
use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use girt::fetch::{
    AdvertisedRef, Advertisement, FetchLimits, FetchRequest, FetchUpdateLimits, KnownHistory,
};
use girt::refs::{Expected, RefName, Reflog, Target};
use girt::remote::{Direction, Refspecs};
use girt::transport::TransportControl;
use girt::{InitKind, ObjectId, Repository};

fn request(repo: Repository) -> FetchRequest {
    let specs = Refspecs::parse(Direction::Fetch, ["refs/tags/*:refs/remotes/origin/*"]).unwrap();
    FetchRequest::prepare(repo, specs, Default::default(), Reflog::Preserve).unwrap()
}
fn workflow(c: &mut Criterion) {
    let mut group = c.benchmark_group("fetch_workflow");
    group
        .sample_size(30)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(4));
    let root = tempfile::tempdir().unwrap();
    let planning = request(Repository::init(root.path().join("planning"), InitKind::Bare).unwrap());
    for count in [10, 10_000] {
        let advertisement = Advertisement {
            refs: (0..count)
                .map(|i| AdvertisedRef {
                    name: RefName::new(format!("refs/tags/{i}")).unwrap(),
                    id: ObjectId::for_blob(i.to_string().as_bytes()),
                    peeled: false,
                })
                .collect(),
            capabilities: Vec::new(),
        };
        group.bench_with_input(
            BenchmarkId::new("plan", count),
            &advertisement,
            |b, advertisement| {
                b.iter(|| black_box(planning.plan(black_box(advertisement)).unwrap()));
            },
        );
    }
    let source = Repository::init(root.path().join("source"), InitKind::Bare).unwrap();
    let id = source
        .loose_objects()
        .unwrap()
        .write_blob(&vec![b'x'; 4096])
        .unwrap();
    for i in 0..10 {
        source
            .references()
            .unwrap()
            .update_without_reflog(
                &RefName::new(format!("refs/tags/{i}")).unwrap(),
                Target::Direct(id),
                Expected::Absent,
            )
            .unwrap();
    }
    let cancel = AtomicBool::new(false);
    group.bench_function("install_publish_10", |b| {
        b.iter_batched(
            || {
                let root = tempfile::tempdir().unwrap();
                let repository =
                    Repository::init(root.path().join("repo"), InitKind::Bare).unwrap();
                let ready = request(repository)
                    .receive_local(
                        source.git_dir(),
                        &KnownHistory::default(),
                        FetchLimits::default(),
                        TransportControl::new(&cancel),
                        |_| ControlFlow::Continue(()),
                    )
                    .unwrap();
                (root, ready)
            },
            |(root, ready)| {
                let report = ready.finish(FetchUpdateLimits::default(), &cancel).unwrap();
                black_box((root, report))
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}
criterion_group!(benches, workflow);
criterion_main!(benches);
