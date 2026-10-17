//! Run `cargo run --example publish_branch`; creates and removes a disposable repository.
//! This deliberately publishes without reflogs. Existing tip recovery needs a separate policy.
use girt::refs::{Expected, RefName, Target};
use girt::{Commit, InitKind, Repository, Signature};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let repo = Repository::init(
        girt::ObjectFormat::Sha1,
        directory.path().join("repo"),
        InitKind::Bare,
    )?;
    let objects = repo.loose_objects();
    let mut editor = girt::tree::TreeEditor::new(&objects);
    editor.set(b"README.md", objects.write_blob(b"# Project\n")?)?;
    let tree = editor.write()?;
    let author = Signature::now(b"A. Writer", b"writer@example.com")?;
    let builder = Commit::builder(tree, author.clone(), b"First snapshot\n");
    let first = objects.write_commit(&builder.build()?)?;
    let refs = repo.references()?;
    let head = RefName::HEAD;
    let branch = refs.update_resolved_without_reflog(&head, first, Expected::Absent)?;
    let second_commit = Commit::builder(tree, author, b"Advance branch\n")
        .parent(first)
        .build()?;
    let second = objects.write_commit(&second_commit)?;
    refs.update_resolved_without_reflog(&head, second, Expected::Value(Target::Direct(first)))?;
    assert_eq!(refs.resolve(&head, 8)?.id, Some(second));
    assert_eq!(refs.read(&head)?, Some(Target::Symbolic(branch.clone())));
    let branches = refs.list_namespace(&RefName::new(b"refs/heads")?)?;
    assert_eq!(branches[0].name, branch);
    // Delete the observed terminal branch, keeping HEAD symbolic and now unborn.
    refs.delete_resolved_without_reflog(&head, Expected::Value(branches[0].target.clone()))?;
    assert_eq!(refs.read(&head)?, Some(Target::Symbolic(branch)));
    assert_eq!(refs.resolve(&head, 8)?.id, None);
    assert!(refs.list()?.is_empty());
    println!("Published {first}, advanced to {second}, then deleted main without reflogs");
    Ok(())
}
