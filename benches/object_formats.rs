//! Dual-format hashing, owned codecs and warm loose storage; fixture setup is outside sampling.
use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use girt::{
    Commit, EntryMode, LooseObjects, ObjectFormat, ObjectId, ObjectKind, Tag, Tree, TreeEntry,
};

fn formats(c: &mut Criterion) {
    let mut group = c.benchmark_group("object_formats");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(1));
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let label = format.to_string();
        let bytes: Vec<_> = (0..65536)
            .map(|i| ((i * 37 + i / 251) % 256) as u8)
            .collect();
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_function(BenchmarkId::new("hash_blob_64KiB", &label), |b| {
            b.iter(|| format.hash_object(ObjectKind::Blob, black_box(&bytes)));
        });
        let entries = (0u32..256)
            .map(|i| TreeEntry {
                mode: EntryMode::Blob,
                name: format!("file-{i:04}").into_bytes(),
                id: ObjectId::for_blob(format, &i.to_le_bytes()),
            })
            .collect();
        let tree = Tree::new(format, entries).unwrap();
        let payload = tree.encode();
        group.throughput(Throughput::Bytes(payload.len() as u64));
        group.bench_function(BenchmarkId::new("parse_tree_256", &label), |b| {
            b.iter(|| Tree::parse(format, black_box(&payload)).unwrap());
        });
        group.bench_function(BenchmarkId::new("encode_tree_256", &label), |b| {
            b.iter(|| black_box(&tree).encode());
        });
        let commit = format!(
            "tree {}\nauthor A <a> -1 +0000\ncommitter C <c> 1 +0000\ngpgsig opaque\n folded\n\nmessage\n",
            tree.id()
        );
        group.throughput(Throughput::Bytes(commit.len() as u64));
        group.bench_function(BenchmarkId::new("parse_commit_graph", &label), |b| {
            b.iter(|| Commit::parse(format, black_box(commit.as_bytes())).unwrap());
        });
        group.bench_function(BenchmarkId::new("parse_commit_and_fields", &label), |b| {
            b.iter(|| {
                Commit::parse(format, black_box(commit.as_bytes()))
                    .unwrap()
                    .to_fields()
                    .unwrap()
            });
        });
        let tag = format!(
            "object {}\ntype tree\ntag v1\ntagger A <a> 1 +0000\n\nmessage\n",
            tree.id()
        );
        group.throughput(Throughput::Bytes(tag.len() as u64));
        group.bench_function(BenchmarkId::new("parse_tag_target", &label), |b| {
            b.iter(|| Tag::parse(format, black_box(tag.as_bytes())).unwrap());
        });
        group.bench_function(BenchmarkId::new("parse_tag_and_fields", &label), |b| {
            b.iter(|| {
                Tag::parse(format, black_box(tag.as_bytes()))
                    .unwrap()
                    .to_fields()
                    .unwrap()
            });
        });
        let directory = tempfile::tempdir().unwrap();
        let objects = LooseObjects::new(directory.path(), format);
        let id = objects.write_blob(&bytes).unwrap();
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_function(BenchmarkId::new("read_warm_blob_64KiB", &label), |b| {
            b.iter(|| objects.read_blob(black_box(id), bytes.len()).unwrap());
        });
        group.bench_function(BenchmarkId::new("write_existing_blob_64KiB", &label), |b| {
            b.iter(|| objects.write_blob(black_box(&bytes)).unwrap());
        });
        group.bench_function(BenchmarkId::new("publish_new_blob_64KiB", &label), |b| {
            b.iter_batched(
                || tempfile::tempdir().unwrap(),
                |directory| {
                    let objects = LooseObjects::new(directory.path(), format);
                    let id = objects.write_blob(black_box(&bytes)).unwrap();
                    (directory, id)
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}
criterion_group!(benches, formats, storage);
criterion_main!(benches);

/// Warm snapshots, decoded payloads and caller-owned writes; no fixture construction is timed.
fn storage(c: &mut Criterion) {
    use girt::index::{Entry, Index, Limits, Mode};
    use girt::{
        InitKind, PackCompression, PackLimits, PackObject, PackWriteLimits, ReadLimits, Repository,
        write_pack_with_compression,
    };

    let mut group = c.benchmark_group("storage_formats");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(1));
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let label = format.to_string();
        let payloads: Vec<Vec<u8>> = (0..16)
            .map(|variant| {
                let mut state = 123456789u64;
                let mut bytes: Vec<_> = (0..16384)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        state as u8
                    })
                    .collect();
                bytes[variant * 100] ^= 255;
                bytes
            })
            .collect();
        let inputs: Vec<_> = payloads
            .iter()
            .map(|data| PackObject {
                id: ObjectId::for_blob(format, data),
                kind: ObjectKind::Blob,
                data,
            })
            .collect();
        group.throughput(Throughput::Bytes(16 * 16384));
        for (policy_name, policy) in [
            ("write_ordinary", PackCompression::Ordinary),
            ("write_delta", PackCompression::Delta(Default::default())),
        ] {
            group.bench_function(BenchmarkId::new(policy_name, &label), |b| {
                b.iter(|| {
                    write_pack_with_compression(
                        format,
                        black_box(&inputs),
                        &mut std::io::sink(),
                        &mut std::io::sink(),
                        PackWriteLimits::default(),
                        policy,
                    )
                    .unwrap()
                });
            });
        }
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
        let (mut pack, mut index) = (vec![], vec![]);
        let written = write_pack_with_compression(
            format,
            &inputs,
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
            PackCompression::Delta(Default::default()),
        )
        .unwrap();
        assert!(written.deltas.entries > 0);
        let base = repo
            .object_dir()
            .join("pack")
            .join(format!("pack-{}", written.checksum));
        std::fs::write(base.with_extension("pack"), &pack).unwrap();
        std::fs::write(base.with_extension("idx"), &index).unwrap();
        let objects = repo.objects(PackLimits::default()).unwrap();
        eprintln!(
            "storage_formats/{label}: 16x16384 bytes, pack={}, index={}, deltas={}, max_depth={}",
            pack.len(),
            index.len(),
            written.deltas.entries,
            written.deltas.max_depth
        );
        group.throughput(Throughput::Bytes((pack.len() + index.len()) as u64));
        group.bench_function(BenchmarkId::new("open_validate_warm", &label), |b| {
            b.iter(|| repo.objects(black_box(PackLimits::default())).unwrap());
        });
        group.throughput(Throughput::Bytes(16 * 16384));
        group.bench_function(BenchmarkId::new("read_all_delta_warm", &label), |b| {
            b.iter(|| {
                for input in &inputs {
                    black_box(
                        objects
                            .read(input.id, ReadLimits::default())
                            .unwrap()
                            .unwrap(),
                    );
                }
            });
        });
        let entries = (0..10000)
            .map(|n| {
                Entry::new(
                    format!("directory/file-{n:08}").into_bytes(),
                    Mode::Regular,
                    inputs[0].id,
                )
            })
            .collect();
        let index = Index::new(format, entries, Limits::default()).unwrap();
        let bytes = index.encode(Limits::default()).unwrap();
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_function(BenchmarkId::new("parse_index_10000", &label), |b| {
            b.iter(|| Index::parse(format, black_box(&bytes), Limits::default()).unwrap());
        });
        group.bench_function(BenchmarkId::new("encode_index_10000", &label), |b| {
            b.iter(|| black_box(&index).encode(Limits::default()).unwrap());
        });
        let record = format!(
            "{} {} A <a@example.invalid> 1700000000 +0000\tupdate\n",
            ObjectId::null(format),
            inputs[0].id
        );
        let log = record.repeat(1000);
        group.throughput(Throughput::Bytes(log.len() as u64));
        group.bench_function(BenchmarkId::new("parse_reflog_1000", &label), |b| {
            b.iter(|| girt::refs::ReflogEntry::parse(format, black_box(log.as_bytes())).unwrap());
        });
        #[cfg(unix)]
        {
            let refs = repo.references().unwrap();
            for n in 0..128 {
                let name = girt::refs::RefName::new(format!("refs/heads/branch-{n:04}")).unwrap();
                refs.update_without_reflog(
                    &name,
                    girt::refs::Target::Direct(inputs[0].id),
                    girt::refs::Expected::Absent,
                )
                .unwrap();
            }
            group.throughput(Throughput::Elements(128));
            group.bench_function(BenchmarkId::new("list_refs_128_warm", &label), |b| {
                b.iter(|| refs.list().unwrap());
            });
        }
    }
    group.finish();
}
