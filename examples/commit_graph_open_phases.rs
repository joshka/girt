//! Phase timings for loading a repository's commit-graph.
//! Usage: `commit_graph_open_phases REPO [ROUNDS]`.
use std::time::Instant;

use girt::commit_graph::CommitGraph;
use girt::{PackLimits, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("repository path required")?;
    let rounds: usize = args.next().map_or(Ok(3), |r| r.parse())?;
    for _ in 0..rounds {
        let start = Instant::now();
        let repo = Repository::open(&path)?;
        let objects = repo
            .objects_with(PackLimits::trusted())?
            .without_commit_graph();
        let opened = start.elapsed();
        let start = Instant::now();
        let bytes = std::fs::read(repo.object_dir().join("info/commit-graph"))?;
        let read = start.elapsed();
        let start = Instant::now();
        let graph = CommitGraph::parse(&bytes, repo.object_format())?;
        let parsed = start.elapsed();
        let start = Instant::now();
        let objects = objects.with_commit_graph(graph)?;
        let attached = start.elapsed();
        let start = Instant::now();
        drop(objects);
        let dropped = start.elapsed();
        let start = Instant::now();
        let full = Repository::open(&path)?.objects_with(PackLimits::trusted())?;
        let total = start.elapsed();
        drop(full);
        eprintln!(
            "repo+packs={opened:?} read={read:?} parse={parsed:?} attach={attached:?} drop={dropped:?} default_open={total:?}"
        );
    }
    Ok(())
}
