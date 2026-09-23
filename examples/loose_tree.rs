//! Run with `cargo run --example loose_tree`; all storage is temporary.
use girt::{EntryMode, LooseObjects, ObjectFormat, Tree, TreeEntry};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let objects = LooseObjects::new(directory.path(), ObjectFormat::Sha1)?;
    let text = objects.write_blob(b"hello\n")?;
    let binary = objects.write_blob(b"\0\xff")?;
    let tree = Tree::new(vec![
        TreeEntry {
            mode: EntryMode::Blob,
            name: b"hello.txt".to_vec(),
            id: text,
        },
        TreeEntry {
            mode: EntryMode::Blob,
            name: b"data.bin".to_vec(),
            id: binary,
        },
    ])?;
    let id = objects.write_tree(&tree)?;
    let restored = objects.read_tree(id, 1024)?;
    assert_eq!(id, tree.id());
    assert_eq!(restored.encode(), tree.encode());
    assert_eq!(
        objects.read_blob(restored.entries()[0].id, 1024)?,
        b"\0\xff"
    );
    assert_eq!(
        objects.read_blob(restored.entries()[1].id, 1024)?,
        b"hello\n"
    );
    println!(
        "Stored and read tree {id} with {} entries",
        restored.entries().len()
    );
    Ok(())
}
