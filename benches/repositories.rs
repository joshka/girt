use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use girt::{Config, Repository};

fn repositories(c: &mut Criterion) {
    let small = b"[core]\nrepositoryformatversion = 0\nbare = true\n";
    let mut large = small.to_vec();
    for index in 0..1000 {
        large.extend_from_slice(format!("[remote \"remote-{index}\"]\nurl = https://example.com/repository-{index}\nfetch = +refs/heads/*:refs/remotes/remote-{index}/*\n").as_bytes());
    }
    c.bench_function("config/parse-small", |b| {
        b.iter(|| Config::parse(black_box(small)).unwrap())
    });
    c.bench_function("config/parse-1000-remotes", |b| {
        b.iter(|| Config::parse(black_box(&large)).unwrap())
    });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("objects")).unwrap();
    std::fs::create_dir(root.path().join("refs")).unwrap();
    std::fs::write(root.path().join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    std::fs::write(root.path().join("config"), small).unwrap();
    c.bench_function("repository/open-bare-warm", |b| {
        b.iter(|| Repository::open(black_box(root.path())).unwrap())
    });
}
criterion_group!(benches, repositories);
criterion_main!(benches);
