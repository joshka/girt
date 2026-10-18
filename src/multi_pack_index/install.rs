//! Publish a complete optional index while preserving the old file until replacement.
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use super::{MultiPackIndex, WriteLimits, WriteOptions};
use crate::retention::MaintenanceIsolation;
use crate::{ObjectId, Repository};

/// A standalone MIDX successfully published in the primary object directory.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct IndexWritten {
    /// Checksum identifying the encoded index and its optional bitmap sidecars.
    pub checksum: ObjectId,
    /// Number of unique objects represented.
    pub objects: usize,
    /// Number of named primary-store packs.
    pub packs: usize,
}

/// MIDX installation failure, retaining whether the new index became visible.
#[derive(Debug, thiserror::Error)]
#[error("cannot {operation} multi-pack index")]
#[non_exhaustive]
pub struct InstallError {
    /// Whether atomic replacement completed before the failure. Inspect storage before retrying.
    pub published: bool,
    /// Publication phase that failed.
    pub operation: &'static str,
    /// Filesystem, isolation, input or encoding failure.
    #[source]
    pub source: crate::Error,
}

impl Repository {
    /// Writes a primary-store MIDX under caller-owned maintenance exclusion and default bounds.
    ///
    /// Equivalent to [`Self::write_multi_pack_index_with`] with default limits and no cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`InstallError`] for unavailable exclusion, a held writer lock or invalid inputs.
    pub fn write_multi_pack_index<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        options: WriteOptions,
    ) -> Result<IndexWritten, InstallError> {
        self.write_multi_pack_index_with(
            isolation,
            options,
            WriteLimits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Atomically publishes a monolithic MIDX built from one verified primary-store snapshot.
    ///
    /// The isolation guard must exclude other pack writers and retirement from acquisition through
    /// return, as required by [`MaintenanceIsolation`]. A separate MIDX writer lock coordinates
    /// auxiliary-index publishers. See [`MaintenanceIsolation`] for a caller-owned guard example.
    /// Existing packs, incremental layers and checksum-named bitmap
    /// sidecars remain untouched; the standalone file takes precedence over incremental chains.
    /// A bitmap with another checksum cannot attach to the new index.
    ///
    /// Source indexes verify before encoding, and pinned generations are checked again before
    /// replacement. A complete private file is synchronized before atomic replacement. On Unix,
    /// the containing directory is synchronized afterwards; other platforms do not promise name
    /// durability across power loss. This operation does not change references or create bitmaps.
    /// Snapshot opening uses [`crate::PackLimits::default`]; use the object-store encoder for
    /// larger caller-managed snapshots. Hostile directory replacement is outside the exclusion
    /// contract.
    ///
    /// # Errors
    ///
    /// Before replacement, errors preserve an existing MIDX. Afterwards, directory synchronization
    /// or lock-release failure returns `published=true`; inspect current storage before retrying.
    /// Filesystem sharing or replacement restrictions can refuse publication with
    /// `published=false`. A pre-existing lock is never stolen; establish that no writer owns it
    /// before removing it. Cancellation is checked before source opening, during encoding and
    /// before replacement.
    pub fn write_multi_pack_index_with<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        options: WriteOptions,
        limits: WriteLimits,
        cancel: &AtomicBool,
    ) -> Result<IndexWritten, InstallError> {
        self.write_multi_pack_index_controlled(
            isolation,
            options,
            limits,
            crate::PackLimits::default(),
            cancel,
        )
    }

    pub(super) fn write_multi_pack_index_controlled<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        options: WriteOptions,
        limits: WriteLimits,
        packs: crate::PackLimits,
        cancel: &AtomicBool,
    ) -> Result<IndexWritten, InstallError> {
        let mut operation = "acquire maintenance exclusion";
        let mut published = false;
        let result = (|| -> crate::Result<IndexWritten> {
            super::write::check(cancel)?;
            let _exclusion = isolation.acquire(self)?;
            let _operation = crate::locks::operation();
            let directory = self.object_dir().join("pack");
            match fs::metadata(&directory) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Err(super::WriteError::NoObjects.into());
                }
                Err(error) => return Err(error.into()),
            }
            operation = "acquire writer lock";
            let lock_path = directory.join("multi-pack-index.lock");
            let lock = WriterLock::acquire(lock_path.clone()).map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    crate::Error::from(super::WriteError::Locked(lock_path))
                } else {
                    crate::Error::from(error)
                }
            })?;
            operation = "open primary snapshot";
            let objects =
                self.objects_controlled(packs, crate::AlternateLimits::default(), cancel)?;
            operation = "encode index";
            let bytes = objects.encode_multi_pack_index_with(options, limits, cancel)?;
            let parsed = MultiPackIndex::parse_with(
                &bytes,
                self.object_format(),
                super::Limits {
                    max_bytes: limits.max_bytes,
                    max_objects: limits.max_objects,
                    max_packs: limits.max_packs,
                },
            )?;
            let checksum = ObjectId::from_bytes(
                self.object_format(),
                &bytes[bytes.len() - self.object_format().digest_len()..],
            )?;
            let report = IndexWritten {
                checksum,
                objects: parsed.len(),
                packs: parsed.pack_names().len(),
            };
            operation = "write private index";
            let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
            temporary.write_all(&bytes)?;
            temporary.as_file().sync_all()?;
            // Encoding checks its own returned artifact; installation checks again after the
            // private file has been written and synchronized, immediately before publication.
            operation = "revalidate source packs";
            for (_, pack) in objects.primary_packs_with_paths() {
                pack.check_unchanged()?;
            }
            lock.registration.check_path()?;
            super::write::check(cancel)?;
            operation = "publish index";
            temporary
                .persist(directory.join("multi-pack-index"))
                .map_err(|error| error.error)?;
            published = true;
            #[cfg(unix)]
            {
                operation = "synchronize pack directory";
                fs::File::open(&directory)?.sync_all()?;
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
}

pub(crate) struct WriterLock {
    path: PathBuf,
    file: Option<crate::locks::SharedFile>,
    registration: crate::locks::Guard,
}

impl WriterLock {
    pub(crate) fn acquire(path: PathBuf) -> io::Result<Self> {
        let file =
            crate::locks::SharedFile::from(crate::locks::unreplaceable_lock_options().open(&path)?);
        let identity = crate::locks::capture_identity(&path, &file)?;
        let registration = crate::locks::Guard::new(&path, &file, identity);
        Ok(Self {
            path,
            file: Some(file),
            registration,
        })
    }

    pub(crate) fn check(&self) -> io::Result<()> {
        self.registration.check_path()
    }

    pub(crate) fn release(mut self) -> io::Result<()> {
        self.registration.check_path()?;
        drop(self.file.take());
        fs::remove_file(&self.path)
    }
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        if self.file.is_some() && self.registration.check_path().is_ok() {
            drop(self.file.take());
            let _ = fs::remove_file(&self.path);
        }
    }
}
