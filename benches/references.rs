use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use girt::refs::{Expected, RefName, Target};
use girt::{ObjectId, Repository};

fn references(c: &mut Criterion) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("objects")).unwrap();
    std::fs::create_dir(root.path().join("refs")).unwrap();
    std::fs::write(root.path().join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    let repo = Repository::open(root.path()).unwrap();
    let refs = repo.references().unwrap();
    let main = RefName::new(b"refs/heads/main").unwrap();
    let head = RefName::new(b"HEAD").unwrap();
    let id = ObjectId::from_bytes([1; 20]);
    refs.update_without_reflog(&main, Target::Direct(id), Expected::Absent)
        .unwrap();
    c.bench_function("references/read-loose-warm", |b| {
        b.iter(|| refs.read(black_box(&main)).unwrap())
    });
    c.bench_function("references/resolve-head-warm", |b| {
        b.iter(|| refs.resolve(black_box(&head), 8).unwrap())
    });
    c.bench_function("references/update-existing-no-reflog", |b| {
        b.iter(|| {
            refs.update_without_reflog(
                &main,
                Target::Direct(black_box(id)),
                Expected::Value(Target::Direct(id)),
            )
            .unwrap()
        })
    });
    for count in [10, 10000] {
        let mut packed = b"# pack-refs with: sorted\n".to_vec();
        for index in 0..count {
            packed.extend_from_slice(format!("{id} refs/tags/tag-{index:05}\n").as_bytes());
        }
        std::fs::write(root.path().join("packed-refs"), packed).unwrap();
        let last = RefName::new(format!("refs/tags/tag-{:05}", count - 1)).unwrap();
        c.bench_function(&format!("references/read-packed-warm-{count}"), |b| {
            b.iter(|| refs.read(black_box(&last)).unwrap())
        });
    }
}
criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(2));
    targets = references
}
criterion_main!(benches);
