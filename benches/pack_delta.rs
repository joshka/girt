//! Original workloads: byte edits, shifted binary data, generated source, independent noise,
//! repeated bytes, tiny values, and oversized inputs. Fixture construction is outside sampling.
use std::hint::black_box;
use std::io;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use girt::{
    DeltaOptions, ObjectId, ObjectKind, PackCompression, PackObject, PackWriteLimits,
    write_pack_with_compression,
};

fn noise(size: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..size)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn workload(label: &str) -> Vec<Vec<u8>> {
    (0..16)
        .map(|variant| match label {
            "edited-binary" => {
                let mut bytes = noise(65536, 0x123456789);
                bytes[variant * 3079] ^= 255;
                bytes
            }
            "shifted-binary" => {
                let mut bytes = noise(65536, 0x123456789);
                bytes.splice(variant * 3079..variant * 3079 + 3, [variant as u8; 19]);
                bytes
            }
            "source" => (0..512)
                .map(|line| {
                    format!(
                        "pub const VALUE_{line}: u32 = {};\n",
                        line + usize::from(line == variant) * 1000
                    )
                })
                .collect::<String>()
                .into_bytes(),
            "independent" => noise(65536, 0x123456789 + variant as u64),
            "repeated" => {
                let mut bytes = vec![b'x'; 65536];
                bytes[..8].copy_from_slice(&(variant as u64).to_le_bytes());
                bytes
            }
            "oversized" => noise(1048577, 0x123456789 + variant as u64),
            "tiny" => (variant as u64).to_le_bytes().to_vec(),
            _ => unreachable!(),
        })
        .collect()
}

fn pack_delta(c: &mut Criterion) {
    let mut group = c.benchmark_group("pack-delta");
    for label in [
        "edited-binary",
        "shifted-binary",
        "source",
        "independent",
        "repeated",
        "oversized",
        "tiny",
    ] {
        group.measurement_time(Duration::from_secs(if label == "oversized" {
            8
        } else {
            2
        }));
        let data = workload(label);
        let objects: Vec<_> = data
            .iter()
            .map(|data| PackObject {
                id: ObjectId::for_blob(girt::ObjectFormat::Sha1, data),
                kind: ObjectKind::Blob,
                data,
            })
            .collect();
        let input_bytes: usize = data.iter().map(Vec::len).sum();
        group.throughput(Throughput::Bytes(input_bytes as u64));
        for (policy, compression) in [
            ("ordinary", PackCompression::Ordinary),
            ("delta", PackCompression::Delta(DeltaOptions::default())),
        ] {
            let written = write_pack_with_compression(
                girt::ObjectFormat::Sha1,
                &objects,
                &mut io::sink(),
                &mut io::sink(),
                PackWriteLimits::default(),
                compression,
            )
            .unwrap();
            eprintln!(
                "delta-size,{label},{policy},{input_bytes},{},{},{},{},{}",
                written.pack_bytes,
                written.deltas.entries,
                written.deltas.candidates,
                written.deltas.work,
                written.deltas.max_depth
            );
            group.bench_with_input(BenchmarkId::new(label, policy), &objects, |b, objects| {
                b.iter(|| {
                    write_pack_with_compression(
                        girt::ObjectFormat::Sha1,
                        black_box(objects),
                        &mut io::sink(),
                        &mut io::sink(),
                        PackWriteLimits::default(),
                        compression,
                    )
                    .unwrap()
                })
            });
        }
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(2));
    targets = pack_delta
}
criterion_main!(benches);
