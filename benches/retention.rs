//! Retention planning and additive repack over an independently generated packed history.
use std::hint::black_box;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use criterion::{Criterion, criterion_group, criterion_main};
use girt::retention::{MaintenanceIsolation, RepackLimits, RetentionPolicy};
use girt::{ObjectFormat, ObjectId, ObjectKind, PackObject, PackWriteLimits, Repository};
#[path = "../tests/support/history_git.rs"]
mod history_git;

struct IsolatedFixture;

impl MaintenanceIsolation for IsolatedFixture {
    type Guard = ();

    fn acquire(&mut self, _: &Repository) -> std::io::Result<Self::Guard> {
        // This disposable fixture has no independent writers, readers or alternate dependents.
        Ok(())
    }
}

fn existing_deltas(c: &mut Criterion) {
    let root = tempfile::tempdir().unwrap();
    let repository = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("repo"),
        girt::InitKind::Bare,
    )
    .unwrap();
    let base: Vec<u8> = (0..16_384u32)
        .flat_map(|i| i.wrapping_mul(2_654_435_761).to_le_bytes())
        .collect();
    let payloads: Vec<_> = (0..128u32)
        .map(|version| {
            let mut bytes = base.clone();
            bytes.extend_from_slice(&version.to_le_bytes());
            bytes
        })
        .collect();
    let inputs: Vec<_> = payloads
        .iter()
        .map(|data| PackObject {
            id: ObjectId::for_blob(ObjectFormat::Sha1, data),
            kind: ObjectKind::Blob,
            data,
        })
        .collect();
    let (mut pack, mut index) = (Vec::new(), Vec::new());
    let written = girt::write_pack_with_compression(
        ObjectFormat::Sha1,
        &inputs,
        &mut pack,
        &mut index,
        PackWriteLimits::default(),
        girt::PackCompression::Delta(girt::DeltaOptions::default()),
    )
    .unwrap();
    assert!(written.deltas.entries > 100);
    let directory = repository.object_dir().join("pack");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("pack-{}", written.checksum));
    std::fs::write(path.with_extension("pack"), pack).unwrap();
    std::fs::write(path.with_extension("idx"), index).unwrap();
    // Original payloads and girt-written fixture; Git independently checks its framing and deltas.
    let relative = format!("objects/pack/pack-{}.idx", written.checksum);
    history_git::git(repository.git_dir(), &["verify-pack", &relative], b"");
    let policy = RetentionPolicy {
        heads: inputs.iter().map(|object| object.id).collect(),
        ..Default::default()
    };
    let cancel = AtomicBool::new(false);
    c.bench_function("repack/existing-deltas-128-blobs-64k", |bench| {
        bench.iter(|| {
            repository
                .repack_retained(
                    black_box(&policy),
                    RepackLimits::default(),
                    black_box(&cancel),
                )
                .unwrap()
        })
    });
}

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
    let repack = repository
        .repack_retained(&policy, RepackLimits::default(), &cancel)
        .unwrap();
    assert!(repack.written.objects >= 256);
    c.bench_function("repack/idempotent-packed-linear-256", |bench| {
        bench.iter(|| {
            repository.repack_retained(
                black_box(&policy),
                RepackLimits::default(),
                black_box(&cancel),
            )
        })
    });
    c.bench_function("prune/idempotent-packed-linear-256", |bench| {
        bench.iter(|| {
            repository.prune_unreachable_loose(
                &mut IsolatedFixture,
                black_box(&policy),
                black_box(&cancel),
            )
        })
    });
    c.bench_function("reflog-expire/noop-packed-linear-256", |bench| {
        bench.iter(|| {
            repository.expire_reflogs(&mut IsolatedFixture, black_box(&policy), black_box(&cancel))
        })
    });
    #[cfg(unix)]
    {
        repository
            .retire_old_packs(
                &mut IsolatedFixture,
                &policy,
                RepackLimits::default(),
                &cancel,
            )
            .unwrap();
        c.bench_function("pack-retire/idempotent-packed-linear-256", |bench| {
            bench.iter(|| {
                repository.retire_old_packs(
                    &mut IsolatedFixture,
                    black_box(&policy),
                    RepackLimits::default(),
                    black_box(&cancel),
                )
            })
        });
        c.bench_function("maintenance/idempotent-packed-linear-256", |bench| {
            bench.iter(|| {
                repository.run_maintenance(
                    &mut IsolatedFixture,
                    black_box(&policy),
                    RepackLimits::default(),
                    black_box(&cancel),
                )
            })
        });
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(6));
    targets = retention, existing_deltas
}
criterion_main!(benches);
