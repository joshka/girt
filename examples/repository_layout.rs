//! Inspect repository paths, registrations and shallow roots without changing files.
//! Run `cargo run --example repository_layout -- /path/to/repository`.
use std::sync::atomic::AtomicBool;

use girt::Repository;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: repository_layout PATH")?;
    let repository = Repository::open(path)?;
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
