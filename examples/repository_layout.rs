//! Inspect repository paths, registrations and shallow roots without changing files.
//! Run `cargo run --example repository_layout -- /path/to/repository`.
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use girt::Repository;

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Repository to inspect.
    path: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let repository = Repository::open(args.path)?;
    println!("Git directory: {}", repository.git_dir().display());
    println!("Common directory: {}", repository.common_dir().display());
    println!("Bare: {}", repository.is_bare());
    println!("Checkout: {:?}", repository.worktree());
    for worktree in repository.worktrees(10_000, &AtomicBool::new(false))? {
        println!(
            "{}: {:?} ({:?})",
            worktree.git_dir.display(),
            worktree.path,
            worktree.state
        );
    }
    for root in repository.shallow_roots().iter() {
        println!("Shallow root: {root}");
    }
    Ok(())
}
