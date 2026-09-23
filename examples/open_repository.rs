//! Read a loose blob by explicit repository path and full SHA-1 ID.
//! Run `cargo run --example open_repository -- /path/to/repo <blob-id>`.
use girt::{ObjectId, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().ok_or("usage: open_repository PATH BLOB_ID")?;
    let id: ObjectId = args
        .next()
        .ok_or("missing blob ID")?
        .to_str()
        .ok_or("ID must be ASCII")?
        .parse()?;
    let repository = Repository::open(path)?;
    let bytes = repository
        .loose_objects()?
        .read_blob(id, 16 * 1024 * 1024)?;
    println!(
        "Read {} bytes from {}",
        bytes.len(),
        repository.object_dir().display()
    );
    Ok(())
}
