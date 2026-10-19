//! Additive, isolated loose-object packing; deletion is a separate operation.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fs, io};

use tempfile::NamedTempFile;

use super::{PackCompression, PackObject, PackWriteLimits, PackWritten, publish};
use crate::retention::MaintenanceIsolation;
use crate::{AlternateLimits, ObjectId, PackLimits, ReadLimits, Repository};

/// Bounds enumeration, decoding, encoding and verification of one additive loose-object batch.
///
/// Defaults visit at most one million entries, select at most 50,000 objects and encode at most
/// 512 MiB of payload and one GiB of compressed source input. Byte and object caps finish a
/// nonempty prefix and report further work; an oversized first object is refused. Each compressed
/// source file and decoded object also obeys `read`. Pack indexes may occupy 512 MiB, admitting
/// Linux-sized primary storage. No file is deleted.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct LoosePackLimits {
    /// Fanout directories and entries visited, including ignored names.
    pub max_entries: usize,
    /// Sum of compressed source-file lengths admitted before header decoding.
    /// Header and payload reads can each consume those bytes; this is not a syscall-byte counter.
    pub max_source_bytes: u64,
    /// Bounds source pack snapshots and opening the newly published pair.
    pub packs: PackLimits,
    /// Per-object compressed-source, payload and replacement decoding bounds.
    pub read: ReadLimits,
    /// Selected-object, aggregate payload and encoded artifact bounds.
    pub write: PackWriteLimits,
}

impl Default for LoosePackLimits {
    fn default() -> Self {
        Self {
            max_entries: 1_000_000,
            max_source_bytes: 1024 * 1024 * 1024,
            packs: PackLimits::default().with_max_index_bytes(512 * 1024 * 1024),
            read: ReadLimits::default(),
            write: PackWriteLimits::default().with_max_objects(50_000),
        }
    }
}

impl LoosePackLimits {
    /// Replaces the directory-entry budget.
    #[must_use]
    pub fn with_max_entries(mut self, value: usize) -> Self {
        self.max_entries = value;
        self
    }

    /// Replaces the aggregate compressed-source budget.
    #[must_use]
    pub fn with_max_source_bytes(mut self, value: u64) -> Self {
        self.max_source_bytes = value;
        self
    }

    /// Replaces pack snapshot bounds.
    #[must_use]
    pub fn with_packs(mut self, value: PackLimits) -> Self {
        self.packs = value;
        self
    }

    /// Replaces per-object read bounds.
    #[must_use]
    pub fn with_read(mut self, value: ReadLimits) -> Self {
        self.read = value;
        self
    }

    /// Replaces selected-object, payload and artifact bounds.
    #[must_use]
    pub fn with_write(mut self, value: PackWriteLimits) -> Self {
        self.write = value;
        self
    }
}

/// Confirmed visibility of an additive pair; the default confirms no publication.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum LoosePackPublication {
    /// No final name is confirmed visible.
    #[default]
    None,
    /// The pack is visible, with no confirmed index.
    Pack,
    /// Both pack and index are visible, even if a later sync or verification failed.
    Pair,
}

/// Observations and additive effects. The default describes no observations or publication.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct LoosePackReport {
    /// Selected and identity-verified loose objects, including before a later publication failure.
    /// This list never authorizes removing a loose copy; inspect publication and verify survivors.
    pub selected: Vec<ObjectId>,
    /// Loose identities skipped because an installed primary index already names them.
    /// No deletion is authorized by this index-only observation.
    pub skipped: Vec<ObjectId>,
    /// A non-indexed loose candidate remains for another batch or a larger per-object limit.
    pub more_loose: bool,
    /// Compressed source lengths admitted before header decoding, including failed decodes
    /// and objects deferred after their header reveals the payload cannot fit.
    pub source_bytes: u64,
    /// Encoded artifact statistics, present after private writing completes.
    pub written: Option<PackWritten>,
    /// Confirmed final-name visibility.
    pub publication: LoosePackPublication,
    /// Name whose directory synchronization failed; visibility does not imply durability.
    pub uncertain: Option<PathBuf>,
    /// Payload bytes identity-checked from the published pack.
    pub verified_bytes: u64,
}

