//! Sidecars are complete before the bitmap/MIDX visibility marker is atomically replaced.
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::{WriteLimits, WriteOptions, Written};
use crate::retention::MaintenanceIsolation;
use crate::{ObjectId, Repository};

/// A complete optional bitmap is visible for the reported inventory.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Published {
    /// Pack or MIDX checksum associated with these sidecars.
    pub anchor: ObjectId,
    /// Inventory object count.
    pub objects: usize,
    /// Selected commit count.
    pub commits: usize,
    /// Number of associated packs.
    pub packs: usize,
}

/// Bitmap publication failed, retaining whether its final visibility marker was replaced.
#[derive(Debug, thiserror::Error)]
#[error("cannot {operation} bitmap index")]
#[non_exhaustive]
pub struct InstallError {
    /// Bitmap replacement completed (pack) or MIDX replacement completed (multi-pack).
    /// Inspect storage after a later directory-sync or lock-release error before retrying.
    pub published: bool,
    /// Publication phase that failed.
    pub operation: &'static str,
    /// Typed underlying encoding, storage, isolation or locking failure.
    #[source]
    pub source: crate::Error,
}

impl Repository {
    /// Publishes bitmap/reverse-index sidecars for an existing complete primary pack.
    ///
    /// Uses default bounds; see [`Self::write_pack_bitmap_with`] for publication guarantees.
    ///
    /// ```no_run
    /// use girt::Repository;
    /// use girt::retention::MaintenanceIsolation;
    /// fn add<I: MaintenanceIsolation>(
    ///     repo: &Repository,
    ///     isolation: &mut I,
    /// ) -> Result<usize, Box<dyn std::error::Error>> {
    ///     let objects = repo.objects()?;
    ///     let pack = objects.packs().next().ok_or("no primary pack")?;
    ///     let written = repo.write_pack_bitmap(isolation, pack.checksum(), Default::default())?;
    ///     Ok(written.commits)
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`InstallError`] for exclusion, encoding, lock, generation or publication failures.
    pub fn write_pack_bitmap<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        pack: ObjectId,
        options: WriteOptions,
    ) -> Result<Published, InstallError> {
        self.write_pack_bitmap_with(
            isolation,
            pack,
            options,
            WriteLimits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Publishes a reverse index before atomically replacing the associated pack bitmap.
    ///
    /// The caller's guard must exclude pack mutation/retirement and sidecar writers through return.
    /// Source indexes and graph contents are verified and generations rechecked before publication.
    /// A separate per-pack bitmap lock is registered for cleanup. Complete temporary files are
    /// synchronized first; on Unix the containing directory is synchronized after replacement.
    /// Other platforms make no power-loss name-durability promise. Identical existing artifacts
    /// are retained. Filesystem sharing/replacement restrictions can refuse replacement.
    ///
    /// Failure before bitmap replacement preserves an old bitmap. A matching reverse index may
    /// already have been published; its ordering is fixed by the same pack checksum. No pack or
    /// index files are changed. Cancellation is checked before visibility changes, but cannot undo
    /// a completed replacement. See [`InstallError::published`] for failures afterwards.
    ///
    /// # Errors
    ///
    /// Returns [`InstallError`] with the concrete cause and final-marker publication state.
    pub fn write_pack_bitmap_with<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        pack: ObjectId,
        options: WriteOptions,
        limits: WriteLimits,
        cancel: &AtomicBool,
    ) -> Result<Published, InstallError> {
        install(self, isolation, Target::Pack(pack), options, limits, cancel)
    }

    /// Publishes a MIDX together with its checksum-associated bitmap and reverse index.
    ///
    /// Uses default bounds; see [`Self::write_multi_pack_bitmap_with`] for publication guarantees.
    ///
    /// # Errors
    ///
    /// Returns [`InstallError`] for unavailable exclusion, invalid inventories or publication.
    pub fn write_multi_pack_bitmap<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        midx: crate::multi_pack_index::WriteOptions,
        options: WriteOptions,
    ) -> Result<Published, InstallError> {
        self.write_multi_pack_bitmap_with(
            isolation,
            midx,
            options,
            WriteLimits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Publishes complete MIDX sidecars before atomically replacing the monolithic MIDX.
    ///
    /// Uses the same exclusion, source-validation and durability contract as
    /// [`Self::write_pack_bitmap_with`], and shares `multi-pack-index.lock` with plain MIDX
    /// writing. Readers of the old MIDX ignore sidecars named for a different checksum. Before
    /// MIDX replacement, failures preserve that index but can leave unused checksum-named
    /// sidecars. Old sidecars and incremental chains remain untouched. The final standalone
    /// MIDX takes precedence over an incremental chain. No references or packs are changed.
    ///
    /// # Errors
    ///
    /// Returns [`InstallError`] retaining whether MIDX replacement completed.
    pub fn write_multi_pack_bitmap_with<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        midx: crate::multi_pack_index::WriteOptions,
        options: WriteOptions,
        limits: WriteLimits,
        cancel: &AtomicBool,
    ) -> Result<Published, InstallError> {
        install(
            self,
            isolation,
            Target::Multi(midx),
            options,
            limits,
            cancel,
        )
    }
}

