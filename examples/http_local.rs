//! Runs only against two disposable repositories and a loopback Git http-backend fixture.
#[path = "../tests/support/http_git.rs"]
mod http_git;

use std::ops::ControlFlow;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use girt::fetch::{FetchLimits, KnownHistory, receive_http};
use girt::push::{ForcePolicy, PreparedPush, PushCommand, PushLimits, send_http};
use girt::refs::RefName;
use girt::transport::TransportControl;
use girt::transport::http::HttpRemote;
use girt::{PackLimits, Repository};

fn git(path: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(path)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success());
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = tempfile::tempdir()?;
    let destination = tempfile::tempdir()?;
    git(
        source.path(),
        &[
            "init",
            "--bare",
            "--template=",
            "--initial-branch=main",
            "--object-format=sha1",
            ".",
        ],
    );
    git(
        destination.path(),
        &[
            "init",
            "--bare",
            "--template=",
            "--initial-branch=main",
            "--object-format=sha1",
            ".",
        ],
    );
    git(destination.path(), &["config", "http.receivepack", "true"]);
    let source_repo = Repository::open(source.path())?;
    let destination_repo = Repository::open(destination.path())?;
    let id = source_repo
        .loose_objects()?
        .write_blob(b"Disposable HTTP example\n")?;
    let cancel = AtomicBool::new(false);
    let control = TransportControl::new(&cancel);
    let prepared = PreparedPush::new(
        &source_repo.objects(PackLimits::default())?,
        vec![PushCommand {
            name: RefName::new("refs/tags/example")?,
            expected: None,
            new: id,
            force: ForcePolicy::FastForwardOnly,
        }],
        PushLimits::default(),
        &cancel,
    )?;
    let server = http_git::Server::new(destination_repo.git_dir(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[])?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let report = runtime.block_on(send_http(&remote, prepared, control))?;
    assert!(report.all_succeeded());
    let download = runtime.block_on(receive_http(
        &remote,
        |_| vec![id],
        None,
        FetchLimits::default(),
        control,
    ))?;
    // CPU validation is deliberately outside block_on. Services can use their bounded worker pool.
    let validated = download.validate(&cancel, |_| ControlFlow::Continue(()))?;
    assert_eq!(validated.object_count(), 1);

    // A second fetch can reuse verified local history without borrowing its initiating scope.
    // Prepare history outside the executor: it reads, hashes and walks local objects.
    let known = Arc::new(KnownHistory::new(
        &source_repo.objects(PackLimits::default())?,
        &[id],
        FetchLimits::default(),
        &cancel,
    )?);
    let workers = Arc::new(tokio::sync::Semaphore::new(1));
    let validation_cancel = Arc::new(AtomicBool::new(false));
    let validated = runtime.block_on(async {
        let download = receive_http(
            &remote,
            |_| vec![id],
            Some(Arc::clone(&known)),
            FetchLimits::default(),
            control,
        )
        .await?;
        drop(known);
        // This example submits one request, bounding both waiting downloads and running work.
        // A service must also bound its queue and aggregate retained bytes before downloading.
        let permit = workers.acquire_owned().await?;
        let worker_cancel = Arc::clone(&validation_cancel);
        let completion = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            download.validate(&worker_cancel, |_| ControlFlow::Continue(()))
        });
        // Keep observing completion if cancellation is requested. Dropping this handle would
        // detach a started worker; it would not stop validation or release its permit early.
        let result = completion.await?; // Join failure and validation failure are separate outcomes.
        Ok::<_, Box<dyn std::error::Error>>(result?)
    })?;
    assert_eq!(validated.pack_bytes(), 0);
    // Installation is an explicit blocking step outside the executor; it rechecks dependencies.
    validated.install(
        &source_repo,
        girt::PackLimits::default(),
        &validation_cancel,
    )?;

    assert_eq!(server.requests(), ["GET", "POST", "GET", "POST", "GET"]);
    println!("Pushed and fetched {id} through disposable smart HTTP");
    Ok(())
}
