//! Run `cargo run --example fetch_local`; all repositories are disposable and local.
use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;

use girt::fetch::{FetchLimits, receive_local};
use girt::refs::{Expected, RefName, Target};
use girt::{
    Commit, CommitFields, EntryMode, PackLimits, ReadLimits, Repository, Signature, Tree, TreeEntry,
};

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
    let objects = source.loose_objects()?;
    let blob = objects.write_blob(b"Fetched through a local upload-pack server\n")?;
    let tree = objects.write_tree(&Tree::new(vec![TreeEntry {
        mode: EntryMode::Blob,
        name: b"hello.txt".to_vec(),
        id: blob,
    }])?)?;
    let author = Signature {
        name: b"Fetch Example".to_vec(),
        email: b"fetch@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let commit = objects.write_commit(&Commit::new(CommitFields {
        tree,
        parents: vec![],
        author: author.clone(),
        committer: author,
        extra_headers: vec![],
        message: b"Local fetch fixture\n".to_vec(),
    })?)?;
    let branch = RefName::new("refs/heads/main")?;
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
                .filter(|reference| reference.name == branch && !reference.peeled)
                .map(|reference| reference.id)
                .collect()
        },
        FetchLimits::default(),
        &cancel,
        |_| ControlFlow::Continue(()),
    )?;
    let result = received.install(&destination, &cancel)?;
    // Updating a ref is a separate, conditional operation, explicitly without a reflog.
    destination.references()?.update_without_reflog(
        &branch,
        Target::Direct(commit),
        Expected::Absent,
    )?;
    let fetched = destination.objects(PackLimits::default())?;
    let restored = fetched
        .read(blob, ReadLimits::default())?
        .ok_or("missing fetched blob")?;
    assert_eq!(
        restored.data(),
        b"Fetched through a local upload-pack server\n"
    );
    println!(
        "Fetched {} objects ({} pack bytes); published {commit} without a reflog",
        result.objects,
        received.pack_bytes()
    );
    Ok(())
}
