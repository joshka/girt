use std::hint::black_box;
use std::io;
use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use girt::fetch::{FetchLimits, receive};
use girt::transport::TransportControl;
use girt::{PackLimits, ReadLimits};

#[path = "../tests/support/pack_git.rs"]
mod pack_git;

fn packet(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend(format!("{:04x}", bytes.len() + 4).bytes());
    out.extend(bytes);
}

fn fetch(c: &mut Criterion) {
    let cancel = AtomicBool::new(false);
    let mut group = c.benchmark_group("fetch");
    for count in [16, 256] {
        let fixture = pack_git::Fixture::new(true, count);
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
        let pack = std::fs::read(fixture.index_path.with_extension("pack")).unwrap();
        let tag = fixture.records.last().unwrap().0;
        let mut response = Vec::new();
        packet(
            &mut response,
            format!("{tag} refs/tags/packed\0side-band-64k ofs-delta\n").as_bytes(),
        );
        response.extend(b"0000");
        packet(&mut response, b"NAK\n");
        for chunk in pack.chunks(65515) {
            let mut band = vec![1];
            band.extend(chunk);
            packet(&mut response, &band);
        }
        response.extend(b"0000");
        let payload_bytes: usize = fixture.records.iter().map(|record| record.2.len()).sum();
        eprintln!(
            "fetch-workload,{count},{},{payload_bytes},{}",
            pack.len(),
            response.len()
        );
        group.throughput(Throughput::Bytes(payload_bytes as u64));
        group.bench_function(
            format!("replay-protocol-import-connectivity-{count}"),
            |b| {
                b.iter(|| {
                    receive(
                        &mut black_box(response.as_slice()),
                        &mut io::sink(),
                        |_| vec![tag],
                        FetchLimits::default(),
                        &cancel,
                        |_| ControlFlow::Continue(()),
                    )
                    .unwrap()
                })
            },
        );
        // Exercise a real local upload-pack once outside sampling to retain transport provenance.
        let received = girt::fetch::receive_local(
            fixture.root.path(),
            |_| vec![tag],
            FetchLimits::default(),
            &cancel,
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
        assert_eq!(received.object_count(), fixture.records.len());
        group.bench_function(format!("local-process-import-connectivity-{count}"), |b| {
            b.iter(|| {
                girt::fetch::receive_local_with_control(
                    fixture.root.path(),
                    |_| vec![tag],
                    FetchLimits::default(),
                    TransportControl {
                        cancel: &cancel,
                        deadline: Some(Instant::now() + Duration::from_secs(30)),
                    },
                    |_| ControlFlow::Continue(()),
                )
                .unwrap()
            })
        });
    }
    let id = girt::ObjectId::for_blob(b"advertisement benchmark");
    let mut advertisement = Vec::new();
    for i in 0..10_000 {
        let caps = if i == 0 { "\0side-band-64k" } else { "" };
        packet(
            &mut advertisement,
            format!("{id} refs/heads/branch-{i:05}{caps}\n").as_bytes(),
        );
    }
    advertisement.extend(b"0000");
    group.throughput(Throughput::Elements(10_000));
    group.bench_function("advertisement-select-none-10000", |b| {
        b.iter(|| {
            receive(
                &mut black_box(advertisement.as_slice()),
                &mut io::sink(),
                |_| vec![],
                FetchLimits::default(),
                &cancel,
                |_| ControlFlow::Continue(()),
            )
            .unwrap()
        })
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(5));
    targets = fetch
}
criterion_main!(benches);
