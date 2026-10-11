use std::{sync::atomic::AtomicBool, time::{Duration, Instant, SystemTime}};
use girt::{Repository, retention::{RepackLimits, RetentionPolicy}};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let repo = Repository::open(&args[1])?;
    let policy = RetentionPolicy { recent_cutoff: SystemTime::now() + Duration::from_secs(60), ..RetentionPolicy::trusted() };
    let cancel = AtomicBool::new(false);
    let start = Instant::now();
    let plan = repo.plan_retention(&policy, &cancel);
    println!("scan_seconds={:.6} required={} complete={}", start.elapsed().as_secs_f64(), plan.required.len(), plan.is_complete());
    let start = Instant::now();
    let written = repo.repack_retained(&policy, RepackLimits::trusted(), &cancel)?;
    println!("repack_seconds={:.6} {:?}", start.elapsed().as_secs_f64(), written);
    Ok(())
}
