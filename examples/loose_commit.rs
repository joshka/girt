//! Run with `cargo run --example loose_commit`; all storage is disposable.
use girt::{
    Commit, CommitFields, EntryMode, LooseObjects, ObjectFormat, Signature, Tree, TreeEntry,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let objects = LooseObjects::new(directory.path().join("objects"), ObjectFormat::Sha1);
    let blob = objects.write_blob(b"Hello from a commit!\n")?;
    let tree = Tree::new(
        girt::ObjectFormat::Sha1,
        vec![TreeEntry {
            mode: EntryMode::Blob,
            name: b"hello.txt".to_vec(),
            id: blob,
        }],
    )?;
    let tree_id = objects.write_tree(&tree)?;
    let author = Signature {
        name: b"A. Writer".to_vec(),
        email: b"writer@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: -420,
    };
    let commit = Commit::new(CommitFields {
        tree: tree_id,
        parents: vec![],
        author: author.clone(),
        committer: author,
        extra_headers: vec![],
        message: b"Record the first snapshot\n".to_vec(),
    })?;
    let id = objects.write_commit(&commit)?;
    let restored = objects.read_commit(id, 4096)?;
    assert_eq!(restored, commit);
    let restored_tree = objects.read_tree(restored.fields().tree, 4096)?;
    assert_eq!(
        objects.read_blob(restored_tree.entries()[0].id, 1024)?,
        b"Hello from a commit!\n"
    );
    println!("Stored commit {id} referencing tree {tree_id} and blob {blob}");
    // The temporary directory and every loose object are removed on normal exit.
    Ok(())
}
