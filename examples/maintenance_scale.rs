//! Measure maintenance on an already built, disposable packed repository.
//!
//! Run only while no other process can mutate the repository or retain its old pack files. For
//! example, create a private Git fixture, then run `cargo run --release --example
//! maintenance_scale -- /absolute/path/to/bare.git`. The result reports one run, not a benchmark
//! distribution. The sampler observes pack-directory bytes and open descriptors every 5 ms; short
//! peaks may be missed. Use an external process monitor for peak resident memory.

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant, SystemTime};

#[cfg(unix)]
use girt::Repository;
#[cfg(unix)]
use girt::retention::{MaintenanceIsolation, RepackLimits, RetentionPolicy};

#[cfg(unix)]
struct DisposableIsolation;

#[cfg(unix)]
impl MaintenanceIsolation for DisposableIsolation {
    type Guard = ();

    fn acquire(&mut self, _: &Repository) -> io::Result<Self::Guard> {
        // The operator owns this disposable fixture and has stopped all Git producers/readers.
        Ok(())
    }
}

#[cfg(unix)]
#[derive(Default)]
struct Peaks {
    pack_bytes: u64,
    descriptors: usize,
}

#[cfg(unix)]
fn pack_bytes(directory: &Path) -> io::Result<u64> {
    let mut bytes = 0u64;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let length = match entry.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        bytes = bytes.saturating_add(length);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn sample(directory: PathBuf, running: &AtomicBool) -> io::Result<Peaks> {
    let mut peaks = Peaks::default();
    loop {
        peaks.pack_bytes = peaks.pack_bytes.max(pack_bytes(&directory)?);
        peaks.descriptors = peaks.descriptors.max(fs::read_dir("/dev/fd")?.count());
        if !running.load(Ordering::Relaxed) {
            return Ok(peaks);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("pass the absolute path of a disposable bare Git repository")?;
    let repository = Repository::open(PathBuf::from(path))?;
    let pack_directory = repository.object_dir().join("pack");
    let initial_bytes = pack_bytes(&pack_directory)?;
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let cancel = AtomicBool::new(false);
    let running = AtomicBool::new(true);
    let start = Instant::now();
    let (result, peaks) = thread::scope(|scope| {
        let monitor = scope.spawn(|| sample(pack_directory.clone(), &running));
        let result = repository.run_maintenance(
            &mut DisposableIsolation,
            &policy,
            RepackLimits::default(),
            &cancel,
        );
        running.store(false, Ordering::Relaxed);
        (result, monitor.join().expect("sampler thread panicked"))
    });
    let report = result?;
    let peaks = peaks?;
    let elapsed = start.elapsed();
    let final_bytes = pack_bytes(&pack_directory)?;
    let retired = report
        .retired
        .expect("completed run has a retirement report");
    let written = retired
        .published
        .expect("completed retirement has a replacement pack")
        .written;
    println!("elapsed_ms={}", elapsed.as_millis());
    println!("initial_pack_directory_bytes={initial_bytes}");
    println!("peak_observed_pack_directory_bytes={}", peaks.pack_bytes);
    println!("final_pack_directory_bytes={final_bytes}");
    println!("peak_observed_descriptors={}", peaks.descriptors);
    println!("replacement_objects={}", written.objects);
    println!("replacement_pack_bytes={}", written.pack_bytes);
    println!("replacement_index_bytes={}", written.index_bytes);
    println!("removed_pack_artifacts={}", retired.removed.len());
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("destructive maintenance is unsupported on this platform");
    std::process::exit(1);
}
