//! Run `cargo run --example history -- /path/to/repo <commit-id> [other-commit-id]`.
use std::path::PathBuf;

use clap::Parser;
use girt::{ObjectId, Repository};

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Repository to open.
    path: PathBuf,
    /// Commit to walk history from.
    commit: ObjectId,
    /// Second commit for ancestor and merge-base queries.
    other_commit: Option<ObjectId>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let root = args.commit;
    let repo = Repository::open(args.path)?;
    let objects = repo.objects()?;
    for id in objects.walk(&[root])? {
        println!("{id}");
    }
    if let Some(other) = args.other_commit {
        println!("ancestor: {}", objects.is_ancestor(root, other)?);
        println!("merge bases: {:?}", objects.merge_bases(root, other)?);
    }
    Ok(())
}