/// Why additive loose packing stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoosePackError {
    /// Caller isolation could not be acquired.
    #[error("loose packing isolation unavailable")]
    Isolation(#[source] io::Error),
    /// Cancellation was observed between bounded phases.
    #[error("loose packing cancelled")]
    Cancelled,
    /// A work budget was exhausted.
    #[error("loose packing limit exceeded: {0}")]
    Limit(&'static str),
    /// A correctly named fanout or object is not regular storage.
    #[error("loose packing refuses non-regular storage: {0}")]
    NonRegular(PathBuf),
    /// Publication found conflicting existing bytes.
    #[error("conflicting loose pack artifact: {0}")]
    Existing(PathBuf),
    /// Filesystem failure.
    #[error("loose packing filesystem failure")]
    Io(#[from] io::Error),
    /// An existing decoder or encoder refused the data, preserving its typed diagnostic.
    #[error("cannot encode or verify loose pack")]
    Primitive(#[source] Box<crate::Error>),
    /// The published replacement does not have the selected object count.
    #[error("loose pack replacement changed")]
    Changed,
}

impl LoosePackError {
    fn primitive(error: impl Into<crate::Error>) -> Self {
        Self::Primitive(Box::new(error.into()))
    }
}

impl From<publish::Error> for LoosePackError {
    fn from(error: publish::Error) -> Self {
        match error {
            publish::Error::Cancelled => Self::Cancelled,
            publish::Error::Existing(path) => Self::Existing(path),
            publish::Error::Io(error) => Self::Io(error),
        }
    }
}

/// Additive packing failed; no loose copy was removed.
///
/// A pair may already be visible. Inspect `report` before retrying, including its uncertain path.
#[derive(Debug, thiserror::Error)]
#[error("cannot pack loose objects: {source}")]
#[non_exhaustive]
pub struct LoosePackFailure {
    /// Observations and publication progress before failure.
    pub report: Box<LoosePackReport>,
    /// Typed failure and underlying diagnostic.
    #[source]
    pub source: LoosePackError,
}

impl Repository {
    /// Packs a bounded batch of primary loose objects, keeping every loose file in place.
    ///
    /// Includes unreachable objects. Primary indexed duplicates are skipped; alternate packs are
    /// not used to skip primary loose objects. Snapshot discovery still loads alternates under
    /// `AlternateLimits::default()` and can refuse malformed or oversized alternate metadata. Every
    /// selected loose payload is identity-checked, encoded into an ordinary pack and verified
    /// again from the published pair. Kept/promisor packs are never rewritten or merged into
    /// the output. The caller's isolation must exclude pack installs/removals and loose-file
    /// changes throughout the operation.
    ///
    /// This is the additive half of loose-objects maintenance. A later isolated `prune_packed`
    /// may remove verified loose duplicates; newly packed loose copies deliberately remain here.
    /// No task, trigger, network access or background process is started. Cancellation is checked
    /// between bounded source reads, during encoding and replacement decoding, and at publication
    /// boundaries; a bounded inventory sort, loose header/payload decode or synchronization
    /// completes before the next check. Byte/object caps publish a nonempty prefix and report
    /// further work; a first object that cannot fit is refused.
    ///
    /// # Errors
    ///
    /// Refuses cancellation, non-regular storage, malformed objects, conflicting output names and
    /// exhausted budgets. Failed encoding leaves only private temporaries, while publication or
    /// verification failure may leave an indexed or unindexed pack, described in the partial
    /// report.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::pack::LoosePackLimits;
    /// use girt::retention::MaintenanceIsolation;
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// struct Exclusive;
    /// impl MaintenanceIsolation for Exclusive {
    ///     type Guard = ();
    ///     fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
    ///         // This example alone accesses its disposable repository.
    ///         Ok(())
    ///     }
    /// }
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let id = repo.loose_objects().write_blob(b"unreferenced payload")?;
    /// let report = repo.pack_loose(
    ///     &mut Exclusive,
    ///     LoosePackLimits::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    /// assert_eq!(report.selected, [id]);
    /// assert_eq!(
    ///     repo.loose_objects().read_blob(id, 1024)?,
    ///     b"unreferenced payload"
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn pack_loose<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        limits: LoosePackLimits,
        cancel: &AtomicBool,
    ) -> Result<LoosePackReport, LoosePackFailure> {
        self.pack_loose_with(isolation, limits, cancel, (|| {}, || {}))
    }

    fn pack_loose_with<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        limits: LoosePackLimits,
        cancel: &AtomicBool,
        checkpoints: (impl FnOnce(), impl FnOnce()),
    ) -> Result<LoosePackReport, LoosePackFailure> {
        let mut report = LoosePackReport::default();
        let result = (|| {
            check(cancel)?;
            let _guard = isolation.acquire(self).map_err(LoosePackError::Isolation)?;
            let candidates = inventory(self, limits.max_entries, cancel)?;
            if candidates.is_empty() {
                return Ok(());
            }
            let objects = self
                .objects_controlled(limits.packs, AlternateLimits::default(), cancel)
                .map_err(LoosePackError::primitive)?;
            let decoded = select(self, candidates, &objects, limits, cancel, &mut report)?;
            if decoded.is_empty() {
                return Ok(());
            }
            publish_batch(
                self,
                &objects,
                decoded,
                limits,
                cancel,
                &mut report,
                checkpoints,
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(report),
            Err(source) => Err(LoosePackFailure {
                report: Box::new(report),
                source,
            }),
        }
    }
}

fn select(
    repo: &Repository,
    candidates: Vec<(ObjectId, PathBuf)>,
    objects: &crate::Objects,
    limits: LoosePackLimits,
    cancel: &AtomicBool,
    report: &mut LoosePackReport,
) -> Result<Vec<(ObjectId, crate::Object)>, LoosePackError> {
    let primary = objects.primary_packs_with_paths();
    let mut decoded = Vec::new();
    let mut bytes = 0u64;
    for (id, path) in candidates {
        check(cancel)?;
        let mut indexed = false;
        for (_, pack) in &primary {
            if pack.find(id).map_err(LoosePackError::primitive)?.is_some() {
                indexed = true;
                break;
            }
        }
        if indexed {
            report.skipped.push(id);
            continue;
        }
        if decoded.len() >= limits.write.max_objects as usize {
            if decoded.is_empty() {
                return Err(LoosePackError::Limit("selected objects"));
            }
            report.more_loose = true;
            break;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() {
            return Err(LoosePackError::NonRegular(path));
        }
        if metadata.len() > limits.read.max_input_bytes as u64 {
            if decoded.is_empty() {
                return Err(LoosePackError::Limit("per-file compressed input"));
            }
            report.more_loose = true;
            break;
        }
        let remaining_source = limits.max_source_bytes - report.source_bytes;
        if metadata.len() > remaining_source {
            if decoded.is_empty() {
                return Err(LoosePackError::Limit("aggregate compressed input"));
            }
            report.more_loose = true;
            break;
        }
        // Charge the complete source before even decoding its bounded header.
        report.source_bytes += metadata.len();
        let remaining = limits.write.max_input_bytes - bytes;
        let max_payload = remaining
            .min(limits.write.max_object_bytes)
            .min(limits.read.max_object_bytes as u64)
            .min(limits.read.max_decode_bytes as u64);
        let (_, size) = repo
            .loose_objects()
            .locate(id)
            .map_err(LoosePackError::primitive)?;
        if size > max_payload {
            if decoded.is_empty() {
                return Err(LoosePackError::Limit("object payload bytes"));
            }
            report.more_loose = true;
            break;
        }
        let max_payload = usize::try_from(max_payload).unwrap_or(usize::MAX);
        let object = repo
            .loose_objects()
            .read_raw(id, max_payload)
            .map_err(LoosePackError::primitive)?;
        bytes = bytes
            .checked_add(object.data().len() as u64)
            .ok_or(LoosePackError::Limit("input bytes"))?;
        report.selected.push(id);
        decoded.push((id, object));
    }
    Ok(decoded)
}

fn publish_batch(
    repo: &Repository,
    objects: &crate::Objects,
    decoded: Vec<(ObjectId, crate::Object)>,
    limits: LoosePackLimits,
    cancel: &AtomicBool,
    report: &mut LoosePackReport,
    checkpoints: (impl FnOnce(), impl FnOnce()),
) -> Result<(), LoosePackError> {
    check(cancel)?;
    let input: Vec<_> = decoded
        .iter()
        .map(|(id, object)| PackObject {
            id: *id,
            kind: object.kind(),
            data: object.data(),
        })
        .collect();
    let directory = repo.object_dir().join("pack");
    fs::create_dir_all(&directory)?;
    // Persist a newly created pack directory before a later run can prune loose copies.
    publish::sync_directory(repo.object_dir())?;
    let mut pack = NamedTempFile::new_in(&directory)?;
    let mut index = NamedTempFile::new_in(&directory)?;
    let written = super::write::write_controlled(
        repo.object_format(),
        &input,
        &mut pack,
        &mut index,
        limits.write,
        PackCompression::Ordinary,
        &mut || {
            if cancel.load(Ordering::Relaxed) {
                Err(super::PackWriteError::Io(io::ErrorKind::Interrupted.into()))
            } else {
                Ok(())
            }
        },
    );
    check(cancel)?;
    let written = written.map_err(LoosePackError::primitive)?;
    report.written = Some(written);
    drop(input);
    drop(decoded);
    pack.as_file().sync_all()?;
    index.as_file().sync_all()?;
    objects
        .check_packs_unchanged(cancel)
        .map_err(LoosePackError::primitive)?;
    let base = directory.join(format!("pack-{}", written.checksum));
    let mut progress = publish::Progress::default();
    let (after_pack, after_pair) = checkpoints;
    let mut after_pack = Some(after_pack);
    let result = publish::pair_checked(
        publish::Pair {
            pack,
            index,
            basename: &base,
            marker: None,
        },
        cancel,
        &mut progress,
        |step| {
            if step == publish::Step::Index {
                after_pack.take().expect("Index step runs once")();
                check(cancel)?;
            }
            objects
                .check_packs_unchanged(cancel)
                .map_err(LoosePackError::primitive)
        },
    );
    report.publication = match progress.visible.last() {
        Some(path) if path.extension().is_some_and(|ext| ext == "idx") => {
            LoosePackPublication::Pair
        }
        Some(_) => LoosePackPublication::Pack,
        None => LoosePackPublication::None,
    };
    report.uncertain = progress.uncertain;
    result?;
    after_pair();
    check(cancel)?;
    let mut opening = limits.packs;
    let replacement = super::FilePack::open(
        repo.object_format(),
        &base.with_extension("idx"),
        &mut opening,
        cancel,
    )
    .map_err(LoosePackError::primitive)?;
    if replacement.object_count() != report.selected.len() {
        return Err(LoosePackError::Changed);
    }
    replacement
        .verify(cancel)
        .map_err(LoosePackError::primitive)?;
    for position in 0..replacement.object_count() {
        check(cancel)?;
        let object = replacement
            .read(position, limits.read, cancel)
            .map_err(LoosePackError::primitive)?;
        report.verified_bytes = report
            .verified_bytes
            .checked_add(object.data().len() as u64)
            .filter(|bytes| *bytes <= limits.write.max_input_bytes)
            .ok_or(LoosePackError::Limit("verification bytes"))?;
    }
    Ok(())
}

fn check(cancel: &AtomicBool) -> Result<(), LoosePackError> {
    if cancel.load(Ordering::Relaxed) {
        Err(LoosePackError::Cancelled)
    } else {
        Ok(())
    }
}

fn inventory(
    repo: &Repository,
    max_entries: usize,
    cancel: &AtomicBool,
) -> Result<Vec<(ObjectId, PathBuf)>, LoosePackError> {
    let mut remaining = max_entries;
    let mut result = Vec::new();
    let mut visit = || {
        check(cancel)?;
        remaining = remaining
            .checked_sub(1)
            .ok_or(LoosePackError::Limit("directory entries"))?;
        Ok::<(), LoosePackError>(())
    };
    for number in 0..256 {
        check(cancel)?;
        let prefix = format!("{number:02x}");
        let directory = repo.object_dir().join(&prefix);
        let metadata = match fs::symlink_metadata(&directory) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        visit()?;
        if !metadata.is_dir() {
            return Err(LoosePackError::NonRegular(directory));
        }
        for entry in fs::read_dir(&directory)? {
            visit()?;
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.len() != repo.object_format().digest_len() * 2 - 2
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                continue;
            }
            let id = ObjectId::from_hex(repo.object_format(), &format!("{prefix}{name}"))
                .map_err(|_| LoosePackError::Limit("object name"))?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if !metadata.is_file() {
                return Err(LoosePackError::NonRegular(entry.path()));
            }
            result.push((id, entry.path()));
        }
    }
    result.sort_unstable_by_key(|(id, _)| *id);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InitKind, ObjectFormat};

    struct Exclusive;
    impl MaintenanceIsolation for Exclusive {
        type Guard = ();
        fn acquire(&mut self, _: &Repository) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn cancellation_at_each_publication_boundary_preserves_loose_copies() {
        for at_pack in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let repo = Repository::init(ObjectFormat::Sha1, root.path().join("r"), InitKind::Bare)
                .unwrap();
            let id = repo
                .loose_objects()
                .write_blob(b"keep the loose copy")
                .unwrap();
            let cancel = AtomicBool::new(false);
            let failure = repo
                .pack_loose_with(
                    &mut Exclusive,
                    LoosePackLimits::default(),
                    &cancel,
                    (
                        || {
                            if at_pack {
                                cancel.store(true, Ordering::Relaxed);
                            }
                        },
                        || {
                            if !at_pack {
                                cancel.store(true, Ordering::Relaxed);
                            }
                        },
                    ),
                )
                .unwrap_err();
            assert!(matches!(failure.source, LoosePackError::Cancelled));
            assert_eq!(
                failure.report.publication,
                if at_pack {
                    LoosePackPublication::Pack
                } else {
                    LoosePackPublication::Pair
                }
            );
            assert_eq!(
                repo.loose_objects().read_blob(id, 1024).unwrap(),
                b"keep the loose copy"
            );
        }
    }

    #[test]
    fn damaged_published_pack_is_reported_without_removing_loose_copy() {
        let root = tempfile::tempdir().unwrap();
        let repo =
            Repository::init(ObjectFormat::Sha1, root.path().join("r"), InitKind::Bare).unwrap();
        let id = repo.loose_objects().write_blob(b"keep me").unwrap();
        let failure = repo
            .pack_loose_with(
                &mut Exclusive,
                LoosePackLimits::default(),
                &AtomicBool::new(false),
                (
                    || {},
                    || {
                        let pack = fs::read_dir(repo.object_dir().join("pack"))
                            .unwrap()
                            .map(|entry| entry.unwrap().path())
                            .find(|path| path.extension().is_some_and(|ext| ext == "pack"))
                            .unwrap();
                        fs::write(pack, b"damaged").unwrap();
                    },
                ),
            )
            .unwrap_err();
        assert_eq!(failure.report.publication, LoosePackPublication::Pair);
        assert!(matches!(failure.source, LoosePackError::Primitive(_)));
        assert_eq!(
            repo.loose_objects().read_blob(id, 1024).unwrap(),
            b"keep me"
        );
    }
}
