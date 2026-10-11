//! Repack a disposable, isolated repository while measuring process time and RSS externally.
//!
//! Pass the repository path to publish and retire old packs, or append `additive` to measure
//! publication without retirement. Use `/usr/bin/time -l` on macOS for wall time and peak RSS.
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use girt::Repository;
use girt::retention::{MaintenanceIsolation, RepackLimits, RetentionPolicy};

struct IsolatedFixture;
impl MaintenanceIsolation for IsolatedFixture {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        // The operator must stop all writers, readers and alternate dependents of this fixture.
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("pass an isolated repository path")?;
    let repository = Repository::open(std::path::PathBuf::from(path))?;
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..RetentionPolicy::trusted()
    };
    let (written, removed) = if std::env::args().nth(2).as_deref() == Some("additive") {
        let report = repository.repack_retained(
            &policy,
            RepackLimits::trusted(),
            &AtomicBool::new(false),
        )?;
        (report.written, 0)
    } else {
        let report = repository.retire_old_packs(
            &mut IsolatedFixture,
            &policy,
            RepackLimits::trusted(),
            &AtomicBool::new(false),
        )?;
        (
            report.published.ok_or("replacement not published")?.written,
            report.removed.len(),
        )
    };
    println!(
        "objects={} pack_bytes={} index_bytes={} deltas={} removed={removed}",
        written.objects, written.pack_bytes, written.index_bytes, written.deltas.entries
    );
    Ok(())
}
