use std::hint::black_box;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use girt::ObjectId;
use girt::refs::RefName;
use girt::remote::{Direction, RefSource, Refspecs};

fn remotes(c: &mut Criterion) {
    let mut group = c.benchmark_group("refspec_mapping");
    group
        .sample_size(30)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2));
    for count in [10, 10_000] {
        let sources: Vec<_> = (0..count)
            .map(|index| RefSource {
                name: RefName::new(format!("refs/heads/topic/{index}")).unwrap(),
                id: ObjectId::for_blob(girt::ObjectFormat::Sha1, index.to_string().as_bytes()),
            })
            .collect();
        let specs = Refspecs::parse(
            Direction::Fetch,
            [
                "+refs/heads/*:refs/remotes/origin/*",
                "refs/heads/topic/*:refs/tags/archive/*",
                "^refs/heads/topic/1*",
                "^refs/heads/topic/3*",
            ],
        )
        .unwrap();
        group.bench_with_input(
            BenchmarkId::new("two_mappings_two_exclusions", count),
            &sources,
            |b, sources| {
                b.iter(|| black_box(specs.map(black_box(sources)).unwrap()));
            },
        );
    }
    group.finish();
}
criterion_group!(benches, remotes);
criterion_main!(benches);
