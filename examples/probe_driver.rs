use std::{io, sync::atomic::AtomicBool, time::{Duration, SystemTime}};
use girt::{Repository, retention::{MaintenanceIsolation, RepackLimits, RetentionPolicy}};
struct Disposable;
impl MaintenanceIsolation for Disposable {
 type Guard = ();
 fn acquire(&mut self, _: &Repository) -> io::Result<()> { Ok(()) }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
 let args: Vec<String> = std::env::args().collect();
 let repo = Repository::open(&args[2])?;
 let policy = RetentionPolicy { recent_cutoff: SystemTime::now() + Duration::from_secs(60), ..RetentionPolicy::trusted() };
 let cancel = AtomicBool::new(false);
 if args[1] == "repack" {
  let result = repo.repack_retained(&policy, RepackLimits::trusted(), &cancel)?;
  println!("{:?}", result.written);
 } else {
  let result = repo.run_maintenance(&mut Disposable, &policy, RepackLimits::trusted(), &cancel)?;
  println!("{:?}", result);
 }
 Ok(())
}
