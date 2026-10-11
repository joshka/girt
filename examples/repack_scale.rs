//! Measures additive repacking of a disposable large repository.
//!
//! Run `cargo run --release --example repack_scale -- /path/to/repository` under an external
//! time/RSS monitor. The repository must be trusted and idle. This publishes a pack without
//! deleting old storage; `maintenance_scale` measures durable retirement and the full GC sequence.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

use girt::Repository;
use girt::retention::{RepackLimits, RetentionPolicy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("pass a disposable repository path")?;
    let repository = Repository::open(PathBuf::from(path))?;
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..RetentionPolicy::trusted()
    };
    let start = Instant::now();
    let published =
        repository.repack_retained(&policy, RepackLimits::trusted(), &AtomicBool::new(false))?;
    println!("elapsed_ms={}", start.elapsed().as_millis());
    println!("pack_bytes={}", published.written.pack_bytes);
    println!("index_bytes={}", published.written.index_bytes);
    println!("objects={}", published.written.objects);
    println!("delta_entries={}", published.written.deltas.entries);
    println!("delta_candidates={}", published.written.deltas.candidates);
    Ok(())
}
