//! Time verified reads of an explicit object list from a repository containing many packs.
//! Generate the repository and list independently, then run with MIDX enabled and disabled.
//! Arguments: repository, ID list, rounds, optional mode (`read`, `contains`, `index-scan`,
//! `midx`). Use separate present/absent lists and roughly 100 packs for lookup-only comparisons.
//! `midx` checks the parsed MIDX alone; it omits candidate verification and loose/outside-pack
//! fallback, so compare it to `index-scan` as an index-search microbenchmark, not an end-to-end
//! guarantee.
use std::hint::black_box;
use std::time::Instant;

use girt::{ObjectId, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("expected repository path")?;
    let list = args.next().ok_or("expected object-ID list path")?;
    let rounds: usize = args.next().ok_or("expected number of rounds")?.parse()?;
    let mode = args.next().unwrap_or_else(|| "read".to_owned());
    let ids = std::fs::read_to_string(list)?
        .lines()
        .map(str::parse)
        .collect::<Result<Vec<ObjectId>, _>>()?;
    let repo = Repository::open(path)?;
    let start = Instant::now();
    let objects = repo.objects()?;
    let opening = start.elapsed();
    let packs: Vec<_> = objects.packs().collect();
    let bytes = if mode == "midx" {
        std::fs::read(repo.object_dir().join("pack/multi-pack-index"))?
    } else {
        Vec::new()
    };
    let midx = if mode == "midx" {
        Some(girt::multi_pack_index::MultiPackIndex::parse(
            &bytes,
            repo.object_format(),
        )?)
    } else {
        None
    };
    let start = Instant::now();
    let mut found = 0_usize;
    for _ in 0..rounds {
        for id in &ids {
            let present = match mode.as_str() {
                "read" => {
                    black_box(objects.read(*id)?.ok_or("object absent")?);
                    true
                }
                "contains" => objects.contains(*id)?,
                "index-scan" => scan(&packs, *id)?,
                "midx" => midx.as_ref().ok_or("index absent")?.find(*id).is_some(),
                _ => return Err("expected read, contains, index-scan or midx".into()),
            };
            found += usize::from(black_box(present));
        }
    }
    println!(
        "objects={} rounds={rounds} mode={mode} found={found} open_ns={} read_ns={}",
        ids.len(),
        opening.as_nanos(),
        start.elapsed().as_nanos(),
    );
    Ok(())
}

fn scan(packs: &[girt::IndexedPack<'_>], id: ObjectId) -> Result<bool, girt::ObjectReadError> {
    for pack in packs {
        if pack.contains(id)? {
            return Ok(true);
        }
    }
    Ok(false)
}
