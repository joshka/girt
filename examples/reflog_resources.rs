//! Original bounded history fixtures; warm-cache read, interpretation and root recovery
//! observations.
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use girt::refs::{Backend, ImportedRecord, RefName, ReflogLimits};
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
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for count in [100, 10_000] {
            probe(format, count, Backend::Files)?;
            probe(format, count, Backend::Reftable)?;
        }
    }
    Ok(())
}
fn probe(
    format: ObjectFormat,
    count: usize,
    backend: Backend,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let repo =
        Repository::init_with_backend(format, root.path().join("repo"), InitKind::Bare, backend)?;
    let tip = ObjectId::from_hex(format, &"12".repeat(format.digest_len()))?;
    let record = format!(
        "{} {tip} A <a@b> 1700000000 +0000\tfixture\n",
        ObjectId::null(format)
    );
    let bytes = record.repeat(count);
    if backend == Backend::Files {
        std::fs::create_dir_all(repo.git_dir().join("logs"))?;
        std::fs::write(repo.git_dir().join("logs/HEAD"), &bytes)?;
    } else {
        write_binary(&repo, tip, count)?;
    }
    let refs = repo.references()?;
    let name = RefName::new("HEAD")?;
    let cancel = AtomicBool::new(false);
    let before = descriptors();
    let retained = refs
        .imported_reflog(&name, ReflogLimits::default(), &cancel)?
        .unwrap();
    let during = descriptors();
    assert!(retained.is_complete());
    let payload: usize = retained
        .records()
        .iter()
        .map(|r| match r {
            ImportedRecord::File { bytes, .. } => bytes.len(),
            ImportedRecord::Reftable { value, .. } => {
                value.name.len()
                    + value.email.len()
                    + value.message.len()
                    + 2 * format.digest_len()
                    + 18
            }
        })
        .sum();
    drop(retained);
    let after = descriptors();
    let mut samples = Vec::new();
    for _ in 0..30 {
        let start = Instant::now();
        let log = refs
            .imported_reflog(&name, ReflogLimits::default(), &cancel)?
            .unwrap();
        assert!(log.is_complete());
        assert_eq!(log.recoverable_roots().count(), count);
        drop(log);
        samples.push(start.elapsed().as_nanos());
    }
    samples.sort_unstable();
    println!(
        "backend={backend:?} format={format} records={count} payload={payload} min_ns={} median_ns={} max_ns={} descriptors={before:?}/{during:?}/{after:?}",
        samples[0], samples[15], samples[29]
    );
    Ok(())
}

fn write_binary(
    repo: &Repository,
    tip: ObjectId,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use girt::refs::reftable::{Limits, LogRecord, LogValue, RecordName, Table};
    let table = Table {
        format: tip.format(),
        min_update_index: 1,
        max_update_index: count as u64,
        references: Vec::new(),
        logs: (1..=count as u64)
            .rev()
            .map(|index| LogRecord {
                name: RecordName::new(b"HEAD").unwrap(),
                update_index: index,
                value: Some(LogValue {
                    old: ObjectId::null(tip.format()),
                    new: tip,
                    name: b"A".to_vec(),
                    email: b"a@b".to_vec(),
                    seconds: 1700000000,
                    offset_minutes: 0,
                    message: b"fixture\n".to_vec(),
                }),
            })
            .collect(),
    };
    std::fs::write(
        repo.git_dir().join("reftable/original.ref"),
        table.encode(Limits::default())?,
    )?;
    std::fs::write(
        repo.git_dir().join("reftable/tables.list"),
        b"original.ref\n",
    )?;
    Ok(())
}
