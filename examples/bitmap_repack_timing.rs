//! One disposable-repository repack, with bitmap writing explicitly enabled or disabled.
//! Run under /tmp/girt-timing.lock on independent equivalent copies, alternating the setting.
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use girt::Repository;
use girt::retention::{RepackLimits, RetentionPolicy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("expected disposable bare repository")?;
    let enabled: bool = args.next().ok_or("expected bitmap boolean")?.parse()?;
    let repo = Repository::open(path)?;
    let mut limits = RepackLimits::trusted();
    limits.write_bitmaps = Some(enabled);
    let start = Instant::now();
    let report =
        repo.repack_retained(&RetentionPolicy::trusted(), limits, &AtomicBool::new(false))?;
    let elapsed = start.elapsed();
    let directory = repo.object_dir().join("pack");
    let base = directory.join(format!("pack-{}", report.written.checksum));
    let mut sidecar_bytes = 0_u64;
    for extension in ["bitmap", "rev"] {
        if let Ok(metadata) = std::fs::metadata(base.with_extension(extension)) {
            sidecar_bytes += metadata.len();
        }
    }
    println!(
        "{{\"bitmaps\":{enabled},\"elapsed_ns\":{},\"objects\":{},\"pack_bytes\":{},\"sidecar_bytes\":{sidecar_bytes}}}",
        elapsed.as_nanos(),
        report.written.objects,
        report.written.pack_bytes
    );
    Ok(())
}
