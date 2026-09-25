//! Initialize a disposable repository and discover it from a nested directory.
//! Run `cargo run --example init_repository`. SHA-256 loose storage works without refs or indexes.
use girt::{InitKind, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let repository = Repository::init(
        girt::ObjectFormat::Sha256,
        root.path().join("project"),
        InitKind::Worktree,
    )?;
    let nested = repository.worktree().unwrap().join("src");
    std::fs::create_dir(&nested)?;
    let discovered = Repository::discover_with_ceiling(&nested, root.path())?;
    assert_eq!(discovered.git_dir(), repository.git_dir());
    let id = discovered.loose_objects().write_blob(b"hello\n")?;
    assert_eq!(discovered.loose_objects().read_blob(id, 1024)?, b"hello\n");
    println!("Initialized and discovered an empty main branch; wrote blob {id}");
    Ok(())
}
