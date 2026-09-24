//! Run `cargo run --example publish_branch`; creates and removes a disposable repository.
//! This deliberately publishes without reflogs. Existing tip recovery needs a separate policy.
use girt::refs::{Expected, RefName, Target};
use girt::{Commit, CommitFields, Repository, Signature, Tree};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    // Minimal bare files layout for the example; girt does not yet expose repository
    // initialization.
    std::fs::create_dir(directory.path().join("objects"))?;
    std::fs::create_dir(directory.path().join("refs"))?;
    std::fs::write(directory.path().join("HEAD"), b"ref: refs/heads/main\n")?;
    std::fs::write(directory.path().join("config"), b"[core]\nbare=true\n")?;
    let repo = Repository::open(directory.path())?;
    let objects = repo.loose_objects()?;
    let tree = objects.write_tree(&Tree::new(vec![])?)?;
    let author = Signature {
        name: b"A. Writer".to_vec(),
        email: b"writer@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let mut fields = CommitFields {
        tree,
        parents: vec![],
        author: author.clone(),
        committer: author,
        extra_headers: vec![],
        message: b"First snapshot\n".to_vec(),
    };
    let first = objects.write_commit(&Commit::new(fields.clone())?)?;
    let refs = repo.references()?;
    let head = RefName::new(b"HEAD")?;
    let branch = refs.update_resolved_without_reflog(&head, first, Expected::Absent)?;
    fields.parents.push(first);
    fields.message = b"Advance branch\n".to_vec();
    let second = objects.write_commit(&Commit::new(fields)?)?;
    refs.update_resolved_without_reflog(&head, second, Expected::Value(Target::Direct(first)))?;
    assert_eq!(refs.resolve(&head, 8)?.id, Some(second));
    assert_eq!(refs.read(&head)?, Some(Target::Symbolic(branch)));
    println!("Published {first}, then advanced main to {second} without reflogs");
    Ok(())
}
