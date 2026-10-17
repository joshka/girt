//! Isolated reader measurement; generate inputs with tests/fixtures/file-storage/generate.py.
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use girt::{AlternateLimits, ObjectId, PackLimits, ReadLimits, Repository};

fn handles() -> Option<usize> {
    #[cfg(unix)]
    {
        let directory = if cfg!(target_os = "linux") {
            "/proc/self/fd"
        } else {
            "/dev/fd"
        };
        std::fs::read_dir(directory)
            .ok()
            .map(|entries| entries.count())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Mode {
    /// Measure cancellation of an in-flight store open instead of reads.
    Cancel,
    /// Keep the process alive for one second after measuring so it can be sampled.
    Hold,
}

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Repository to open.
    repository: PathBuf,
    /// Object to read.
    id: ObjectId,
    /// Optional measurement mode.
    #[arg(value_enum)]
    mode: Option<Mode>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let repository = Repository::open(args.repository)?;
    let id = args.id;
    let baseline = handles();
    let failed = repository.objects_with(PackLimits::default().with_max_index_bytes(0));
    assert!(matches!(
        failed,
        Err(girt::ObjectReadError::Limit("pack index bytes"))
    ));
    assert_eq!(handles(), baseline);
    if args.mode == Some(Mode::Cancel) {
        let cancelled = AtomicBool::new(false);
        let start = Instant::now();
        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(10));
                cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
            });
            repository.objects_controlled(
                PackLimits::default(),
                AlternateLimits::default(),
                &cancelled,
            )
        });
        assert!(matches!(result, Err(girt::ObjectReadError::Cancelled)));
        assert_eq!(handles(), baseline);
        println!(
            "cancel_us={} handles_before={baseline:?} handles_after={:?}",
            start.elapsed().as_micros(),
            handles()
        );
        return Ok(());
    }
    let cancelled = AtomicBool::new(false);
    let start = Instant::now();
    let mut objects = repository.objects_controlled(
        PackLimits::default(),
        AlternateLimits::default(),
        &cancelled,
    )?;
    let opened = start.elapsed();
    let retained = handles();
    let clone = objects.clone();
    let cloned = handles();
    let failed = objects.refresh(
        PackLimits::default().with_max_index_bytes(0),
        AlternateLimits::default(),
    );
    assert!(matches!(
        failed,
        Err(girt::ObjectReadError::Limit("pack index bytes"))
    ));
    assert_eq!(handles(), cloned);
    let start = Instant::now();
    objects.refresh(PackLimits::default(), AlternateLimits::default())?;
    let refreshed = start.elapsed();
    let generations = handles();
    assert_eq!(
        generations,
        baseline
            .zip(retained)
            .map(|(base, open)| base + 2 * (open - base))
    );
    println!(
        "refresh_us={} handles_two_generations={generations:?}",
        refreshed.as_micros()
    );
    let start = Instant::now();
    let object = clone
        .read_controlled(id, ReadLimits::default(), &cancelled)?
        .ok_or("object missing")?;
    let read = start.elapsed();
    let start = Instant::now();
    let second = clone
        .read_with(id, ReadLimits::default())?
        .ok_or("object missing")?;
    let reread = start.elapsed();
    assert_eq!(object, second);
    println!(
        "pid={} open_us={} first_read_us={} warm_read_us={} payload_bytes={} handles_before={baseline:?} handles_open={retained:?} handles_clone={cloned:?}",
        std::process::id(),
        opened.as_micros(),
        read.as_micros(),
        reread.as_micros(),
        object.data().len()
    );
    drop(second);
    drop(object);
    drop(objects);
    drop(clone);
    println!("handles_after_drop={:?}", handles());
    assert_eq!(handles(), baseline);
    // Optional sampling interval is outside measured operations and retains no reader.
    if args.mode == Some(Mode::Hold) {
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}
