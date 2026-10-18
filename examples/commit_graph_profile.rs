//! Phase timings for object-backed and explicitly indexed history queries.
//!
//! Opening includes default graph attachment. The reader then drops that snapshot to measure
//! explicit parsing/attachment separately; object mode queries read commits directly.
//!
//! Usage: `commit_graph_profile REPO TIP BASE objects|indexed|verified walk|range|ancestor`.
use std::time::Instant;

use girt::{HistoryLimits, ObjectId, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("repository path required")?;
    let tip: ObjectId = args.next().ok_or("tip ID required")?.parse()?;
    let base: ObjectId = args.next().ok_or("base ID required")?.parse()?;
    let trust = args.next().ok_or("objects, indexed or verified required")?;
    let query = args.next().ok_or("walk, range or ancestor required")?;
    let start = Instant::now();
    let repo = Repository::open(path)?;
    let mut objects = repo.objects()?.without_commit_graph();
    eprintln!("open={:?}", start.elapsed());
    if trust != "objects" {
        let start = Instant::now();
        let graph = repo.commit_graph()?.ok_or("commit-graph required")?;
        eprintln!(
            "graph_parse={:?} commits={}",
            start.elapsed(),
            graph.commits().len()
        );
        if trust == "verified" {
            let start = Instant::now();
            graph.verify(&objects)?;
            eprintln!("object_verification={:?}", start.elapsed());
        } else if trust != "indexed" {
            return Err("unknown trust choice".into());
        }
        let start = Instant::now();
        objects = objects.with_commit_graph(graph)?;
        eprintln!("attachment={:?}", start.elapsed());
    }
    let start = Instant::now();
    let limits = HistoryLimits::trusted();
    match query.as_str() {
        "walk" => println!("{}", objects.walk_with(&[tip], limits)?.len()),
        "range" => println!(
            "{}",
            objects.walk_range_with(&[tip], &[base], limits)?.len()
        ),
        "ancestor" => println!("{}", objects.is_ancestor_with(base, tip, limits)?),
        _ => return Err("unknown query".into()),
    }
    eprintln!("query={:?}", start.elapsed());
    Ok(())
}
