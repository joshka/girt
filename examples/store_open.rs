//! Measures warm-cache store opening, repeated opening, and unchanged refresh on an existing repo.
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use girt::refs::RefName;
use girt::{AlternateLimits, PackLimits, ReadLimits, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let operation = args
        .next()
        .ok_or("expected open, repeat, refresh or verify")?;
    let path = args.next().ok_or("expected repository path")?;
    let start = Instant::now();
    let repository = Repository::open(path)?;
    let head = repository
        .references()?
        .resolve(&RefName::new("HEAD")?, 10)?
        .id
        .ok_or("unborn HEAD")?;
    let count = if operation == "repeat" { 8 } else { 1 };
    for _ in 0..count {
        let mut objects = repository.objects(PackLimits::trusted())?;
        let measured = Instant::now();
        match operation.as_str() {
            "refresh" => objects.refresh(PackLimits::trusted(), AlternateLimits::default())?,
            "open" | "repeat" => {}
            "verify" => objects.verify_packs(&AtomicBool::new(false))?,
            _ => return Err("expected open, repeat, refresh or verify".into()),
        }
        let object = objects
            .read(head, ReadLimits::trusted())?
            .ok_or("missing HEAD")?;
        std::hint::black_box(object);
        if operation == "refresh" {
            println!("refresh_seconds={:.9}", measured.elapsed().as_secs_f64());
        }
    }
    println!("total_seconds={:.9}", start.elapsed().as_secs_f64());
    Ok(())
}
