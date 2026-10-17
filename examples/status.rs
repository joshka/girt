//! Run without arguments for a disposable fixture, or pass an existing repository path.
use std::path::PathBuf;

use clap::Parser;
use girt::status::{Baseline, Untracked};
use girt::{InitKind, Repository};

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Existing repository to inspect. Omit to use a disposable fixture.
    path: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let temp = tempfile::tempdir()?;
    let repository = match args.path {
        Some(path) => Repository::open(path)?,
        None => fixture(&temp)?,
    };
    let report = repository.raw_status(Baseline::Head, Untracked::RawFilesWithoutIgnores)?;
    println!(
        "Literal bytes and POSIX modes; ignores/attributes/config normalization are not applied."
    );
    println!("{report:#?}");
    Ok(())
}

fn fixture(temp: &tempfile::TempDir) -> Result<Repository, Box<dyn std::error::Error>> {
    let repository = Repository::init(
        girt::ObjectFormat::Sha1,
        temp.path().join("repo"),
        InitKind::Worktree,
    )?;
    let id = repository.loose_objects().write_blob(b"staged\n")?;
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
