//! Read a loose blob by explicit repository path and full format-bearing ID.
//! Run `cargo run --example open_repository -- /path/to/repo <blob-id>`.
use std::path::PathBuf;

use clap::Parser;
use girt::{ObjectId, Repository};

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Repository to open.
    path: PathBuf,
    /// Full format-bearing ID of the loose blob.
    blob_id: ObjectId,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let repository = Repository::open(args.path)?;
    let bytes = repository
        .loose_objects()
        .read_blob(args.blob_id, 16 * 1024 * 1024)?;
    println!(
        "Read {} bytes from {}",
        bytes.len(),
        repository.object_dir().display()
    );
    Ok(())
}
