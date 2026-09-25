//! Measures live descriptor counts around an owned reftable snapshot and compaction.
use std::sync::atomic::AtomicBool;

use girt::refs::reftable::{Snapshot, StackLimits, compact};
use girt::refs::{Backend, Expected, RefName, Target};
use girt::{InitKind, ObjectFormat, ObjectId, Repository};

fn descriptors() -> Option<usize> {
    #[cfg(target_os = "linux")]
    let path = "/proc/self/fd";
    #[cfg(target_os = "macos")]
    let path = "/dev/fd";
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    return std::fs::read_dir(path).ok().map(Iterator::count);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    None
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let repo = Repository::init_with_backend(
        ObjectFormat::Sha1,
        root.path().join("repo"),
        InitKind::Bare,
        Backend::Reftable,
    )?;
    let before = descriptors();
    let refs = repo.references()?;
    let name = RefName::new(b"refs/tags/probe")?;
    for byte in 1..=16 {
        refs.update_without_reflog(
            &name,
            Target::Direct(ObjectId::Sha1([byte; 20])),
            Expected::Any,
        )?;
    }
    let directory = repo.git_dir().join("reftable");
    let snapshot = Snapshot::read(
        &directory,
        ObjectFormat::Sha1,
        StackLimits::default(),
        &AtomicBool::new(false),
    )?;
    let retained = descriptors();
    let report = compact(
        &directory,
        ObjectFormat::Sha1,
        StackLimits::default(),
        &AtomicBool::new(false),
    )?;
    let after_compaction = descriptors();
    assert!(report.retained.is_empty());
    assert_eq!(snapshot.table.references.len(), 2);
    assert!(
        Snapshot::read(
            &directory,
            ObjectFormat::Sha1,
            StackLimits::default(),
            &AtomicBool::new(true)
        )
        .is_err()
    );
    drop(snapshot);
    let after_drop = descriptors();
    println!(
        "descriptors before={before:?} retained={retained:?} compacted={after_compaction:?} dropped={after_drop:?}"
    );
    println!("compacted_tables={}", report.input_tables);
    Ok(())
}
