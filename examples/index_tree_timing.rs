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
        let entries = (0..count)
            .map(|n| {
                Entry::new(
                    format!("dir-{:04}/nested/file-{n:08}", n / 100).into_bytes(),
                    Mode::Regular,
                    blob,
                )
            })
            .collect();
        let index = Index::new(ObjectFormat::Sha1, entries, Limits::default())?;
        std::fs::write(
            repo.git_dir().join("index"),
            index.encode(Limits::default())?,
        )?;
        println!("{}", index.write_tree(&repo)?);
        return Ok(());
    }
    let repo = Repository::open(path)?;
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
        _ => panic!("expected fixture, tree-id, checked or profile"),
    };
    eprintln!("api_ns={}", start.elapsed().as_nanos());
    println!("{id}");
    Ok(())
}
