//! Read exact bytes from an existing loose or packed repository without modifying it.
//!
//! Run `cargo run --example packed_repository -- /path/to/repo <object-id>` with a full SHA-1 or
//! SHA-256 identity matching the repository's format.
//! To prepare a disposable packed repository, create commits and run `git repack -ad` there.
use std::io::{self, Write};
use std::path::PathBuf;

use clap::Parser;
use girt::{ObjectId, PackLimits, ReadLimits, Repository};

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Repository to open.
    path: PathBuf,
    /// Full object identity matching the repository's object format.
    id: ObjectId,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let repository = Repository::open(args.path)?;
    let objects = repository.objects(PackLimits::default())?;
    let object = objects
        .read(args.id, ReadLimits::default())?
        .ok_or("object absent")?;
    eprintln!(
        "{} {} ({} bytes)",
        object.kind().as_str(),
        object.id(),
        object.data().len()
    );
    io::stdout().write_all(object.data())?;
    Ok(())
}
