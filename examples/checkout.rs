//! Run `cargo run --example checkout` on macOS/Linux. All files are disposable.
use std::sync::atomic::AtomicBool;

use girt::{EntryMode, InitKind, Repository, Tree, TreeEntry};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let repo = Repository::init(
        girt::ObjectFormat::Sha1,
        temp.path().join("repo"),
        InitKind::Worktree,
    )?;
    let objects = repo.loose_objects();
    let blob = objects.write_blob(b"literal\r\nbytes\0\xff")?;
    let target = objects.write_tree(&Tree::new(
        girt::ObjectFormat::Sha1,
        vec![TreeEntry {
            name: b"hello.txt".to_vec(),
            mode: EntryMode::Blob,
            id: blob,
        }],
    )?)?;
    // This example owns the repository exclusively. After a no-checkout clone, likewise pass
    // None as baseline and read the desired commit's tree for target. HEAD is never switched.
    let cancel = AtomicBool::new(false);
    match repo.checkout_tree(None, Some(target), Default::default(), &cancel) {
        Ok(report) => println!("Published raw tree checkout: {report:?}"),
        Err(failure) => {
            eprintln!(
                "Applied: {:?}; cleanup failures: {:?}",
                failure.report, failure.cleanup
            );
            return Err(failure.into());
        }
    }
    assert_eq!(
        std::fs::read(repo.worktree().unwrap().join("hello.txt"))?,
        b"literal\r\nbytes\0\xff"
    );
    // A subsequent checkout names the current baseline explicitly. This removes only our
    // verified tracked file and publishes an empty index; it does not delete unknown contents.
    repo.checkout_tree(Some(target), None, Default::default(), &cancel)?;
    assert!(!repo.worktree().unwrap().join("hello.txt").exists());
    drop(repo);
    temp.close()?; // Recursive cleanup belongs only to this disposable example's owner.
    Ok(())
}
