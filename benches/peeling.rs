//! Warm loose-store tag chains; setup is outside measurement.
use std::hint::black_box;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use girt::{
    InitKind, ObjectFormat, ObjectKind, PackLimits, PeelLimits, Repository, Tag, TagFields,
};

fn peeling(c: &mut Criterion) {
    let mut group = c.benchmark_group("peeling");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for depth in [1, 16] {
            let root = tempfile::tempdir().unwrap();
            let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
            let loose = repo.loose_objects();
            let mut id = loose.write_blob(b"terminal").unwrap();
            let mut kind = ObjectKind::Blob;
            for _ in 0..depth {
                let tag = Tag::new(TagFields {
                    target: id,
                    target_kind: kind,
                    name: b"v".to_vec(),
                    tagger: None,
                    extra_headers: vec![],
                    message: vec![],
                })
                .unwrap();
                id = loose.write_tag(&tag).unwrap();
                kind = ObjectKind::Tag;
            }
            let objects = repo.objects(PackLimits::default()).unwrap();
            let cancel = AtomicBool::new(false);
            objects.peel(id, PeelLimits::default(), &cancel).unwrap();
            group.bench_function(format!("{format}/{depth}"), |b| {
                b.iter(|| {
                    objects
                        .peel(black_box(id), PeelLimits::default(), &cancel)
                        .unwrap()
                })
            });
        }
    }
    group.finish();
}
criterion_group!(benches, peeling);
criterion_main!(benches);
