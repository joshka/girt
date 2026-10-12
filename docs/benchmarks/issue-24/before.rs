use std::sync::atomic::AtomicBool;
use girt::Repository;
use girt::retention::{MaintenanceIsolation, RetentionPolicy, RepackLimits};
struct Disposable;
impl MaintenanceIsolation for Disposable {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> { Ok(()) }
}
fn main() {
    let mode = std::env::args().nth(1).unwrap();
    let path = std::env::args().nth(2).unwrap();
    let repository = Repository::open(path).unwrap();
    let mut policy = RetentionPolicy::trusted();
    if mode == "repack-old" {
        policy.recent_cutoff = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    }
    let cancel = AtomicBool::new(false);
    match mode.as_str() {
        "gc" => { repository.expire_reflogs(&mut Disposable, &policy, &cancel).unwrap(); repository.prune_unreachable_loose(&mut Disposable, &policy, &cancel).unwrap(); }
        "legacy" => {
            repository.expire_reflogs(&mut Disposable, &policy, &cancel).unwrap();
            repository.prune_unreachable_loose(&mut Disposable, &policy, &cancel).unwrap();
        }
        "repack" | "repack-old" => { repository.repack_retained(&policy, RepackLimits::trusted(), &cancel).unwrap(); }
        "maintenance" => { repository.run_maintenance(&mut Disposable, &policy, RepackLimits::trusted(), &cancel).unwrap(); }
        "plan" => { let plan = repository.plan_retention(&policy, &cancel); assert!(plan.is_complete(), "{:?}", plan.outcome); println!("{} objects", plan.required.len()); }
        _ => panic!("unknown mode"),
    }
}
