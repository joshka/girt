//! Build a tree in memory without writing any objects or traversing a filesystem.
use girt::{EntryMode, ObjectId, Tree, TreeEntry};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tree = Tree::new(
        girt::ObjectFormat::Sha1,
        vec![TreeEntry {
            mode: EntryMode::Blob,
            name: b"hello.txt".to_vec(),
            id: ObjectId::for_blob(girt::ObjectFormat::Sha1, b"hello from girt\n"),
        }],
    )?;

    let payload = tree.encode();
    let parsed = Tree::parse(girt::ObjectFormat::Sha1, &payload)?;

    // Parsing preserves the original records, including readable but invalid trees. Validate when
    // accepting an external payload that must have valid names, unique entries, and Git ordering.
    // Here it is redundant because the payload came unchanged from Tree::new.
    parsed.validate()?;

    assert_eq!(parsed.entries(), tree.entries());

    println!("tree {} ({} payload bytes)", tree.id(), payload.len());

    Ok(())
}
