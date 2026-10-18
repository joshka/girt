//! Emit inferred relationships for two stored trees using the default rename policy.
//! Run `cargo run --example rewrites -- REPOSITORY OLD_TREE NEW_TREE`.
//! Each NUL-delimited record contains an `R` or `C` marker, a three-digit similarity score, and
//! source and target byte paths.
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use girt::rewrites::{Limits, Options};
use girt::{ObjectId, Repository};

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Repository to open.
    repository: PathBuf,
    /// ID of the old tree.
    old_tree: ObjectId,
    /// ID of the new tree.
    new_tree: ObjectId,
}

fn main() -> girt::Result<()> {
    let args = Args::parse();
    let repo = Repository::open(args.repository)?;
    let objects = repo.objects()?;

    let records = objects.detect_rewrites(
        Some(args.old_tree),
        Some(args.new_tree),
        Options::default(),
        Limits::default(),
        None,
        &AtomicBool::new(false),
    )?;

    let mut output = io::stdout().lock();
    for record in records {
        write!(output, "{}{:03}\0", record.kind.letter(), record.similarity)?;
        output.write_all(&record.source)?;
        output.write_all(&[0])?;
        output.write_all(&record.target)?;
        output.write_all(&[0])?;
    }
    Ok(())
}
