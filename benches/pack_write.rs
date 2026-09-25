use std::hint::black_box;
use std::io;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use girt::{ObjectId, ObjectKind, PackObject, PackWriteLimits, write_pack};

fn payload(size: usize, random: bool, variant: usize) -> Vec<u8> {
    let mut state = 0x1234_5678_9abc_def0u64 + variant as u64;
    let mut data: Vec<_> = (0..size)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            if random { state as u8 } else { b'x' }
        })
        .collect();
    data[..8].copy_from_slice(&(variant as u64).to_le_bytes());
    data
}

fn pack_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("pack-write");
    for (count, size) in [(64, 4096), (4, 1048576)] {
        for (label, random) in [("similar", false), ("pseudorandom", true)] {
            let data: Vec<_> = (0..count).map(|i| payload(size, random, i)).collect();
            let objects: Vec<_> = data
                .iter()
                .map(|data| PackObject {
                    id: ObjectId::for_blob(girt::ObjectFormat::Sha1, data),
                    kind: ObjectKind::Blob,
                    data,
                })
                .collect();
            let written = write_pack(
                girt::ObjectFormat::Sha1,
                &objects,
                &mut io::sink(),
                &mut io::sink(),
                PackWriteLimits::default(),
            )
            .unwrap();
            eprintln!(
                "pack-size,{count}x{size}-{label},{},{},{}",
                count * size,
                written.pack_bytes,
                written.index_bytes
            );
            group.throughput(Throughput::Bytes((count * size) as u64));
            group.bench_with_input(
                BenchmarkId::new(label, format!("{count}x{size}")),
                &objects,
                |b, objects| {
                    b.iter(|| {
                        write_pack(
                            girt::ObjectFormat::Sha1,
                            black_box(objects),
                            &mut io::sink(),
                            &mut io::sink(),
                            PackWriteLimits::default(),
                        )
                        .unwrap()
                    })
                },
            );
        }
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(5));
    targets = pack_write
}
criterion_main!(benches);
