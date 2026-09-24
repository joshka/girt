use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use girt::fetch::{FetchLimits, KnownHistory, receive_http};
use girt::transport::TransportControl;
use girt::transport::http::HttpRemote;
use girt::{PackLimits, ReadLimits};

#[path = "../tests/support/http_git.rs"]
mod http_git;
#[path = "../tests/support/pack_git.rs"]
mod pack_git;

fn http(c: &mut Criterion) {
    let f = pack_git::Fixture::new(true, 16);
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
        b"HTTP benchmark next\n",
    );
    let new: girt::ObjectId = std::str::from_utf8(&new).unwrap().trim().parse().unwrap();
    pack_git::git(
        f.root.path(),
        &["update-ref", "refs/heads/main", &new.to_string()],
        b"",
    );
    let server = http_git::Server::new(f.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("http-loopback");
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
                    .block_on(receive_http(
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
    assert!(!server.requests().is_empty());
    eprintln!(
        "http-workload,blobs=16,payload_bytes={},objects={}",
        f.records.iter().map(|r| r.2.len()).sum::<usize>(),
        f.records.len()
    );
}
criterion_group!(benches, http);
criterion_main!(benches);
