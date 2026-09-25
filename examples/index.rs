//! Construct and replace a disposable repository's index without creating working files.
use girt::index::{Entry, Limits, Mode};
use girt::{InitKind, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let repository = Repository::init(
        girt::ObjectFormat::Sha256,
        temporary.path().join("repo"),
        InitKind::Worktree,
    )?;
    let limits = Limits::default();
    assert!(repository.read_index(limits)?.is_none());

    // Store the object first; index publication does not verify object existence or apply filters.
    let id = repository
        .loose_objects()
        .write_blob(b"hello from the index\n")?;
    let mut edit = repository.edit_index(limits)?;
    let mut entries = edit.index().entries().to_vec();
    entries.push(Entry::new(b"hello.txt".to_vec(), Mode::Regular, id));
    edit.replace_entries(entries)?;
    edit.commit()?;

    let index = repository.read_index(limits)?.expect("published index");
    assert_eq!(index.entries()[0].id, id);
    assert!(!repository.worktree().unwrap().join("hello.txt").exists());
    println!(
        "Published {} index entry referencing {id}",
        index.entries().len()
    );
    // Temporary-directory destruction removes the index, object and repository metadata.
    Ok(())
}
