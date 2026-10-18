//! Run `cargo run --example fetch_local`; all repositories are disposable and local.
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use girt::fetch::{FetchLimits, KnownHistory, receive_local};
use girt::refs::{Expected, RefName, Target};
use girt::transport::TransportControl;
use girt::{Commit, CommitFields, EntryMode, Repository, Signature, Tree, TreeEntry};

fn repository() -> Result<(tempfile::TempDir, Repository), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join("objects"))?;
    std::fs::create_dir(root.path().join("refs"))?;
    std::fs::write(root.path().join("HEAD"), b"ref: refs/heads/main\n")?;
    std::fs::write(root.path().join("config"), b"[core]\nbare=true\n")?;
    let repo = Repository::open(root.path())?;
    Ok((root, repo))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (_source_root, source) = repository()?;
    let (_destination_root, destination) = repository()?;

    let objects = source.loose_objects();
    let blob = objects.write_blob(b"Fetched through native local storage\n")?;
    let tree = objects.write_tree(&Tree::new(
        girt::ObjectFormat::Sha1,
        vec![TreeEntry {
            mode: EntryMode::Blob,
            name: b"hello.txt".to_vec(),
            id: blob,
        }],
    )?)?;

    let author = Signature::now(b"Fetch Example", b"fetch@example.com")?;
    let commit = objects.write_commit(&Commit::new(CommitFields {
        tree,
        parents: vec![],
        author: author.clone(),
        committer: author,
        extra_headers: vec![],
        message: b"Local fetch fixture\n".to_vec(),
    })?)?;

    let branch = RefName::branch("main")?;
    source.references()?.update_without_reflog(
        &branch,
        Target::Direct(commit),
        Expected::Absent,
    )?;

    let cancel = AtomicBool::new(false);
    let received = receive_local(
        source.git_dir(),
        |advertisement| {
            advertisement
                .refs
                .iter()
                .filter(|reference| {
                    reference.name == branch
                        && reference.kind == girt::fetch::AdvertisementKind::Tip
                })
                .map(|reference| reference.id)
                .collect()
        },
        girt::fetch::LocalFetchRequest::new(FetchLimits::default()),
        TransportControl::new(&cancel)
            .with_deadline(Some(Instant::now() + Duration::from_secs(30))),
        girt::fetch::LocalFetchObserver::default(),
    )?;

    let result = received.install(&destination, girt::PackLimits::default(), &cancel)?;

    // Updating a ref is a separate, conditional operation, explicitly without a reflog.
    destination.references()?.update_without_reflog(
        &branch,
        Target::Direct(commit),
        Expected::Absent,
    )?;

    let fetched = destination.objects()?;
    let restored = fetched.read(blob)?.ok_or("missing fetched blob")?;
    assert_eq!(restored.data(), b"Fetched through native local storage\n");

    println!(
        "Fetched {} objects ({} pack bytes); published {commit} without a reflog",
        result.objects,
        received.pack_bytes()
    );

    let known = KnownHistory::new(&fetched, &[commit], FetchLimits::default(), &cancel)?;
    let repeated = receive_local(
        source.git_dir(),
        |_| vec![commit],
        girt::fetch::LocalFetchRequest::new(FetchLimits::default()).with_known(&known),
        TransportControl::new(&cancel)
            .with_deadline(Some(Instant::now() + Duration::from_secs(30))),
        girt::fetch::LocalFetchObserver::default(),
    )?;

    // Recheck known dependencies in the destination even when no pack was needed.
    repeated.install(&destination, girt::PackLimits::default(), &cancel)?;
    assert_eq!(repeated.pack_bytes(), 0);
    println!(
        "Repeated fetch: {} objects, {} pack bytes",
        repeated.object_count(),
        repeated.pack_bytes()
    );

    Ok(())
}
