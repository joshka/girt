//! Read exact bytes from an existing loose or packed repository without modifying it.
//!
//! Run `cargo run --example packed_repository -- /path/to/repo <full-sha1-id>`.
//! To prepare a disposable packed repository, create commits and run `git repack -ad` there.
use std::io::{self, Write};

use girt::{ObjectId, PackLimits, ReadLimits, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("expected repository path")?;
    let id: ObjectId = args
        .next()
        .ok_or("expected full SHA-1 object identity")?
        .parse()?;
    let repository = Repository::open(path)?;
    let objects = repository.objects(PackLimits::default())?;
    let object = objects
        .read(id, ReadLimits::default())?
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
