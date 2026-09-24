use std::hint::black_box;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use girt::clone::{BranchSelection, CloneRequest};
use girt::fetch::{AdvertisedRef, Advertisement};
use girt::refs::{RefName, Reflog};
use girt::{InitKind, ObjectId};

fn clone_plan(c: &mut Criterion) {
    let root = tempfile::tempdir().unwrap();
    let request = CloneRequest::prepare_tracking(
        root.path().join("copy"),
        InitKind::Bare,
        b"benchmark",
        BranchSelection::Default,
        Reflog::Preserve,
    )
    .unwrap();
    let mut group = c.benchmark_group("clone");
    group
        .sample_size(30)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    for count in [10, 10_000] {
        let mut refs: Vec<_> = (0..count)
            .map(|i| AdvertisedRef {
                name: RefName::new(format!("refs/heads/b{i}")).unwrap(),
                id: ObjectId::for_blob(i.to_string().as_bytes()),
                peeled: false,
            })
            .collect();
        refs.push(AdvertisedRef {
            name: RefName::new("HEAD").unwrap(),
            id: refs[0].id,
            peeled: false,
        });
        let ad = Advertisement {
            refs,
            capabilities: vec![b"symref=HEAD:refs/heads/b0".to_vec()],
        };
        group.bench_with_input(BenchmarkId::new("plan", count), &ad, |b, ad| {
            b.iter(|| black_box(request.plan(black_box(ad)).unwrap()))
        });
    }
    group.finish();
}
criterion_group!(benches, clone_plan);
criterion_main!(benches);
