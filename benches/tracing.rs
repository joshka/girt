//! End-to-end cached reads and traversal, including the cost of optional span recording.
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use girt::{
    Commit, CommitFields, HistoryLimits, InitKind, PackLimits, Repository, Signature, Tree,
};

fn measure(c: &mut Criterion) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(root.path().join("repo"), InitKind::Bare).unwrap();
    let loose = repo.loose_objects().unwrap();
    let blob = loose.write_blob(&vec![b'x'; 4096]).unwrap();
    let tree = loose.write_tree(&Tree::new(vec![]).unwrap()).unwrap();
    let signature = Signature {
        name: b"benchmark".to_vec(),
        email: b"benchmark@example.test".to_vec(),
        seconds: 0,
        offset_minutes: 0,
    };
    let mut parents = vec![];
    for _ in 0..128 {
        let commit = Commit::new(CommitFields {
            tree,
            parents,
            author: signature.clone(),
            committer: signature.clone(),
            extra_headers: vec![],
            message: vec![],
        })
        .unwrap();
        parents = vec![loose.write_commit(&commit).unwrap()];
    }
    let objects = repo.objects(PackLimits::default()).unwrap();
    let mut run = |mode: &str| {
        let mut group = c.benchmark_group(mode);
        group.sample_size(30);
        group.warm_up_time(std::time::Duration::from_secs(1));
        group.measurement_time(std::time::Duration::from_secs(2));
        group.bench_function("cached_blob_4k", |b| {
            b.iter(|| black_box(loose.read_blob(black_box(blob), 4096).unwrap()))
        });
        group.bench_function("history_128", |b| {
            b.iter(|| {
                black_box(
                    objects
                        .walk(black_box(&parents), HistoryLimits::default())
                        .unwrap(),
                )
            })
        });
        group.finish();
    };
    #[cfg(not(feature = "tracing"))]
    run("tracing_disabled");
    #[cfg(feature = "tracing")]
    {
        run("tracing_no_subscriber");
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(std::io::sink)
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .finish();
        tracing::subscriber::with_default(subscriber, || run("tracing_interested_sink"));
    }
}
criterion_group!(benches, measure);
criterion_main!(benches);
