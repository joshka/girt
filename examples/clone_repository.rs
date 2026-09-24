//! Run `cargo run --example clone_repository`; all repositories are disposable.
use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use girt::clone::{BranchSelection, CloneRequest};
use girt::fetch::{FetchLimits, FetchUpdateLimits};
use girt::refs::{Expected, RefName, Reflog, Target};
use girt::remote::Remote;
use girt::transport::TransportControl;
use girt::{Commit, CommitFields, InitKind, Repository, Signature, Tree};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let source = Repository::init(root.path().join("source"), InitKind::Bare)?;
    let destination_path = root.path().join("destination");
    let objects = source.loose_objects()?;
    let tree = objects.write_tree(&Tree::new(Vec::new())?)?;
    let identity = Signature {
        name: b"Fetch Example".to_vec(),
        email: b"fetch@example.com".to_vec(),
        seconds: 1700000000,
        offset_minutes: 0,
    };
    let commit = objects.write_commit(&Commit::new(CommitFields {
        tree,
        parents: Vec::new(),
        author: identity.clone(),
        committer: identity.clone(),
        extra_headers: Vec::new(),
        message: b"Original fetch example\n".to_vec(),
    })?)?;
    source.references()?.update_without_reflog(
        &RefName::new("refs/heads/main")?,
        Target::Direct(commit),
        Expected::Absent,
    )?;

    // The persisted URL is credential-free metadata. Transport selection stays explicit.
    let source_path = std::fs::canonicalize(source.git_dir())?;
    let url = source_path
        .to_str()
        .ok_or("example needs a UTF-8 temporary path")?;
    let request = CloneRequest::prepare(
        &destination_path,
        InitKind::Worktree,
        url.as_bytes(),
        BranchSelection::Default,
        Reflog::Preserve,
    )?;
    let cancel = AtomicBool::new(false);
    let ready = request.receive_local(
        &source_path,
        FetchLimits::default(),
        TransportControl {
            cancel: &cancel,
            deadline: Some(Instant::now() + Duration::from_secs(30)),
        },
        |_| ControlFlow::Continue(()),
    )?;
    assert!(!destination_path.exists());
    // HTTP/SSH callers receive an owned download instead. Move it to a bounded worker, validate
    // synchronously, then finish here. Join that worker even after requesting cancellation.
    let report = match ready.finish(FetchUpdateLimits::default(), &cancel) {
        Ok(report) => report,
        Err(error) => {
            // Preserve both reports: nested fetch/transaction failures retain partial effects.
            // Production code should inspect the destination; never delete it automatically.
            eprintln!(
                "Clone effects: {:?}; failure: {}",
                error.report, error.source
            );
            return Err(error.into());
        }
    };
    let repository = report
        .repository
        .as_ref()
        .ok_or("missing completed repository")?;
    assert_eq!(
        repository
            .references()?
            .read(&RefName::new("refs/heads/main")?)?,
        Some(Target::Direct(commit))
    );
    assert!(!repository.git_dir().join("index").exists());
    assert_eq!(std::fs::read_dir(&destination_path)?.count(), 1); // Only .git; no checkout.
    let remote = Remote::find(repository.config(), b"origin")?.ok_or("missing origin")?;
    assert_eq!(remote.fetch_url(), Some(url.as_bytes()));
    // Pass remote.fetch_refspecs().clone() to FetchRequest::prepare for subsequent fetch.
    println!(
        "Created no-checkout repository at {} with {:?}",
        destination_path.display(),
        report.head
    );
    drop(report);
    root.close()?; // Cleanup belongs to this disposable example, not the clone operation.
    Ok(())
}
