//! Print inferred relationships for two trees using stored bytes and default rename policy.
use std::error::Error;
use std::io::{self, Write};
use std::sync::atomic::AtomicBool;

use girt::rewrites::{Kind, Limits, Options};
use girt::{ObjectId, PackLimits, Repository};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 4 {
        return Err("usage: rewrites REPOSITORY OLD_TREE NEW_TREE".into());
    }
    let old: ObjectId = args[2].to_str().ok_or("non-text old ID")?.parse()?;
    let new: ObjectId = args[3].to_str().ok_or("non-text new ID")?.parse()?;
    let repo = Repository::open(&args[1])?;
    let objects = repo.objects(PackLimits::default())?;
    let records = objects.detect_rewrites(
        Some(old),
        Some(new),
        Options::default(),
        Limits::default(),
        None,
        &AtomicBool::new(false),
    )?;
    let mut output = io::stdout().lock();
    for record in records {
        let marker = match record.kind {
            Kind::Rename => 'R',
            Kind::Copy => 'C',
        };
        write!(output, "{marker}{:03}\0", record.similarity)?;
        output.write_all(&record.source)?;
        output.write_all(&[0])?;
        output.write_all(&record.target)?;
        output.write_all(&[0])?;
    }
    Ok(())
}
