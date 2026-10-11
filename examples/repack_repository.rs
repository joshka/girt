//! Repack a disposable benchmark repository, preserving existing compressed entries.
use std::sync::atomic::AtomicBool;

use girt::Repository;
use girt::retention::{RepackLimits, RetentionPolicy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: repack_repository <repository>")?;
    let repository = Repository::open(path)?;
    let result = repository.repack_retained(
        &RetentionPolicy::trusted(),
        RepackLimits::trusted(),
        &AtomicBool::new(false),
    )?;
    println!(
        "objects={} pack_bytes={} index_bytes={} deltas={}",
        result.written.objects,
        result.written.pack_bytes,
        result.written.index_bytes,
        result.written.deltas.entries
    );
    Ok(())
}
