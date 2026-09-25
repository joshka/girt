#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "../tests/support/pack_git.rs"]
mod pack_git;
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "../tests/support/ssh_git.rs"]
mod ssh_git;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod supported {
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use criterion::Criterion;
    use girt::fetch::{FetchLimits, KnownHistory, receive_ssh};
    use girt::transport::TransportControl;
    use girt::{PackLimits, ReadLimits};

    use super::{pack_git, ssh_git};

    pub fn ssh(c: &mut Criterion) {
        let f = pack_git::Fixture::new(girt::ObjectFormat::Sha1, true, 16);
        let objects = f.repo.objects(PackLimits::default()).unwrap();
        assert!(
            objects
                .read(f.ordinary, ReadLimits::default())
                .unwrap()
                .is_some()
        );
        assert!(
            objects
                .read(f.delta, ReadLimits::default())
                .unwrap()
                .is_some()
        );
        assert!(f.index_path.exists());
        let tag = f.records.last().unwrap().0;
        let cancel = AtomicBool::new(false);
        let limits = FetchLimits::default();
        let known = KnownHistory::new(&objects, &[tag], limits, &cancel).unwrap();
        let known = Some(std::sync::Arc::new(known));
        let old = f.records[f.records.len() - 2].0;
        let tree = pack_git::git(f.root.path(), &["rev-parse", "main^{tree}"], b"");
        let new = pack_git::git(
            f.root.path(),
            &[
                "commit-tree",
                std::str::from_utf8(&tree).unwrap().trim(),
                "-p",
                &old.to_string(),
            ],
            b"SSH benchmark next\n",
        );
        let new: girt::ObjectId = std::str::from_utf8(&new).unwrap().trim().parse().unwrap();
        pack_git::git(
            f.root.path(),
            &["update-ref", "refs/heads/main", &new.to_string()],
            b"",
        );
        let server = ssh_git::Server::new(f.root.path(), "none");
        let remote = server.remote("config");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut group = c.benchmark_group("ssh-loopback");
        group
            .sample_size(10)
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(2));
        for (name, history, want) in [
            ("full-download-validate", None, new),
            ("incremental-download-validate", known.clone(), new),
            ("known-only-discovery-validate", known.clone(), tag),
        ] {
            group.bench_function(name, |b| {
                b.iter(|| {
                    let downloaded = rt
                        .block_on(receive_ssh(
                            &remote,
                            |_| vec![want],
                            history.clone(),
                            limits,
                            TransportControl::new(&cancel),
                        ))
                        .unwrap();
                    downloaded
                        .validate(&cancel, |_| ControlFlow::Continue(()))
                        .unwrap()
                })
            });
        }
        group.finish();
        eprintln!(
            "ssh-workload,blobs=16,payload_bytes={},objects={}",
            f.records.iter().map(|r| r.2.len()).sum::<usize>(),
            f.records.len()
        );
    }
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
criterion::criterion_group!(benches, supported::ssh);
#[cfg(any(target_os = "macos", target_os = "linux"))]
criterion::criterion_main!(benches);
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() {
    eprintln!("SSH adapter requires macOS or Linux");
}