enum Target {
    Pack(ObjectId),
    Multi(crate::multi_pack_index::WriteOptions),
}
fn install<I: MaintenanceIsolation>(
    repository: &Repository,
    isolation: &mut I,
    target: Target,
    options: WriteOptions,
    limits: WriteLimits,
    cancel: &AtomicBool,
) -> Result<Published, InstallError> {
    super::write::check(cancel).map_err(|source| InstallError {
        published: false,
        operation: "check cancellation",
        source: source.into(),
    })?;
    let _guard = isolation
        .acquire(repository)
        .map_err(|source| InstallError {
            published: false,
            operation: "acquire maintenance exclusion",
            source: source.into(),
        })?;
    install_held(repository, target, options, limits, cancel, None)
}

// Additive repack may race with external retirement under its existing caller contract. All bytes
// remain checksum-associated; a concurrent unlink can leave an ignored orphan sidecar, never a
// bitmap attached to another pack or authority to remove storage.
pub(crate) fn write_repack_bitmap(
    repository: &Repository,
    pack: ObjectId,
    limits: WriteLimits,
    cancel: &AtomicBool,
) -> Result<Published, InstallError> {
    install_held(
        repository,
        Target::Pack(pack),
        WriteOptions::default(),
        limits,
        cancel,
        None,
    )
}

pub(crate) fn write_repack_bitmap_from_verified_graph(
    repository: &Repository,
    pack: ObjectId,
    verified: &crate::retention::VerifiedGraph,
    limits: WriteLimits,
    cancel: &AtomicBool,
) -> Result<Published, InstallError> {
    install_held(
        repository,
        Target::Pack(pack),
        WriteOptions::default(),
        limits,
        cancel,
        Some(verified),
    )
}

fn install_held(
    repository: &Repository,
    target: Target,
    options: WriteOptions,
    limits: WriteLimits,
    cancel: &AtomicBool,
    verified_graph: Option<&crate::retention::VerifiedGraph>,
) -> Result<Published, InstallError> {
    let mut operation = "acquire writer lock";
    let mut published = false;
    let result = (|| -> crate::Result<Published> {
        super::write::check(cancel)?;
        let _operation = crate::locks::operation();
        let directory = repository.object_dir().join("pack");
        let lock_path = directory.join(match target {
            Target::Pack(id) => format!("pack-{id}.bitmap.lock"),
            Target::Multi(_) => "multi-pack-index.lock".to_owned(),
        });
        operation = "acquire writer lock";
        let lock = crate::multi_pack_index::WriterLock::acquire(lock_path)?;
        operation = "open source inventory";
        let objects =
            repository.objects_controlled(Default::default(), Default::default(), cancel)?;
        operation = "encode graph artifacts";
        let (index, written, packs, basename) = match target {
            Target::Pack(id) => {
                let written = match verified_graph {
                    Some(verified) => objects
                        .encode_pack_bitmap_from_verified_graph(id, verified, limits, cancel)?,
                    None => objects.encode_pack_bitmap(id, options, limits, cancel)?,
                };
                (None, written, 1, directory.join(format!("pack-{id}")))
            }
            Target::Multi(midx) => {
                let written = objects.encode_multi_pack_bitmap(midx, options, limits, cancel)?;
                let packs = objects.primary_packs_with_paths().len();
                let basename =
                    directory.join(format!("multi-pack-index-{}", written.bitmap.anchor));
                (Some(written.index), written.bitmap, packs, basename)
            }
        };
        let report = report(&written, packs);
        operation = "synchronize private artifacts";
        let reverse = private(&directory, &written.reverse_index)?;
        let bitmap = private(&directory, &written.bitmap)?;
        let index = index
            .as_deref()
            .map(|bytes| private(&directory, bytes))
            .transpose()?;
        operation = "revalidate source inventory";
        for (_, pack) in objects.primary_packs_with_paths() {
            super::write::check(cancel)?;
            pack.check_unchanged()?;
        }
        lock.check()?;
        super::write::check(cancel)?;
        operation = "publish reverse index";
        publish(reverse, &basename.with_extension("rev"), cancel)?;
        operation = "publish bitmap";
        publish(bitmap, &basename.with_extension("bitmap"), cancel)?;
        if let Some(index) = index {
            operation = "publish multi-pack index";
            publish(index, &directory.join("multi-pack-index"), cancel)?;
        }
        published = true;
        #[cfg(unix)]
        {
            operation = "synchronize pack directory";
            File::open(&directory)?.sync_all()?;
        }
        operation = "release writer lock";
        lock.release()?;
        Ok(report)
    })();
    result.map_err(|source| InstallError {
        published,
        operation,
        source,
    })
}

fn report(written: &Written, packs: usize) -> Published {
    Published {
        anchor: written.anchor,
        objects: written.objects,
        commits: written.commits,
        packs,
    }
}
fn private(directory: &Path, bytes: &[u8]) -> io::Result<tempfile::NamedTempFile> {
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    Ok(file)
}
fn publish(file: tempfile::NamedTempFile, path: &Path, cancel: &AtomicBool) -> crate::Result<()> {
    super::write::check(cancel)?;
    // Avoid replacing identical immutable artifacts, including a Git-written read-only .rev.
    if let Ok(existing) = File::open(path) {
        let length = file.as_file().metadata()?.len();
        if existing.metadata()?.len() == length {
            let mut old = Vec::new();
            existing
                .take(length.saturating_add(1))
                .read_to_end(&mut old)?;
            let mut new = Vec::new();
            file.reopen()?.read_to_end(&mut new)?;
            if old == new {
                return Ok(());
            }
        }
    }
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}
