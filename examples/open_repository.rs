//! Read a blob through a trusted handle with explicit repository configuration.
//! Run `cargo run --example open_repository -- /path/to/repo <blob-revision>`.
use std::path::PathBuf;

use clap::Parser;
use girt::Repository;
use girt::trusted::Handle;

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Repository to open without inherited configuration.
    path: PathBuf,
    /// Revision naming a blob (a full blob ID or HEAD:README.md).
    revision: String,
}

fn main() -> girt::Result<()> {
    let args = Args::parse();
    let git = Handle::new(Repository::open(args.path)?);
    let id = git.rev_parse(&args.revision)?;
    let bytes = git.repository().objects()?.read_blob(id)?;
    println!(
        "Read {} bytes from {}",
        bytes.len(),
        git.repository().object_dir().display()
    );
    Ok(())
}
