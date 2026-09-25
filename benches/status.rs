use std::hint::black_box;
use std::sync::atomic::AtomicBool;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use girt::index::{Entry, Mode};
use girt::status::{Baseline, Limits, Untracked};
use girt::{EntryMode, InitKind, Repository, Tree, TreeEntry};

fn benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("raw_status");
    for (count, changed) in [(100, false), (100, true), (1_000, false), (1_000, true)] {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            girt::ObjectFormat::Sha1,
            temp.path().join("repo"),
            InitKind::Worktree,
        )
        .unwrap();
        let bytes = vec![b'x'; 1024];
        let id = repo.loose_objects().write_blob(&bytes).unwrap();
        let mut entries = Vec::new();
        for n in 0..count {
            let path = format!("file-{n:06}");
            let work = if changed && n % 10 == 0 {
                b"changed"
            } else {
                bytes.as_slice()
            };
            std::fs::write(repo.worktree().unwrap().join(&path), work).unwrap();
            entries.push(Entry::new(path.into_bytes(), Mode::Regular, id));
        }
        let tree = Tree::new(
            girt::ObjectFormat::Sha1,
            entries
                .iter()
                .map(|entry| TreeEntry {
                    name: entry.path.clone(),
                    mode: EntryMode::Blob,
                    id: entry.id,
                })
                .collect(),
        )
        .unwrap();
        let tree_id = repo.loose_objects().write_tree(&tree).unwrap();
        let mut edit = repo.edit_index(Default::default()).unwrap();
        edit.replace_entries(entries).unwrap();
        edit.commit().unwrap();
        let cancel = AtomicBool::new(false);
        let run = || {
            repo.raw_status(
                Baseline::Tree(Some(tree_id)),
                Untracked::Omit,
                Limits::default(),
                &cancel,
            )
            .unwrap()
        };
        assert!(run().staged.is_empty());
        assert_eq!(run().unstaged.len(), if changed { count / 10 } else { 0 });
        let label = if changed {
            "ten_percent_changed"
        } else {
            "clean_worktree"
        };
        group.bench_function(BenchmarkId::new(label, count), |b| {
            b.iter(|| black_box(run()))
        });
    }
    group.finish();
}
criterion_group!(benches, benchmark);
criterion_main!(benches);
