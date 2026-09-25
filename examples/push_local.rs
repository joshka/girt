//! Run `cargo run --example push_local`; both repositories are disposable and local.
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use girt::push::{ForcePolicy, PreparedPush, PushCommand, PushLimits, send_local_with_control};
use girt::refs::RefName;
use girt::transport::TransportControl;
use girt::{
    Commit, CommitFields, EntryMode, ObjectKind, PackLimits, ReadLimits, Repository, Signature,
    Tag, TagFields, Tree, TreeEntry,
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
    let loose = source.loose_objects();
    let blob = loose.write_blob(b"Published by girt through local receive-pack\n")?;
    let tree = loose.write_tree(&Tree::new(
        girt::ObjectFormat::Sha1,
        vec![TreeEntry {
            mode: EntryMode::Blob,
            name: b"hello.txt".to_vec(),
            id: blob,
        }],
    )?)?;
    let author = Signature {
        name: b"Push Example".to_vec(),
        email: b"push@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let commit = loose.write_commit(&Commit::new(CommitFields {
        tree,
        parents: vec![],
        author: author.clone(),
        committer: author.clone(),
        extra_headers: vec![],
        message: b"Local push fixture\n".to_vec(),
    })?)?;
    let tag = loose.write_tag(&Tag::new(TagFields {
        target: commit,
        target_kind: ObjectKind::Commit,
        name: b"v1".to_vec(),
        tagger: Some(author),
        extra_headers: vec![],
        message: b"First publication\n".to_vec(),
    })?)?;
    let cancel = AtomicBool::new(false);
    let objects = source.objects(PackLimits::default())?;
    let commands = vec![
        PushCommand {
            name: RefName::new("refs/heads/main")?,
            expected: None,
            new: commit,
            force: ForcePolicy::FastForwardOnly,
        },
        PushCommand {
            name: RefName::new("refs/tags/v1")?,
            expected: None,
            new: tag,
            force: ForcePolicy::FastForwardOnly,
        },
    ];
    let prepared = PreparedPush::new(&objects, commands, PushLimits::default(), &cancel)?;
    let report = send_local_with_control(
        destination.git_dir(),
        &prepared,
        TransportControl {
            cancel: &cancel,
            deadline: Some(Instant::now() + Duration::from_secs(30)),
        },
    )?;
    // A complete protocol exchange can still contain rejection or partial success.
    if !report.all_succeeded() {
        return Err(format!("receive-pack rejected updates: {report:?}").into());
    }
    let restored = destination
        .objects(PackLimits::default())?
        .read(blob, ReadLimits::default())?
        .ok_or("missing pushed blob")?;
    assert_eq!(
        restored.data(),
        b"Published by girt through local receive-pack\n"
    );
    println!(
        "Published branch and annotated tag: {} objects, {} pack bytes",
        prepared.object_count(),
        prepared.pack_bytes()
    );
    let repeated = PreparedPush::new_excluding(
        &objects,
        vec![PushCommand {
            name: RefName::new("refs/heads/main")?,
            expected: Some(commit),
            new: commit,
            force: ForcePolicy::FastForwardOnly,
        }],
        &[commit],
        PushLimits::default(),
        &cancel,
    )?;
    let report = send_local_with_control(
        destination.git_dir(),
        &repeated,
        TransportControl {
            cancel: &cancel,
            deadline: Some(Instant::now() + Duration::from_secs(30)),
        },
    )?;
    if !report.all_succeeded() {
        return Err(format!("repeat rejected: {report:?}").into());
    }
    assert_eq!(repeated.object_count(), 0);
    println!(
        "Repeated push: {} objects, {} pack bytes",
        repeated.object_count(),
        repeated.pack_bytes()
    );
    Ok(())
}
