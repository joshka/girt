//! Disposable public-API driver for the retained transfer measurements.
use std::io::Write;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use girt::fetch::FetchLimits;
use girt::push::{ForcePolicy, PushCommand};
use girt::refs::{RefName, Reflog, Target};
use girt::transfer::{Environment, FetchOptions, NoCallbacks};
use girt::{PackLimits, Repository};

fn phase(label: &str, start: Instant) -> Instant {
    eprintln!("phase {label}={:.6}", start.elapsed().as_secs_f64());
    Instant::now()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let op = args[1].as_str();
    let path = &args[2];
    let cancel = AtomicBool::new(false);
    let t = Instant::now();
    match op {
        "fetch" => {
            let repo = Repository::open(path)?;
            let env = Environment::from_process();
            let options = FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]);
            let out = repo.fetch("origin", &options, &env, &mut NoCallbacks)?;
            println!(
                "updated={} missing={}",
                out.updated.len(),
                out.missing_sources.len()
            );
        }
        "prepare-transfer" => {
            let repo = Repository::open(path)?;
            let objects = repo.objects(PackLimits::trusted())?;
            let commands = repo
                .references()?
                .list()?
                .into_iter()
                .enumerate()
                .filter_map(|(n, r)| {
                    if let Target::Direct(id) = r.target {
                        Some(PushCommand {
                            name: RefName::new(format!("refs/for/{n}")).unwrap(),
                            expected: None,
                            new: id,
                            force: ForcePolicy::Allow,
                        })
                    } else {
                        None
                    }
                })
                .collect();
            let mut phase_start = Instant::now();
            let prepared = girt::push::PreparedPush::new_local_with_progress(
                &objects,
                commands,
                &[],
                girt::push::PushLimits::trusted(),
                &cancel,
                |event| {
                    if let girt::push::PreparationProgress::Packing { objects: (0, _) } = event {
                        phase_start = phase("select-locate", phase_start);
                    }
                    if event == girt::push::PreparationProgress::Complete {
                        phase("write", phase_start);
                    }
                },
            )?;
            println!(
                "objects={} bytes={}",
                prepared.object_count(),
                prepared.pack_bytes()
            );
        }
        "clone" => {
            use std::ops::ControlFlow;
            let source = std::fs::canonicalize(path)?;
            let dest = std::path::PathBuf::from(&args[3]);
            let request = girt::clone::CloneRequest::prepare_tracking(
                &dest,
                girt::InitKind::Bare,
                source.to_str().unwrap().as_bytes(),
                girt::clone::BranchSelection::Default,
                Reflog::Preserve,
            )?;
            let ready = request.receive_local(
                &source,
                FetchLimits::trusted(),
                girt::transport::TransportControl::new(&cancel),
                |_| ControlFlow::Continue(()),
            )?;
            let t = phase("receive", t);
            let report = ready
                .finish(girt::fetch::FetchUpdateLimits::trusted(), &cancel)
                .map_err(|e| e.source)?;
            phase("finish", t);
            println!("{}", report.repository.is_some());
        }
        _ => return Err(format!("unknown op {op}").into()),
    }
    std::io::stdout().flush()?;
    Ok(())
}
