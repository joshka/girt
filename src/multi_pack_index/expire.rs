use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use crate::retention::MaintenanceIsolation;
use crate::{PackLimits, ReadLimits, Repository};

/// Bounds for verifying and retiring unused packs from a standalone multi-pack index.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct ExpireLimits {
    /// Maximum combined pack-directory observations across all phases, including repeats.
    pub max_entries: usize,
    /// Maximum total decoded survivor payload bytes before publication.
    pub max_bytes: u64,
    /// Input MIDX parsing bounds.
    pub index: super::Limits,
    /// Replacement MIDX encoding and working-memory bounds.
    pub write: super::WriteLimits,
    /// Opening bounds for installed packs and indexes, including raw verification work.
    pub packs: PackLimits,
    /// Individual survivor decoding and delta-depth bounds.
    pub read: ReadLimits,
}

impl Default for ExpireLimits {
    fn default() -> Self {
        Self {
            max_entries: 1_000_000,
            max_bytes: 1024 * 1024 * 1024,
            index: Default::default(),
            write: Default::default(),
            packs: Default::default(),
            read: Default::default(),
        }
    }
}

/// Known effects of one standalone multi-pack-index expiry attempt.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ExpireReport {
    /// A synchronized replacement index excluding the deletion set became visible.
    pub index_published: bool,
    /// Successfully unlinked artifact paths, including indexes and recognized pack sidecars.
    pub removed: Vec<PathBuf>,
    /// Named primary pack paths preserved because they are protected, recent or still own objects.
    pub preserved: Vec<PathBuf>,
    /// Packed survivor payload bytes decoded and identity-checked.
    pub verified_bytes: u64,
}

