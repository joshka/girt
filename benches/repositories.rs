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
    std::fs::write(root.path().join("large"), &large).unwrap();
    let large_inputs = girt::config::ConfigInputs {
        files: vec![girt::config::ConfigFile {
            path: root.path().join("large"),
            scope: girt::config::ConfigScope::Local,
            optional: false,
        }],
        ..Default::default()
    };
    c.bench_function("config/resolve-1000-remotes-warm", |b| {
        b.iter(|| Config::resolve(black_box(&large_inputs)).unwrap())
    });
    for depth in 0..10 {
        std::fs::write(
            root.path().join(format!("depth-{depth}")),
            format!(
                "[include]\npath=depth-{}\n[demo]\nvalue={depth}\n",
                depth + 1
            ),
        )
        .unwrap();
    }
    std::fs::write(root.path().join("depth-10"), b"[demo]\nvalue=end\n").unwrap();
    let deep_inputs = girt::config::ConfigInputs {
        files: vec![girt::config::ConfigFile {
            path: root.path().join("depth-0"),
            scope: girt::config::ConfigScope::Local,
            optional: false,
        }],
        ..Default::default()
    };
    c.bench_function("config/resolve-depth-10-warm", |b| {
        b.iter(|| Config::resolve(black_box(&deep_inputs)).unwrap())
    });
    c.bench_function("repository/open-bare-warm", |b| {
        b.iter(|| Repository::open(black_box(root.path())).unwrap())
    });
}
criterion_group!(benches, repositories);
criterion_main!(benches);
