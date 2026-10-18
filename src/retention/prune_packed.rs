use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use std::{fs, io};

use super::MaintenanceIsolation;
use crate::{ObjectId, PackLimits, ReadLimits, Repository};

/// Work bounds for removing loose copies already held in installed primary packs.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct PrunePackedLimits {
    /// Maximum visited fanout directories and entries, including ignored names.
    /// Also bounds the combined baseline and post-proof pack-directory entries separately.
    pub max_entries: usize,
    /// Maximum total verified survivor payload bytes, before any deletion.
    pub max_bytes: u64,
    /// Bounds opening installed packs and their indexes.
    pub packs: PackLimits,
    /// Per-object payload, compressed-input, decoding and delta-depth bounds.
    pub read: ReadLimits,
}

impl Default for PrunePackedLimits {
    fn default() -> Self {
        Self {
            max_entries: 1_000_000,
            max_bytes: 1024 * 1024 * 1024,
            packs: PackLimits::default(),
            read: ReadLimits::default(),
        }
    }
}

/// Known effects of pruning verified loose duplicates, in path order.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct PrunePackedReport {
    /// Loose copies successfully removed; their verified packed survivors remain.
    pub deleted: Vec<ObjectId>,
    /// Named loose files without a primary indexed packed survivor, left untouched.
    pub retained: Vec<ObjectId>,
    /// Payload bytes verified from packed survivors, including proof before a later failure.
    pub verified_bytes: u64,
}

/// Why pruning packed loose duplicates stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PrunePackedCause {
    /// Caller-owned exclusion could not be acquired.
    #[error("prune-packed isolation unavailable")]
    Isolation(#[source] io::Error),
    /// Repository metadata could not be refreshed.
    #[error("cannot refresh prune-packed repository")]
    Open(#[source] Box<crate::repository::OpenError>),
    /// A directory, loose file or unlink could not be inspected or changed.
    #[error("prune-packed filesystem operation failed")]
    Io(#[source] io::Error),
    /// A packed survivor could not be decoded, identity-checked or pinned safely.
    #[error("prune-packed survivor verification failed")]
    Read(#[source] Box<crate::ObjectReadError>),
    /// A loose file, repository layout, pack or index changed after observation.
    #[error("prune-packed inputs changed")]
    Changed,
    /// A fanout or correctly named loose object is not a regular directory/file.
    #[error("prune-packed refuses non-regular loose storage")]
    NonRegular,
    /// Directory enumeration exhausted the caller's entry budget.
    #[error("prune-packed directory entry limit exceeded")]
    EntryLimit,
    /// Verifying the next packed survivor would exceed the aggregate payload budget.
    #[error("prune-packed survivor byte limit exceeded")]
    ByteLimit,
    /// Caller cancellation was observed; completed deletions are not rolled back.
    #[error("prune-packed cancelled")]
    Cancelled,
}

/// Failure preserving completed deletions and a possibly uncertain unlink boundary.
#[derive(Debug, thiserror::Error)]
#[error("prune-packed failed")]
#[non_exhaustive]
pub struct PrunePackedFailure {
    /// The cause that stopped work.
    #[source]
    pub cause: PrunePackedCause,
    /// Known effects and observations before the failure.
    pub report: Box<PrunePackedReport>,
    /// Loose identity whose unlink returned an error; inspect actual storage before retrying.
    pub uncertain: Option<ObjectId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl Stamp {
    fn read(path: &Path, directory: bool) -> Result<Self, PrunePackedCause> {
        let metadata = fs::symlink_metadata(path).map_err(PrunePackedCause::Io)?;
        if if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        } {
            return Err(PrunePackedCause::NonRegular);
        }
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().map_err(PrunePackedCause::Io)?,
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            },
        })
    }
}

struct Loose {
    id: ObjectId,
    path: PathBuf,
    stamp: Stamp,
}

