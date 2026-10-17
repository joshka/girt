//! Measures warm-cache store opening, repeated opening, and unchanged refresh on an existing repo.
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use clap::{Parser, ValueEnum};
use girt::refs::RefName;
use girt::{AlternateLimits, PackLimits, ReadLimits, Repository};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Operation {
    /// Open the object store once.
    Open,
    /// Open the object store eight times.
    Repeat,
    /// Refresh an unchanged object store.
    Refresh,
    /// Verify all packs after opening.
    Verify,
}

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Operation to measure.
    #[arg(value_enum)]
    operation: Operation,
    /// Existing repository to open.
    path: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let operation = args.operation;
    let start = Instant::now();
    let repository = Repository::open(args.path)?;
    let head = repository
        .references()?
        .resolve(&RefName::HEAD, 10)?
        .id
        .ok_or("unborn HEAD")?;
    let count = if operation == Operation::Repeat { 8 } else { 1 };
    for _ in 0..count {
        let mut objects = repository.objects_with(PackLimits::trusted())?;
        let measured = Instant::now();
        match operation {
            Operation::Refresh => {
                objects.refresh(PackLimits::trusted(), AlternateLimits::default())?
            }
            Operation::Open | Operation::Repeat => {}
            Operation::Verify => objects.verify_packs(&AtomicBool::new(false))?,
        }
        let object = objects
            .read_with(head, ReadLimits::trusted())?
            .ok_or("missing HEAD")?;
        std::hint::black_box(object);
        if operation == Operation::Refresh {
            println!("refresh_seconds={:.9}", measured.elapsed().as_secs_f64());
        }
    }
    println!("total_seconds={:.9}", start.elapsed().as_secs_f64());
    Ok(())
}
