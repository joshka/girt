use std::hint::black_box;
use std::io;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use girt::push::{ForcePolicy, PreparedPush, PushCommand, PushLimits, send};
use girt::refs::RefName;
use girt::{ObjectId, PackLimits, ReadLimits};

#[path = "../tests/support/pack_git.rs"]
mod pack_git;

fn packet(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend(format!("{:04x}", bytes.len() + 4).bytes());
    out.extend(bytes);
}
fn push(c: &mut Criterion) {
    let cancel = AtomicBool::new(false);
    let mut group = c.benchmark_group("push");
    for count in [16, 256] {
        let fixture = pack_git::Fixture::new(true, count);
        assert!(fixture.index_path.exists());
        let objects = fixture.repo.objects(PackLimits::default()).unwrap();
        assert!(
            objects
                .read(fixture.ordinary, ReadLimits::default())
                .unwrap()
                .is_some()
        );
        assert!(
            objects
                .read(fixture.delta, ReadLimits::default())
                .unwrap()
                .is_some()
        );
        let tag = fixture.records.last().unwrap().0;
        let command = PushCommand {
            name: RefName::new("refs/tags/packed").unwrap(),
            expected: None,
            new: tag,
            force: ForcePolicy::FastForwardOnly,
        };
        let prepared = PreparedPush::new(
            &objects,
            vec![command.clone()],
            PushLimits::default(),
            &cancel,
        )
        .unwrap();
        let payload: usize = fixture.records.iter().map(|r| r.2.len()).sum();
        eprintln!(
            "push-workload,{count},{},{payload},{}",
            prepared.object_count(),
            prepared.pack_bytes()
        );
        group.throughput(Throughput::Bytes(payload as u64));
        group.bench_function(format!("select-read-validate-pack-{count}"), |b| {
            b.iter(|| {
                PreparedPush::new(
                    black_box(&objects),
                    vec![command.clone()],
                    PushLimits::default(),
                    &cancel,
                )
                .unwrap()
            })
        });
        let old = fixture.records[fixture.records.len() - 2].0;
        let commit = girt::Commit::parse(
            objects
                .read(old, ReadLimits::default())
                .unwrap()
                .unwrap()
                .data(),
        )
        .unwrap();
        let mut fields = commit.fields().clone();
        fields.parents = vec![old];
        fields.message = b"Incremental transfer benchmark\n".to_vec();
        let next = fixture
            .repo
            .loose_objects()
            .unwrap()
            .write_commit(&girt::Commit::new(fields).unwrap())
            .unwrap();
        let update = PushCommand {
            name: RefName::new("refs/heads/main").unwrap(),
            expected: Some(old),
            new: next,
            force: ForcePolicy::FastForwardOnly,
        };
        let full = PreparedPush::new(
            &objects,
            vec![update.clone()],
            PushLimits::default(),
            &cancel,
        )
        .unwrap();
        let reduced = PreparedPush::new_excluding(
            &objects,
            vec![update.clone()],
            &[old],
            PushLimits::default(),
            &cancel,
        )
        .unwrap();
        eprintln!(
            "push-incremental,{count},{},{},{},{}",
            full.object_count(),
            full.pack_bytes(),
            reduced.object_count(),
            reduced.pack_bytes()
        );
        group.bench_function(format!("transfer-full-{count}"), |b| {
            b.iter(|| {
                PreparedPush::new(
                    black_box(&objects),
                    vec![update.clone()],
                    PushLimits::default(),
                    &cancel,
                )
                .unwrap()
            })
        });
        group.bench_function(format!("transfer-excluding-{count}"), |b| {
            b.iter(|| {
                PreparedPush::new_excluding(
                    black_box(&objects),
                    vec![update.clone()],
                    &[old],
                    PushLimits::default(),
                    &cancel,
                )
                .unwrap()
            })
        });
        let mut response = vec![];
        packet(
            &mut response,
            format!(
                "{} capabilities^{{}}\0report-status",
                ObjectId::Sha1([0; 20])
            )
            .as_bytes(),
        );
        response.extend(b"0000");
        packet(&mut response, b"unpack ok");
        packet(&mut response, b"ok refs/tags/packed");
        response.extend(b"0000");
        group.throughput(Throughput::Elements(1));
        group.bench_function(format!("replay-protocol-prepared-{count}"), |b| {
            b.iter(|| {
                send(
                    &mut black_box(response.as_slice()),
                    &mut io::sink(),
                    &prepared,
                    &cancel,
                )
                .unwrap()
            })
        });
        // Real local transport is checked once outside timing; no source refs are changed.
        let dest = tempfile::tempdir().unwrap();
        pack_git::git(
            dest.path(),
            &["init", "--bare", "--object-format=sha1", "--template=", "."],
            b"",
        );
        assert!(
            girt::push::send_local(dest.path(), &prepared, &cancel)
                .unwrap()
                .all_succeeded()
        );
        assert!(fixture.root.path().exists());
    }
    group.finish();
}
criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(5));
    targets = push
}
criterion_main!(benches);
