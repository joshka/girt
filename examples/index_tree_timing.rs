//! Probe for `scripts/bench-index-tree.py`; build with `cargo build --release --example
//! index_tree_timing`. Original fixtures only. Timed commands print the root ID for cross-checking
//! with Git.
use std::time::Instant;

use girt::index::{Entry, Index, Limits, Mode};
use girt::{InitKind, ObjectFormat, Repository};

fn main() -> girt::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let mode = &args[1];
    let path = &args[2];

    if mode == "fixture" {
        let count: usize = args[3].parse().unwrap();
        let repo = Repository::init(ObjectFormat::Sha1, path, InitKind::Worktree)?;
        let blob = repo.loose_objects().write_blob(b"content\n")?;
        let distinct = args.get(4).is_some_and(|arg| arg == "distinct");

        let mut entries = Vec::with_capacity(count);
        for n in 0..count {
            let id = if distinct {
                repo.loose_objects()
                    .write_blob(format!("content {n}\n").as_bytes())?
            } else {
                blob
            };
            entries.push(Entry::new(
                format!("dir-{:04}/nested/file-{n:08}", n / 100).into_bytes(),
                Mode::Regular,
                id,
            ));
        }

        let index = Index::new(ObjectFormat::Sha1, entries, Limits::default())?;
        std::fs::write(
            repo.git_dir().join("index"),
            index.encode(Limits::default())?,
        )?;
        println!("{}", index.write_tree(&repo)?);
        return Ok(());
    }

    let repo = Repository::open(path)?;
    if mode == "decode" {
        // Index reading alone, repeated for sampling profilers.
        let start = Instant::now();
        for _ in 0..100 {
            repo.read_index(Limits::default())?.unwrap();
        }
        eprintln!("decode_ns={}", start.elapsed().as_nanos() / 100);
        return Ok(());
    }

    let index = repo.read_index(Limits::default())?.unwrap();
    let start = Instant::now();
    let id = match mode.as_str() {
        "tree-id" => index.tree_id().unwrap(),
        "checked" => index.write_tree(&repo)?,
        "profile" => {
            let mut id = index.write_tree(&repo)?;
            for _ in 0..100 {
                id = index.write_tree(&repo)?;
            }
            id
        }
        _ => panic!("expected fixture, decode, tree-id, checked or profile"),
    };

    eprintln!("api_ns={}", start.elapsed().as_nanos());
    println!("{id}");
    Ok(())
}
