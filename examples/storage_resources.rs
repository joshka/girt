//! Isolated reader measurement; generate inputs with tests/fixtures/file-storage/generate.py.
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let repository = Repository::open(args.get(1).ok_or("repository path required")?)?;
    let id: ObjectId = args.get(2).ok_or("object id required")?.parse()?;
    let baseline = handles();
    let failed = repository.objects(PackLimits {
        max_index_bytes: 0,
        ..PackLimits::default()
    });
    assert!(matches!(
        failed,
        Err(girt::ObjectReadError::Limit("pack index bytes"))
    ));
    assert_eq!(handles(), baseline);
    if args.get(3).is_some_and(|arg| arg == "cancel") {
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
    let objects = repository.objects_controlled(
        PackLimits::default(),
        AlternateLimits::default(),
        &cancelled,
    )?;
    let opened = start.elapsed();
    let retained = handles();
    let clone = objects.clone();
    let cloned = handles();
    let start = Instant::now();
    let object = clone
        .read_controlled(id, ReadLimits::default(), &cancelled)?
        .ok_or("object missing")?;
    let read = start.elapsed();
    let start = Instant::now();
    let second = clone
        .read(id, ReadLimits::default())?
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
    // Optional sampling interval is outside measured operations and retains no reader.
    if args.get(3).is_some_and(|arg| arg == "hold") {
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}
