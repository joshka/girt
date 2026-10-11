//! Repack a disposable repository, optionally retiring old packs under exclusive ownership.
//!
//! Build first, then measure with an external process monitor:
//! `cargo build --release --example repack` followed by
//! `target/release/examples/repack /path/to/disposable.git [retire]`.
//! The `retire` mode assumes no writers, old readers, or alternate consumers use the repository.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use girt::Repository;
use girt::retention::{MaintenanceIsolation, RepackLimits, RetentionPolicy};
struct Isolated;
impl MaintenanceIsolation for Isolated {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        Ok(())
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let path = args
        .get(1)
        .ok_or("pass a disposable repository path and optional retire mode")?;
    let repo = Repository::open(path)?;
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..RetentionPolicy::trusted()
    };
    let cancel = AtomicBool::new(false);
    let w = if args.get(2).is_some_and(|s| s == "retire") {
        repo.retire_old_packs(&mut Isolated, &policy, RepackLimits::trusted(), &cancel)?
            .published
            .unwrap()
            .written
    } else {
        repo.repack_retained(&policy, RepackLimits::trusted(), &cancel)?
            .written
    };
    println!(
        "objects={} pack_bytes={} index_bytes={}",
        w.objects, w.pack_bytes, w.index_bytes
    );
    Ok(())
}
