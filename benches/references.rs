use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use girt::refs::{Expected, RefEdit, RefName, Reflog, ReflogEntry, Target};
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
    let id = ObjectId::Sha1([1; 20]);
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
        std::fs::write(root.path().join("packed-refs"), &packed).unwrap();
        let last = RefName::new(format!("refs/tags/tag-{:05}", count - 1)).unwrap();
        let last_path = root.path().join(format!("refs/tags/tag-{:05}", count - 1));
        c.bench_function(&format!("references/read-packed-warm-{count}"), |b| {
            b.iter(|| refs.read(black_box(&last)).unwrap())
        });
        c.bench_function(&format!("references/enumerate-packed-warm-{count}"), |b| {
            b.iter(|| refs.list().unwrap())
        });
        c.bench_function(&format!("references/delete-shadowed-packed-{count}"), |b| {
            b.iter_batched(
                || {
                    std::fs::write(root.path().join("packed-refs"), &packed).unwrap();
                    std::fs::create_dir_all(root.path().join("refs/tags")).unwrap();
                    std::fs::write(&last_path, format!("{id}\n")).unwrap();
                },
                |()| {
                    refs.delete_without_reflog(&last, Expected::Value(Target::Direct(id)))
                        .unwrap()
                },
                BatchSize::PerIteration,
            )
        });
    }
    std::fs::remove_file(root.path().join("packed-refs")).unwrap();
    std::fs::create_dir_all(root.path().join("refs/tags")).unwrap();
    for index in 0..1000 {
        std::fs::write(
            root.path().join(format!("refs/tags/tag-{index:05}")),
            format!("{id}\n"),
        )
        .unwrap();
    }
    c.bench_function("references/enumerate-loose-warm-1000", |b| {
        b.iter(|| refs.list().unwrap())
    });
}
criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(2));
    targets = references, transactions
}
criterion_main!(benches);

fn transactions(c: &mut Criterion) {
    for count in [1, 32] {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            girt::ObjectFormat::Sha1,
            root.path().join("repo"),
            girt::InitKind::Bare,
        )
        .unwrap();
        let edits: Vec<_> = (0..count)
            .map(|index| RefEdit {
                name: RefName::new(format!("refs/tags/batch-{index:03}")).unwrap(),
                dereference: false,
                target: Some(Target::Direct(ObjectId::Sha1([1; 20]))),
                expected: Expected::Any,
                reflog: Reflog::Append {
                    committer: girt::Signature {
                        name: b"Benchmark".to_vec(),
                        email: b"bench@example.com".to_vec(),
                        seconds: 1700000000,
                        offset_minutes: 0,
                    },
                    message: b"benchmark publication".to_vec(),
                },
            })
            .collect();
        let refs = repo.references().unwrap();
        c.bench_function(&format!("transactions/publish-with-log-{count}"), |b| {
            b.iter_batched(
                || {
                    for edit in &edits {
                        let relative = std::str::from_utf8(edit.name.as_bytes()).unwrap();
                        let path = repo.git_dir().join("logs").join(relative);
                        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                        std::fs::write(path, b"").unwrap();
                    }
                },
                |()| refs.transaction(black_box(&edits)).unwrap(),
                BatchSize::PerIteration,
            )
        });
    }
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        girt::ObjectFormat::Sha1,
        root.path().join("detach"),
        girt::InitKind::Bare,
    )
    .unwrap();
    let refs = repo.references().unwrap();
    let head = RefName::new("HEAD").unwrap();
    let unborn = Target::Symbolic(RefName::new("refs/heads/main").unwrap());
    let detach = RefEdit {
        name: head.clone(),
        dereference: false,
        target: Some(Target::Direct(ObjectId::Sha1([1; 20]))),
        expected: Expected::Value(unborn.clone()),
        reflog: Reflog::Append {
            committer: girt::Signature {
                name: b"Benchmark".to_vec(),
                email: b"bench@example.com".to_vec(),
                seconds: 1700000000,
                offset_minutes: 0,
            },
            message: b"detach unborn HEAD".to_vec(),
        },
    };
    std::fs::create_dir(repo.git_dir().join("logs")).unwrap();
    c.bench_function("transactions/detach-unborn-with-log", |b| {
        b.iter_batched(
            || {
                refs.update_without_reflog(&head, unborn.clone(), Expected::Any)
                    .unwrap();
                std::fs::write(repo.git_dir().join("logs/HEAD"), b"").unwrap();
            },
            |()| {
                refs.transaction(black_box(std::slice::from_ref(&detach)))
                    .unwrap()
            },
            BatchSize::PerIteration,
        );
    });
    for count in [10, 10000] {
        let record = format!(
            "{} {} Benchmark <bench@example.com> 1700000000 +0000\tbenchmark publication\n",
            ObjectId::Sha1([1; 20]),
            ObjectId::Sha1([2; 20])
        );
        let bytes = record.repeat(count).into_bytes();
        c.bench_function(&format!("transactions/parse-log-{count}"), |b| {
            b.iter(|| ReflogEntry::parse(girt::ObjectFormat::Sha1, black_box(&bytes)).unwrap())
        });
    }
}
