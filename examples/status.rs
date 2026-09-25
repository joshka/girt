//! Run without arguments for a disposable fixture, or pass an existing repository path.
use std::sync::atomic::AtomicBool;

use girt::status::{Baseline, Limits, Untracked};
use girt::{InitKind, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let repository = match std::env::args_os().nth(1) {
        Some(path) => Repository::open(path)?,
        None => fixture(&temp)?,
    };
    let report = repository.raw_status(
        Baseline::Head,
        Untracked::RawFilesWithoutIgnores,
        Limits::default(),
        &AtomicBool::new(false),
    )?;
    println!(
        "Literal bytes and POSIX modes; ignores/attributes/config normalization are not applied."
    );
    println!("{report:#?}");
    Ok(())
}

fn fixture(temp: &tempfile::TempDir) -> Result<Repository, Box<dyn std::error::Error>> {
    let repository = Repository::init(temp.path().join("repo"), InitKind::Worktree)?;
    let id = repository.loose_objects()?.write_blob(b"staged\n")?;
    let mut edit = repository.edit_index(Default::default())?;
    edit.replace_entries(vec![girt::index::Entry::new(
        b"hello.txt".to_vec(),
        girt::index::Mode::Regular,
        id,
    )])?;
    edit.commit()?;
    std::fs::write(
        repository.worktree().unwrap().join("hello.txt"),
        b"working\n",
    )?;
    Ok(repository)
}