impl Repository {
    /// Removes primary loose copies only after decoding their installed packed survivors.
    ///
    /// Caller-owned [`MaintenanceIsolation`] must exclude pack installs/removal, other writers,
    /// dependent readers and alternate-store consumers throughout the operation. The repository
    /// is refreshed under that guard. Only primary indexed packs supply evidence; loose fallback,
    /// implicit objects and alternates never do. Every selected survivor is decoded and hashed
    /// directly from its pinned pack before the first unlink. A damaged packed copy refuses the
    /// attempt even when ordinary lookup could rescue it through the loose file.
    ///
    /// This does not walk reachability, write packs or change references, reflogs, auxiliary
    /// indexes, `.keep` files or alternates. Kept packs can supply verified survivors. Named loose
    /// objects with no primary packed copy remain. Other filenames are ignored. Symlink fanouts
    /// and correctly named non-regular loose entries refuse the scan. Bounds and cancellation
    /// apply before deletion; cancellation after deletion preserves a partial report. Pack/index
    /// and loose-file metadata are checked again before removal; these observed-change checks
    /// cannot replace isolation against non-cooperating writers. Unix stamps include device,
    /// inode and metadata change time; other platforms compare length and modification time.
    /// Pack directory names are also compared using a separate cumulative `max_entries` budget.
    ///
    /// Each unlink is atomic, but the operation is not a transaction and makes no power-loss
    /// rollback promise. Packed survivors must already be durably installed by their publisher.
    ///
    /// # Errors
    ///
    /// Returns [`PrunePackedFailure`] for isolation refusal, malformed/changed storage, damaged
    /// survivors, filesystem errors, limits or cancellation. Proof failure before the first
    /// unlink removes nothing. After an unlink error, inspect `uncertain` and storage under
    /// isolation; retry only with fresh observations. Corruption checks are stricter than Git's
    /// name-only `prune-packed` selection.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::retention::{MaintenanceIsolation, PrunePackedLimits};
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// // Only this single-threaded example knows the disposable path.
    /// struct PrivateFixture;
    /// impl MaintenanceIsolation for PrivateFixture {
    ///     type Guard = ();
    ///     fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    /// let report = repo.prune_packed(
    ///     &mut PrivateFixture,
    ///     PrunePackedLimits::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    /// assert!(report.deleted.is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn prune_packed<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        limits: PrunePackedLimits,
        cancel: &AtomicBool,
    ) -> Result<PrunePackedReport, PrunePackedFailure> {
        let mut report = PrunePackedReport::default();
        let mut uncertain = None;
        let result = (|| {
            checkpoint(cancel)?;
            let _guard = isolation
                .acquire(self)
                .map_err(PrunePackedCause::Isolation)?;
            self.prune_packed_exclusive(limits, cancel, &mut report, &mut uncertain, (|| {}, || {}))
        })();
        result
            .map(|()| report.clone())
            .map_err(|cause| PrunePackedFailure {
                cause,
                report: Box::new(report),
                uncertain,
            })
    }

    fn prune_packed_exclusive(
        &self,
        limits: PrunePackedLimits,
        cancel: &AtomicBool,
        report: &mut PrunePackedReport,
        uncertain: &mut Option<ObjectId>,
        checkpoints: (impl FnOnce(), impl FnMut()),
    ) -> Result<(), PrunePackedCause> {
        let (after_proof, mut after_delete) = checkpoints;
        let current = Repository::open(self.git_dir())
            .map_err(|error| PrunePackedCause::Open(Box::new(error)))?;
        if current.object_dir() != self.object_dir()
            || current.object_format() != self.object_format()
        {
            return Err(PrunePackedCause::Changed);
        }
        let loose = inventory(&current, limits.max_entries, cancel)?;
        let directory = current.object_dir().join("pack");
        let directory_stamp = Stamp::read(&directory, true)?;
        let mut remaining_pack_entries = limits.max_entries;
        let pack_entries = pack_directory_entries(&directory, &mut remaining_pack_entries, cancel)?;
        let objects = current
            .objects_with(limits.packs)
            .map_err(|error| PrunePackedCause::Read(Box::new(error)))?;
        let packs = objects.primary_packs_with_paths();
        let mut selected = Vec::new();
        for loose in loose {
            checkpoint(cancel)?;
            let mut found = None;
            for (number, (_, pack)) in packs.iter().enumerate() {
                let position = pack
                    .find(loose.id)
                    .map_err(|error| PrunePackedCause::Read(Box::new(error)))?;
                if let Some(position) = position {
                    found = Some((number, position));
                    break;
                }
            }
            let Some((number, position)) = found else {
                report.retained.push(loose.id);
                continue;
            };
            let (_, pack) = packs[number];
            let remaining = limits.max_bytes.saturating_sub(report.verified_bytes);
            let (_, bytes) = pack
                .object_header(position, limits.read)
                .map_err(|error| PrunePackedCause::Read(Box::new(error)))?;
            if bytes as u64 > remaining {
                return Err(PrunePackedCause::ByteLimit);
            }
            let object = pack
                .read(position, limits.read, cancel)
                .map_err(|error| PrunePackedCause::Read(Box::new(error)))?;
            report.verified_bytes = report
                .verified_bytes
                .checked_add(object.data().len() as u64)
                .filter(|bytes| *bytes <= limits.max_bytes)
                .ok_or(PrunePackedCause::ByteLimit)?;
            selected.push((loose, number));
        }
        after_proof();
        checkpoint(cancel)?;
        if directory_stamp != Stamp::read(&directory, true)?
            || pack_entries
                != pack_directory_entries(&directory, &mut remaining_pack_entries, cancel)?
        {
            return Err(PrunePackedCause::Changed);
        }
        objects
            .check_packs_unchanged(cancel)
            .map_err(|error| PrunePackedCause::Read(Box::new(error)))?;
        for (loose, number) in selected {
            checkpoint(cancel)?;
            if loose.stamp != Stamp::read(&loose.path, false)?
                || directory_stamp != Stamp::read(&directory, true)?
            {
                return Err(PrunePackedCause::Changed);
            }
            let (_, pack) = packs[number];
            pack.check_unchanged()
                .map_err(|error| PrunePackedCause::Read(Box::new(error)))?;
            *uncertain = Some(loose.id);
            fs::remove_file(&loose.path).map_err(PrunePackedCause::Io)?;
            *uncertain = None;
            report.deleted.push(loose.id);
            after_delete();
        }
        Ok(())
    }
}

