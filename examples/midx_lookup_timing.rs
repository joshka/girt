//! Time verified reads of an explicit object list from a repository containing many packs.
//! Generate the repository and list independently, then run with MIDX enabled and disabled.
use std::hint::black_box;
use std::time::Instant;

use girt::{ObjectId, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("expected repository path")?;
    let list = args.next().ok_or("expected object-ID list path")?;
    let rounds: usize = args.next().ok_or("expected number of rounds")?.parse()?;
    let ids = std::fs::read_to_string(list)?
        .lines()
        .map(str::parse)
        .collect::<Result<Vec<ObjectId>, _>>()?;
    let repo = Repository::open(path)?;
    let start = Instant::now();
    let objects = repo.objects()?;
    let opening = start.elapsed();
    let start = Instant::now();
    for _ in 0..rounds {
        for id in &ids {
            black_box(objects.read(*id)?.ok_or("object absent")?);
        }
    }
    println!(
        "objects={} rounds={rounds} open_ns={} read_ns={}",
        ids.len(),
        opening.as_nanos(),
        start.elapsed().as_nanos(),
    );
    Ok(())
}
