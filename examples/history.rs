//! Run `cargo run --example history -- /path/to/repo <commit-id> [other-commit-id]`.
use std::path::PathBuf;

use clap::Parser;
use girt::{ObjectId, Repo};

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
    let git = Repo::discover(args.path)?;
    for entry in git.log(root)? {
        let (id, _) = entry?;
        println!("{id}");
    }
    if let Some(other) = args.other_commit {
        let objects = git.repository().objects()?;
        println!("ancestor: {}", objects.is_ancestor(root, other)?);
        println!("merge bases: {:?}", objects.merge_bases(root, other)?);
    }
    Ok(())
}
