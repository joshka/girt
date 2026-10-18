//! Benchmarks raw working-tree status on clean and changed trees, with and without cached stat
//! data in the index.

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
            repo.raw_status_with(
                Baseline::Tree(Some(tree_id)),
                Untracked::Omit,
                &girt::pathspec::Pathspec::all(),
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

/// Status walks only macOS and Linux working trees, and stat words come from `MetadataExt`.
#[cfg(not(unix))]
fn stat_cached(_: &mut Criterion) {}

/// A clean tree whose index caches each file's stat words, so status trusts them instead of
/// reading files. Files spread over directories, as the parallel walk expects.
#[cfg(unix)]
fn stat_cached(c: &mut Criterion) {
    use std::os::unix::fs::MetadataExt as _;

    let mut group = c.benchmark_group("raw_status_stat_cached");
    for (directories, files) in [(10, 100), (100, 100)] {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            girt::ObjectFormat::Sha1,
            temp.path().join("repo"),
            InitKind::Worktree,
        )
        .unwrap();
        let root = repo.worktree().unwrap();
        let bytes = vec![b'x'; 1024];
        let id = repo.loose_objects().write_blob(&bytes).unwrap();
        // Old mtimes keep the entries from being smudged as racily clean when the index is written.
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        let mut entries = Vec::new();
        for directory in 0..directories {
            std::fs::create_dir(root.join(format!("dir-{directory:04}"))).unwrap();
            for file in 0..files {
                let path = format!("dir-{directory:04}/file-{file:04}");
                let full = root.join(&path);
                std::fs::write(&full, &bytes).unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(&full)
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
                let metadata = std::fs::symlink_metadata(&full).unwrap();
                let time = |seconds: i64, nanoseconds: i64| girt::index::Timestamp {
                    seconds: seconds as u32,
                    nanoseconds: nanoseconds as u32,
                };
                let stat = girt::index::Stat {
                    ctime: time(metadata.ctime(), metadata.ctime_nsec()),
                    mtime: time(metadata.mtime(), metadata.mtime_nsec()),
                    device: metadata.dev() as u32,
                    inode: metadata.ino() as u32,
                    uid: metadata.uid(),
                    gid: metadata.gid(),
                    size: metadata.size() as u32,
                };
                entries.push(Entry::new(path.into_bytes(), Mode::Regular, id).with_stat(stat));
            }
        }
        let mut edit = repo.edit_index(Default::default()).unwrap();
        edit.replace_entries(entries).unwrap();
        edit.commit().unwrap();
        // Each ctime is now; an index written later trusts it.
        std::fs::File::options()
            .write(true)
            .open(repo.git_dir().join("index"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2))
            .unwrap();
        let cancel = AtomicBool::new(false);
        let run = || {
            repo.raw_status_with(
                Baseline::Tree(None),
                Untracked::Omit,
                &girt::pathspec::Pathspec::all(),
                Limits::default(),
                &cancel,
            )
            .unwrap()
        };
        assert!(run().unstaged.is_empty());
        group.bench_function(
            BenchmarkId::new("clean_worktree", directories * files),
            |b| b.iter(|| black_box(run())),
        );
    }
    group.finish();
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "status/caches.rs"]
mod caches;
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn cached_status(c: &mut Criterion) {
    caches::benchmark(c);
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn cached_status(_: &mut Criterion) {}
criterion_group!(benches, benchmark, stat_cached, cached_status);
criterion_main!(benches);
