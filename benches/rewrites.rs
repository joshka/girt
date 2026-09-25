use std::hint::black_box;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use girt::rewrites::{Error, Limits, Options};
use girt::{EntryMode, InitKind, ObjectFormat, ObjectId, PackLimits, Repository, Tree, TreeEntry};

struct Fixture {
    _root: tempfile::TempDir,
    repo: Repository,
    old: ObjectId,
    new: ObjectId,
}

impl Fixture {
    fn new(format: ObjectFormat, count: usize, edited: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
        let store = repo.loose_objects();
        let mut old = Vec::new();
        let mut new = Vec::new();
        for index in 0..count {
            let body = (0..24)
                .map(|line| format!("file {index:04} original line {line:04}\n"))
                .collect::<String>();
            let old_id = store.write_blob(body.as_bytes()).unwrap();
            let target = if edited {
                format!("{body}additional material for file {index:04}\n")
            } else {
                body
            };
            let new_id = store.write_blob(target.as_bytes()).unwrap();
            old.push(TreeEntry {
                name: format!("old-{index:04}").into_bytes(),
                mode: EntryMode::Blob,
                id: old_id,
            });
            new.push(TreeEntry {
                name: format!("new-{index:04}").into_bytes(),
                mode: EntryMode::Blob,
                id: new_id,
            });
        }
        let old = store.write_tree(&Tree::new(format, old).unwrap()).unwrap();
        let new = store.write_tree(&Tree::new(format, new).unwrap()).unwrap();
        Self {
            _root: root,
            repo,
            old,
            new,
        }
    }

    fn git(&self) -> Vec<u8> {
        let output = Command::new("git")
            .arg("-C")
            .arg(self._root.path().join("repo"))
            .args([
                "diff-tree",
                "-r",
                "--no-commit-id",
                "--name-status",
                "-z",
                "-M50%",
                "--no-rename-empty",
                "--diff-filter=RC",
                &self.old.to_string(),
                &self.new.to_string(),
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        output.stdout
    }

    fn adapter(&self) -> Vec<u8> {
        let output = Command::new("target/release/examples/rewrites")
            .arg(self._root.path().join("repo"))
            .arg(self.old.to_string())
            .arg(self.new.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
}

fn rewrites(c: &mut Criterion) {
    let mut group = c.benchmark_group("rewrites");
    group
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2));
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for (label, count, edited) in [("exact", 32, false), ("edited", 32, true)] {
            let f = Fixture::new(format, count, edited);
            let objects = f.repo.objects(PackLimits::default()).unwrap();
            let cancel = AtomicBool::new(false);
            let run = || {
                objects
                    .detect_rewrites(
                        Some(f.old),
                        Some(f.new),
                        Options::default(),
                        Limits::default(),
                        None,
                        &cancel,
                    )
                    .unwrap()
            };
            assert_eq!(run().len(), count);
            assert_eq!(f.git(), f.adapter());
            group.bench_function(format!("{format:?}/{label}/library"), |b| {
                b.iter(|| black_box(run()))
            });
            group.bench_function(format!("{format:?}/{label}/git-process"), |b| {
                b.iter(|| black_box(f.git()))
            });
            group.bench_function(format!("{format:?}/{label}/girt-process"), |b| {
                b.iter(|| black_box(f.adapter()))
            });
        }
        let f = Fixture::new(format, 1001, true);
        let objects = f.repo.objects(PackLimits::default()).unwrap();
        let cancel = AtomicBool::new(false);
        let run = || {
            objects.detect_rewrites(
                Some(f.old),
                Some(f.new),
                Options::default(),
                Limits::default(),
                None,
                &cancel,
            )
        };
        assert!(matches!(
            run(),
            Err(Error::Candidates {
                sources: 1001,
                targets: 1001,
                limit: 1000
            })
        ));
        group.bench_function(format!("{format:?}/candidate-limit-1001"), |b| {
            b.iter(|| black_box(run()))
        });
        let cancelled = AtomicBool::new(true);
        group.bench_function(format!("{format:?}/cancelled"), |b| {
            b.iter(|| {
                black_box(objects.detect_rewrites(
                    Some(f.old),
                    Some(f.new),
                    Options::default(),
                    Limits::default(),
                    None,
                    &cancelled,
                ))
            })
        });
    }
    group.finish();
}

criterion_group!(benches, rewrites);
criterion_main!(benches);
