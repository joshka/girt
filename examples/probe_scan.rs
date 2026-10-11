//! Times one retention scan on a repository given as the first argument.
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

use girt::retention::RetentionPolicy;
use girt::Repository;

fn main() {
    let path = std::env::args().nth(1).expect("repository path");
    let repo = Repository::open(&path).expect("open");
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..RetentionPolicy::trusted()
    };
    let start = Instant::now();
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    let elapsed = start.elapsed();
    println!(
        "scan_seconds={:.3} complete={} required={}",
        elapsed.as_secs_f64(),
        plan.is_complete(),
        plan.required.len()
    );
}
