//! Run `cargo run --example fetch_remote`; Git serves a disposable local repository.
use std::collections::BTreeSet;
use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use girt::fetch::{FetchLimits, FetchRequest, FetchUpdateLimits, KnownHistory};
use girt::refs::{Expected, RefName, Reflog, Target};
use girt::remote::Remote;
use girt::transport::TransportControl;
use girt::{Commit, CommitFields, Config, InitKind, Repository, Signature, Tree};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let source = Repository::init(
        girt::ObjectFormat::Sha1,
        root.path().join("source"),
        InitKind::Bare,
    )?;
    let destination_path = root.path().join("destination");
    let destination =
        Repository::init(girt::ObjectFormat::Sha1, &destination_path, InitKind::Bare)?;
    let objects = source.loose_objects();
    let tree = objects.write_tree(&Tree::new(girt::ObjectFormat::Sha1, Vec::new())?)?;
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

    // This explicit config snapshot could instead be destination.config(). The example uses a
    // literal local endpoint token so config escaping is independent of temporary path spelling.
    let config = Config::parse(
        b"[remote \"origin\"]\nurl = local-example\nfetch = +refs/heads/*:refs/remotes/origin/*\n",
    )?;
    let remote = Remote::find(&config, b"origin")?.ok_or("missing remote")?;
    assert_eq!(remote.fetch_url(), Some(b"local-example".as_slice()));
    // Consumer endpoint policy resolves this token to the disposable source, with no credentials.
    // An HTTP/SSH consumer constructs its explicit remote and chooses receive_http/receive_ssh.
    let request = FetchRequest::prepare(
        destination,
        remote.fetch_refspecs().clone(),
        BTreeSet::new(),
        Reflog::Append {
            committer: identity,
            message: b"fetch origin".to_vec(),
        },
    )?;
    let cancel = AtomicBool::new(false);
    let control = TransportControl {
        cancel: &cancel,
        deadline: Some(Instant::now() + Duration::from_secs(30)),
    };
    let ready = request.receive_local(
        source.git_dir(),
        &KnownHistory::default(),
        FetchLimits::default(),
        control,
        |_| ControlFlow::Continue(()),
    )?;
    let report = match ready.finish(FetchUpdateLimits::default(), &cancel) {
        Ok(report) => report,
        Err(error) => {
            // Retain both parts: installed objects may remain, and a transaction error can contain
            // partial reference/reflog effects. Retrying requires a fresh destination snapshot.
            eprintln!(
                "Fetch effects: {:?}; failure: {}",
                error.report, error.source
            );
            return Err(error.into());
        }
    };
    let destination = Repository::open(&destination_path)?;
    assert_eq!(
        destination
            .references()?
            .read(&RefName::new("refs/remotes/origin/main")?)?,
        Some(Target::Direct(commit))
    );
    assert!(!destination.git_dir().join("FETCH_HEAD").exists());
    println!(
        "Validated {} objects, installed {:?}, published {} reference(s)",
        report.objects,
        report.installed,
        report.references.len()
    );
    // TempDir removes both repositories after all synchronous work has completed.
    Ok(())
}