/// Why standalone multi-pack-index expiry stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExpireCause {
    /// Caller isolation was unavailable.
    #[error("MIDX expiry isolation unavailable")]
    Isolation(#[source] std::io::Error),
    /// A filesystem operation failed.
    #[error("MIDX expiry filesystem operation failed")]
    Io(#[source] std::io::Error),
    /// Input MIDX parsing or verification failed.
    #[error("invalid MIDX expiry input")]
    Index(#[source] super::Error),
    /// Replacement MIDX encoding failed.
    #[error("cannot encode MIDX expiry replacement")]
    Write(#[source] super::WriteError),
    /// A pinned pack/index or survivor could not be verified.
    #[error("MIDX expiry packed-object verification failed")]
    Read(#[source] Box<crate::ObjectReadError>),
    /// Repository layout, input generations or protected files changed unexpectedly.
    #[error("MIDX expiry inputs changed")]
    Changed,
    /// A caller-supplied observation or survivor budget was exhausted.
    #[error("MIDX expiry limit exceeded: {0}")]
    Limit(&'static str),
    /// Caller cancellation was observed; inspect the report for completed effects.
    #[error("MIDX expiry cancelled")]
    Cancelled,
    /// Destructive directory durability is not implemented on this platform.
    #[error("MIDX expiry is unsupported on this platform")]
    UnsupportedPlatform,
    /// This operation currently requires a standalone index without base layers.
    #[error("incremental MIDX expiry is not supported")]
    IncrementalChain,
    /// Repository metadata could not be refreshed under isolation.
    #[error("cannot refresh MIDX expiry repository")]
    Open(#[source] Box<crate::repository::OpenError>),
}

/// MIDX expiry failure with publication/removal effects and an uncertain filesystem boundary.
#[derive(Debug, thiserror::Error)]
#[error("MIDX expiry failed")]
#[non_exhaustive]
pub struct ExpireFailure {
    /// The cause that stopped work.
    #[source]
    pub cause: ExpireCause,
    /// Known effects before the failure; cancellation never rolls them back.
    pub report: Box<ExpireReport>,
    /// Artifact at a failed unlink or directory-sync boundary; inspect it before retrying.
    pub uncertain: Option<PathBuf>,
}

impl Repository {
    /// Expires verified redundant primary packs that own no objects in a standalone MIDX.
    ///
    /// Caller-owned [`MaintenanceIsolation`] excludes pack installation/removal, writers and
    /// dependent readers for the entire operation. A MIDX writer lock separately coordinates
    /// auxiliary publishers. The input index and retiring packs are verified; each retiring
    /// identity must have a directly decoded and hashed survivor in a retained primary pack.
    /// Loose/alternate fallback never supplies deletion evidence. All proofs finish before
    /// publication. A synchronized replacement index is published before any artifact is removed.
    ///
    /// Only packs named in the starting standalone MIDX can expire. Packs still owning objects,
    /// packs newer than that index, `.keep`, promisor, cruft and unknown-sidecar storage remain.
    /// Girt conservatively preserves promisor packs even where Git would expire them. Recognized
    /// retired-pack `.rev`/`.bitmap` sidecars can be removed; checksum-named MIDX sidecars remain
    /// unreferenced after replacement. References, reflogs, loose objects and alternates are
    /// unchanged. This does not repack or run gc. Incremental chains and non-Unix destructive
    /// expiry refuse explicitly before publication.
    ///
    /// Failure can leave a published new index and redundant old files. The report identifies
    /// exact completed removals and an uncertain boundary. Retry only after reconciling storage
    /// under fresh isolation. Hostile directory replacement is outside the exclusion contract.
    ///
    /// # Errors
    ///
    /// Returns [`ExpireFailure`] for isolation/lock refusal, invalid or changed inputs, damaged
    /// survivors, limits, cancellation or filesystem errors. Before publication, all source files
    /// remain; after publication, no newly published index names a deleted pack.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::multi_pack_index::{ExpireLimits, WriteOptions};
    /// use girt::retention::{MaintenanceIsolation, RepackLimits, RetentionPolicy};
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// # if cfg!(not(unix)) { return Ok(()); }
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// // Only this single-threaded fixture knows the disposable repository path.
    /// struct PrivateFixture;
    /// impl MaintenanceIsolation for PrivateFixture {
    ///     type Guard = ();
    ///     fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    /// let cancel = AtomicBool::new(false);
    /// repo.loose_objects().write_blob(b"packed survivor")?;
    /// repo.repack_retained(
    ///     &RetentionPolicy::default(),
    ///     RepackLimits::default(),
    ///     &cancel,
    /// )?;
    /// repo.write_multi_pack_index(&mut PrivateFixture, WriteOptions::default())?;
    /// let report =
    ///     repo.expire_multi_pack_index(&mut PrivateFixture, ExpireLimits::default(), &cancel)?;
    /// // The sole pack still owns an indexed object, so nothing expires.
    /// assert!(report.removed.is_empty());
    /// assert_eq!(report.preserved.len(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn expire_multi_pack_index<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        limits: ExpireLimits,
        cancel: &AtomicBool,
    ) -> Result<ExpireReport, ExpireFailure> {
        let mut report = ExpireReport::default();
        let mut uncertain = None;
        let result = (|| {
            check(cancel)?;
            let _guard = isolation.acquire(self).map_err(ExpireCause::Isolation)?;
            self.expire_midx_exclusive(
                limits,
                cancel,
                &mut report,
                &mut uncertain,
                (|| {}, || {}, || {}),
            )
        })();
        match result {
            Ok(()) => Ok(report),
            Err(cause) => Err(ExpireFailure {
                cause,
                report: Box::new(report),
                uncertain,
            }),
        }
    }
}

#[cfg(not(unix))]
impl Repository {
    fn expire_midx_exclusive(
        &self,
        _limits: ExpireLimits,
        _cancel: &AtomicBool,
        _report: &mut ExpireReport,
        _uncertain: &mut Option<PathBuf>,
        _checkpoints: (impl FnOnce(), impl FnOnce(), impl FnMut()),
    ) -> Result<(), ExpireCause> {
        Err(ExpireCause::UnsupportedPlatform)
    }
}

fn check(cancel: &AtomicBool) -> Result<(), ExpireCause> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        Err(ExpireCause::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(unix)]
mod native {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::time::SystemTime;

    use super::*;
    use crate::multi_pack_index::{MultiPackIndex, WriteOptions, WriterLock};

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Stamp {
        len: u64,
        modified: SystemTime,
        identity: (u64, u64, i64, i64),
    }
    impl Stamp {
        fn read(path: &Path) -> Result<Self, ExpireCause> {
            let metadata = fs::symlink_metadata(path).map_err(ExpireCause::Io)?;
            if !metadata.is_file() {
                return Err(ExpireCause::Changed);
            }
            Ok(Self {
                len: metadata.len(),
                modified: metadata.modified().map_err(ExpireCause::Io)?,
                identity: (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                ),
            })
        }
    }
    type Inputs = BTreeMap<PathBuf, Stamp>;

    fn observe(
        directory: &Path,
        remaining: &mut usize,
        cancel: &AtomicBool,
        ignore: Option<&Path>,
    ) -> Result<Inputs, ExpireCause> {
        if directory
            .join("multi-pack-index.d/multi-pack-index-chain")
            .try_exists()
            .map_err(ExpireCause::Io)?
        {
            return Err(ExpireCause::IncrementalChain);
        }
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(directory).map_err(ExpireCause::Io)? {
            check(cancel)?;
            *remaining = remaining
                .checked_sub(1)
                .ok_or(ExpireCause::Limit("directory entries"))?;
            let path = entry.map_err(ExpireCause::Io)?.path();
            if ignore == Some(path.as_path()) {
                continue;
            }
            // The optional incremental-index directory is not mutated by standalone expiry.
            if path
                .file_name()
                .is_some_and(|name| name == "multi-pack-index.d")
            {
                continue;
            }
            files.insert(path.clone(), Stamp::read(&path)?);
        }
        Ok(files)
    }

    fn read_index(path: &Path, max_bytes: usize) -> Result<Vec<u8>, ExpireCause> {
        let file = fs::File::open(path).map_err(ExpireCause::Io)?;
        if file.metadata().map_err(ExpireCause::Io)?.len() > max_bytes as u64 {
            return Err(ExpireCause::Limit("MIDX bytes"));
        }
        let mut bytes = Vec::new();
        file.take((max_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(ExpireCause::Io)?;
        if bytes.len() > max_bytes {
            return Err(ExpireCause::Limit("MIDX bytes"));
        }
        Ok(bytes)
    }

    struct Selected {
        named: BTreeSet<PathBuf>,
        retiring: BTreeSet<PathBuf>,
    }

    fn select_packs(
        index: &MultiPackIndex<'_>,
        directory: &Path,
        inputs: &Inputs,
        cancel: &AtomicBool,
        preserved: &mut Vec<PathBuf>,
    ) -> Result<Selected, ExpireCause> {
        let mut owned = vec![false; index.pack_names().len()];
        for number in 0..index.len() {
            check(cancel)?;
            let location = index.location(number).map_err(ExpireCause::Index)?;
            owned[location.pack as usize] = true;
        }
        let mut named = BTreeSet::new();
        let mut retiring = BTreeSet::new();
        let index_modified = inputs
            .get(&directory.join("multi-pack-index"))
            .ok_or(ExpireCause::Changed)?
            .modified;
        for (ordinal, name) in index.pack_names().iter().enumerate() {
            check(cancel)?;
            let path = directory.join(name).with_extension("pack");
            named.insert(path.clone());
            let stem = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or(ExpireCause::Changed)?;
            let protected = inputs.keys().any(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_prefix(stem))
                    .is_some_and(|extension| {
                        !matches!(extension, ".pack" | ".idx" | ".rev" | ".bitmap")
                    })
            });
            let stamp = inputs.get(&path).ok_or(ExpireCause::Changed)?;
            if owned[ordinal] || protected || stamp.modified >= index_modified {
                preserved.push(path);
            } else {
                retiring.insert(path);
            }
        }
        Ok(Selected { named, retiring })
    }

    fn verify_survivors(
        packs: &[(&Path, &crate::pack::FilePack)],
        retained: &[(&Path, &crate::pack::FilePack)],
        retiring: &BTreeSet<PathBuf>,
        limits: ExpireLimits,
        cancel: &AtomicBool,
        verified_bytes: &mut u64,
    ) -> Result<(), ExpireCause> {
        for (path, pack) in packs {
            if !retiring.contains(*path) {
                continue;
            }
            pack.verify_index()
                .map_err(|error| ExpireCause::Read(Box::new(error)))?;
            pack.verify(cancel)
                .map_err(|error| ExpireCause::Read(Box::new(error)))?;
            for position in 0..pack.object_count() {
                check(cancel)?;
                let id = pack.identity_at(position);
                let mut found = None;
                for (_, survivor) in retained {
                    if let Some(position) = survivor
                        .find(id)
                        .map_err(|error| ExpireCause::Read(Box::new(error)))?
                    {
                        found = Some((*survivor, position));
                        break;
                    }
                }
                let (survivor, position) = found.ok_or(ExpireCause::Changed)?;
                let (_, size) = survivor
                    .object_header(position, limits.read)
                    .map_err(|error| ExpireCause::Read(Box::new(error)))?;
                if size as u64 > limits.max_bytes.saturating_sub(*verified_bytes) {
                    return Err(ExpireCause::Limit("survivor bytes"));
                }
                let object = survivor
                    .read(position, limits.read, cancel)
                    .map_err(|error| ExpireCause::Read(Box::new(error)))?;
                *verified_bytes = (*verified_bytes)
                    .checked_add(object.data().len() as u64)
                    .filter(|bytes| *bytes <= limits.max_bytes)
                    .ok_or(ExpireCause::Limit("survivor bytes"))?;
            }
        }
        Ok(())
    }

    impl Repository {
        pub(super) fn expire_midx_exclusive(
            &self,
            limits: ExpireLimits,
            cancel: &AtomicBool,
            report: &mut ExpireReport,
            uncertain: &mut Option<PathBuf>,
            checkpoints: (impl FnOnce(), impl FnOnce(), impl FnMut()),
        ) -> Result<(), ExpireCause> {
            let (after_proof, after_publish, mut after_remove) = checkpoints;
            let current = Repository::open(self.git_dir())
                .map_err(|error| ExpireCause::Open(Box::new(error)))?;
            if current.object_dir() != self.object_dir()
                || current.object_format() != self.object_format()
            {
                return Err(ExpireCause::Changed);
            }
            let _operation = crate::locks::operation();
            let directory = current.object_dir().join("pack");
            let lock = WriterLock::acquire(directory.join("multi-pack-index.lock"))
                .map_err(ExpireCause::Io)?;
            let index_path = directory.join("multi-pack-index");
            if directory
                .join("multi-pack-index.d/multi-pack-index-chain")
                .try_exists()
                .map_err(ExpireCause::Io)?
            {
                // Even a shadowed chain can still name packs that standalone expiry would
                // remove. Preserve all its storage until chain-aware expiry is implemented.
                return Err(ExpireCause::IncrementalChain);
            }
            if !index_path.try_exists().map_err(ExpireCause::Io)? {
                return Err(ExpireCause::Index(
                    crate::multi_pack_index::Error::MissingIndex,
                ));
            }
            let mut remaining = limits.max_entries;
            let mut inputs = observe(&directory, &mut remaining, cancel, None)?;
            let bytes = read_index(&index_path, limits.index.max_bytes)?;
            let index = MultiPackIndex::parse_with(&bytes, current.object_format(), limits.index)
                .map_err(ExpireCause::Index)?;
            if index.base_count() != 0 {
                return Err(ExpireCause::IncrementalChain);
            }
            let objects = current
                .objects_controlled(limits.packs, Default::default(), cancel)
                .map_err(|error| ExpireCause::Read(Box::new(error)))?;
            let packs = objects.primary_packs_with_paths();
            // The generic verifier may resolve a complete pack set from an alternate.
            // Expiry requires every named pack to be loaded from this object directory.
            for name in index.pack_names() {
                let path = directory.join(name).with_extension("pack");
                if !packs.iter().any(|(candidate, _)| *candidate == path) {
                    return Err(ExpireCause::Index(
                        crate::multi_pack_index::Error::MissingPack,
                    ));
                }
            }
            index.verify(&objects).map_err(ExpireCause::Index)?;
            let Selected { named, retiring } =
                select_packs(&index, &directory, &inputs, cancel, &mut report.preserved)?;
            if retiring.is_empty() {
                return Ok(());
            }
            let retained: Vec<_> = packs
                .iter()
                .filter(|(path, _)| named.contains(*path) && !retiring.contains(*path))
                .copied()
                .collect();
            verify_survivors(
                &packs,
                &retained,
                &retiring,
                limits,
                cancel,
                &mut report.verified_bytes,
            )?;
            let replacement = objects
                .encode_multi_pack_index_selected(
                    WriteOptions::default(),
                    limits.write,
                    cancel,
                    |path| named.contains(path) && !retiring.contains(path),
                )
                .map_err(ExpireCause::Write)?;
            MultiPackIndex::parse_with(
                &replacement,
                current.object_format(),
                crate::multi_pack_index::Limits {
                    max_bytes: limits.write.max_bytes,
                    max_objects: limits.write.max_objects,
                    max_packs: limits.write.max_packs,
                },
            )
            .map_err(ExpireCause::Index)?
            .verify(&objects)
            .map_err(ExpireCause::Index)?;
            after_proof();
            check(cancel)?;
            if inputs != observe(&directory, &mut remaining, cancel, None)? {
                return Err(ExpireCause::Changed);
            }
            objects
                .check_packs_unchanged(cancel)
                .map_err(|error| ExpireCause::Read(Box::new(error)))?;
            let mut temporary =
                tempfile::NamedTempFile::new_in(&directory).map_err(ExpireCause::Io)?;
            // Preserve the old index's visibility in shared repositories.
            let permissions = fs::metadata(&index_path)
                .map_err(ExpireCause::Io)?
                .permissions();
            temporary
                .as_file()
                .set_permissions(permissions)
                .map_err(ExpireCause::Io)?;
            temporary.write_all(&replacement).map_err(ExpireCause::Io)?;
            temporary.as_file().sync_all().map_err(ExpireCause::Io)?;
            if inputs != observe(&directory, &mut remaining, cancel, Some(temporary.path()))? {
                return Err(ExpireCause::Changed);
            }
            lock.check().map_err(ExpireCause::Io)?;
            check(cancel)?;
            *uncertain = Some(index_path.clone());
            temporary
                .persist(&index_path)
                .map_err(|error| ExpireCause::Io(error.error))?;
            report.index_published = true;
            fs::File::open(&directory)
                .and_then(|file| file.sync_all())
                .map_err(ExpireCause::Io)?;
            *uncertain = None;
            inputs.insert(index_path.clone(), Stamp::read(&index_path)?);
            if inputs != observe(&directory, &mut remaining, cancel, None)? {
                return Err(ExpireCause::Changed);
            }
            after_publish();
            for pack_path in &retiring {
                // Remove the index first: a crash can leave redundant unindexed bytes, but never
                // a published index pointing to a missing pack. The replacement MIDX is durable.
                for extension in ["idx", "rev", "bitmap", "pack"] {
                    let path = pack_path.with_extension(extension);
                    if !inputs.contains_key(&path) {
                        continue;
                    }
                    check(cancel)?;
                    if inputs != observe(&directory, &mut remaining, cancel, None)? {
                        return Err(ExpireCause::Changed);
                    }
                    lock.check().map_err(ExpireCause::Io)?;
                    for (_, survivor) in &retained {
                        survivor
                            .check_unchanged()
                            .map_err(|error| ExpireCause::Read(Box::new(error)))?;
                    }
                    *uncertain = Some(path.clone());
                    fs::remove_file(&path).map_err(ExpireCause::Io)?;
                    report.removed.push(path.clone());
                    inputs.remove(&path);
                    fs::File::open(&directory)
                        .and_then(|file| file.sync_all())
                        .map_err(ExpireCause::Io)?;
                    *uncertain = None;
                    after_remove();
                }
            }
            lock.release().map_err(ExpireCause::Io)?;
            Ok(())
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, UNIX_EPOCH};

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
        let id = repo
            .loose_objects()
            .write_blob(&b"survivor\n".repeat(100))
            .unwrap();
        let mut names = Vec::new();
        for compression in ["0", "9"] {
            let name = oracle::git(
                repo.git_dir(),
                &[
                    "pack-objects",
                    &format!("--compression={compression}"),
                    "--no-reuse-object",
                    "--no-reuse-delta",
                    "objects/pack/pack",
                ],
                format!("{id}\n").as_bytes(),
            );
            names.push(std::str::from_utf8(&name).unwrap().trim().to_owned());
        }
        let directory = repo.object_dir().join("pack");
        for extension in ["pack", "idx"] {
            let old = directory.join(format!("pack-{}.{extension}", names[0]));
            fs::File::open(old)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
                .unwrap();
        }
        oracle::git(
            repo.git_dir(),
            &[
                "multi-pack-index",
                "write",
                &format!("--preferred-pack=pack-{}.pack", names[1]),
            ],
            b"",
        );
        (root, repo)
    }

    #[test]
    fn unexpected_pack_directory_change_refuses_publication() {
        let (_root, repo) = fixture();
        let mut report = ExpireReport::default();
        let cause = repo
            .expire_midx_exclusive(
                ExpireLimits::default(),
                &AtomicBool::new(false),
                &mut report,
                &mut None,
                (
                    || {
                        fs::write(repo.object_dir().join("pack/new-install"), b"unexpected")
                            .unwrap();
                    },
                    || {},
                    || {},
                ),
            )
            .unwrap_err();
        assert!(matches!(cause, ExpireCause::Changed));
        assert!(!report.index_published);
        assert!(report.removed.is_empty());
    }

    #[test]
    fn incremental_chain_appearing_after_proof_refuses_publication() {
        let (_root, repo) = fixture();
        let mut report = ExpireReport::default();
        let cause = repo
            .expire_midx_exclusive(
                ExpireLimits::default(),
                &AtomicBool::new(false),
                &mut report,
                &mut None,
                (
                    || {
                        let directory = repo.object_dir().join("pack/multi-pack-index.d");
                        fs::create_dir(&directory).unwrap();
                        fs::write(directory.join("multi-pack-index-chain"), b"marker\n").unwrap();
                    },
                    || {},
                    || {},
                ),
            )
            .unwrap_err();
        assert!(matches!(cause, ExpireCause::IncrementalChain));
        assert!(!report.index_published);
        assert!(report.removed.is_empty());
    }

    #[test]
    fn cancellation_after_publication_preserves_all_packs() {
        let (_root, repo) = fixture();
        let cancel = AtomicBool::new(false);
        let mut report = ExpireReport::default();
        let cause = repo
            .expire_midx_exclusive(
                ExpireLimits::default(),
                &cancel,
                &mut report,
                &mut None,
                (|| {}, || cancel.store(true, Ordering::Relaxed), || {}),
            )
            .unwrap_err();
        assert!(matches!(cause, ExpireCause::Cancelled));
        assert!(report.index_published);
        assert!(report.removed.is_empty());
        oracle::git(repo.git_dir(), &["multi-pack-index", "verify"], b"");
        oracle::git(repo.git_dir(), &["fsck", "--full"], b"");
    }

    #[test]
    fn cancellation_after_one_unlink_reports_partial_effects() {
        let (_root, repo) = fixture();
        let cancel = AtomicBool::new(false);
        let mut report = ExpireReport::default();
        let cause = repo
            .expire_midx_exclusive(
                ExpireLimits::default(),
                &cancel,
                &mut report,
                &mut None,
                (|| {}, || {}, || cancel.store(true, Ordering::Relaxed)),
            )
            .unwrap_err();
        assert!(matches!(cause, ExpireCause::Cancelled));
        assert!(report.index_published);
        assert_eq!(report.removed.len(), 1);
        oracle::git(repo.git_dir(), &["multi-pack-index", "verify"], b"");
        oracle::git(repo.git_dir(), &["fsck", "--full"], b"");
    }
}
