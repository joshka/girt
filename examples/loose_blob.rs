//! Run with `cargo run --example loose_blob`; all storage is temporary.
use girt::{LooseObjects, ObjectFormat};

fn main() -> Result<(), girt::Error> {
    let directory = tempfile::tempdir()?;
    let objects = LooseObjects::new(directory.path(), ObjectFormat::Sha1);
    let bytes = b"hello\0Git\xff";

    let id = objects.write_blob(bytes)?;
    let restored = objects.read_blob(id, bytes.len())?;
    assert_eq!(restored, bytes);
    println!("Stored and read {} bytes as {id}", restored.len());
    Ok(())
}