// Directory mtimes may lag on Windows. Compare names as well, with a cumulative
// observation budget so the baseline and post-proof checks stay bounded.
fn pack_directory_entries(
    directory: &Path,
    remaining: &mut usize,
    cancel: &AtomicBool,
) -> Result<std::collections::BTreeSet<std::ffi::OsString>, PrunePackedCause> {
    let mut names = std::collections::BTreeSet::new();
    for entry in fs::read_dir(directory).map_err(PrunePackedCause::Io)? {
        checkpoint(cancel)?;
        *remaining = remaining
            .checked_sub(1)
            .ok_or(PrunePackedCause::EntryLimit)?;
        names.insert(entry.map_err(PrunePackedCause::Io)?.file_name());
    }
    Ok(names)
}

fn checkpoint(cancel: &AtomicBool) -> Result<(), PrunePackedCause> {
    if cancel.load(Ordering::Relaxed) {
        Err(PrunePackedCause::Cancelled)
    } else {
        Ok(())
    }
}

fn inventory(
    repository: &Repository,
    max_entries: usize,
    cancel: &AtomicBool,
) -> Result<Vec<Loose>, PrunePackedCause> {
    let mut remaining = max_entries;
    let mut result = Vec::new();
    let mut visit = || {
        checkpoint(cancel)?;
        remaining = remaining
            .checked_sub(1)
            .ok_or(PrunePackedCause::EntryLimit)?;
        Ok::<(), PrunePackedCause>(())
    };
    for number in 0..256 {
        checkpoint(cancel)?;
        let prefix = format!("{number:02x}");
        let path = repository.object_dir().join(&prefix);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(PrunePackedCause::Io(error)),
        };
        visit()?;
        if !metadata.is_dir() {
            return Err(PrunePackedCause::NonRegular);
        }
        for entry in fs::read_dir(&path).map_err(PrunePackedCause::Io)? {
            visit()?;
            let entry = entry.map_err(PrunePackedCause::Io)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.len() != repository.object_format().digest_len() * 2 - 2
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                continue;
            }
            let id = ObjectId::from_hex(repository.object_format(), &format!("{prefix}{name}"))
                .map_err(|_| PrunePackedCause::Changed)?;
            let path = entry.path();
            let stamp = Stamp::read(&path, false)?;
            result.push(Loose { id, path, stamp });
        }
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::maintenance_git as oracle;

    fn fixture() -> (tempfile::TempDir, Repository) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("r"),
            crate::InitKind::Bare,
        )
        .unwrap();
        let mut input = Vec::new();
        for bytes in [b"first".as_slice(), b"second".as_slice()] {
            let id = repo.loose_objects().write_blob(bytes).unwrap();
            input.extend_from_slice(format!("{id}\n").as_bytes());
        }
        oracle::git(
            repo.git_dir(),
            &["pack-objects", "objects/pack/pack"],
            &input,
        );
        (root, repo)
    }

    #[test]
    fn pack_directory_observation_budget_refuses_before_unlink() {
        let (_root, repo) = fixture();
        let entries = fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count();
        let mut report = PrunePackedReport::default();
        let cause = repo
            .prune_packed_exclusive(
                PrunePackedLimits {
                    max_entries: entries * 2 - 1,
                    ..Default::default()
                },
                &AtomicBool::new(false),
                &mut report,
                &mut None,
                (|| {}, || {}),
            )
            .unwrap_err();
        assert!(matches!(cause, PrunePackedCause::EntryLimit));
        assert!(report.deleted.is_empty());
    }

    #[test]
    fn pack_install_after_proof_refuses_before_unlink() {
        let (_root, repo) = fixture();
        let mut report = PrunePackedReport::default();
        let failure = repo
            .prune_packed_exclusive(
                PrunePackedLimits::default(),
                &AtomicBool::new(false),
                &mut report,
                &mut None,
                (
                    || {
                        fs::write(
                            repo.object_dir().join("pack/new-install"),
                            b"new generation",
                        )
                        .unwrap();
                    },
                    || {},
                ),
            )
            .unwrap_err();
        assert!(matches!(failure, PrunePackedCause::Changed));
        assert!(report.deleted.is_empty());
    }

    #[test]
    fn changed_loose_file_after_proof_refuses_before_unlink() {
        let (_root, repo) = fixture();
        let entries = inventory(&repo, 100, &AtomicBool::new(false)).unwrap();
        let path = entries[0].path.clone();
        let mut report = PrunePackedReport::default();
        let failure = repo
            .prune_packed_exclusive(
                PrunePackedLimits::default(),
                &AtomicBool::new(false),
                &mut report,
                &mut None,
                (
                    || {
                        fs::rename(&path, _root.path().join("original.loose")).unwrap();
                        fs::write(&path, b"changed loose file").unwrap();
                    },
                    || {},
                ),
            )
            .unwrap_err();
        assert!(matches!(failure, PrunePackedCause::Changed));
        assert!(report.deleted.is_empty());
        assert!(path.exists());
    }

    #[test]
    fn cancellation_after_unlink_keeps_completed_report() {
        let (_root, repo) = fixture();
        let cancel = AtomicBool::new(false);
        let mut report = PrunePackedReport::default();
        let failure = repo
            .prune_packed_exclusive(
                PrunePackedLimits::default(),
                &cancel,
                &mut report,
                &mut None,
                (|| {}, || cancel.store(true, Ordering::Relaxed)),
            )
            .unwrap_err();
        assert!(matches!(failure, PrunePackedCause::Cancelled));
        assert_eq!(report.deleted.len(), 1);
        oracle::git(repo.git_dir(), &["fsck", "--full"], b"");
    }
}
