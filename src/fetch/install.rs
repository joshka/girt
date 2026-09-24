use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::{Advertisement, FetchError, FetchLimits, check_cancelled, connectivity};
use crate::pack::Imported;
use crate::{ObjectId, Repository};

/// A complete protocol response with a verified self-contained pack and selected-tip connectivity.
///
/// Owns the original pack and generated index, but no repository files. Drop it to discard a
/// transfer. Payloads used during validation are released before this result is returned.
#[derive(Debug)]
pub struct ReceivedFetch {
    advertisement: Advertisement,
    wants: Vec<ObjectId>,
    pack: Vec<u8>,
    index: Vec<u8>,
    checksum: Option<ObjectId>,
    objects: usize,
}

impl ReceivedFetch {
    pub(super) fn empty(advertisement: Advertisement) -> Self {
        Self {
            advertisement,
            wants: vec![],
            pack: vec![],
            index: vec![],
            checksum: None,
            objects: 0,
        }
    }

    pub(super) fn validate(
        advertisement: Advertisement,
        wants: Vec<ObjectId>,
        pack: Vec<u8>,
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, FetchError> {
        let imported = Imported::read(&pack, limits, cancel)?;
        connectivity::validate(&imported.objects, &wants, limits, cancel)?;
        check_cancelled(cancel)?;
        Ok(Self {
            advertisement,
            wants,
            pack,
            index: imported.index,
            checksum: Some(imported.checksum),
            objects: imported.objects.len(),
        })
    }

    /// Server advertisement used to select the transfer; it may already be stale at the server.
    pub fn advertisement(&self) -> &Advertisement {
        &self.advertisement
    }

    /// Deduplicated selected tip IDs, in caller order. These are not reference-update instructions.
    pub fn wants(&self) -> &[ObjectId] {
        &self.wants
    }

    /// Number of verified received objects, including any unrequested server extras.
    pub fn object_count(&self) -> usize {
        self.objects
    }

    /// Original received pack size, including framing and checksum; zero for an empty selection.
    pub fn pack_bytes(&self) -> usize {
        self.pack.len()
    }

    /// Publishes the validated pack and index without replacing any existing artifact.
    ///
    /// Writes temporary files in the destination pack directory, completes and syncs their
    /// contents, then publishes the pack before its index using no-clobber persistence. The
    /// index is the visibility marker for both Git and girt. Concurrent girt readers see an
    /// older snapshot or the completed pair; reopen to discover new objects. Identical existing
    /// artifacts are reused; different bytes at either final path fail. Concurrent
    /// deletion/repacking by other tools can still make opening fail and requires retry. The
    /// object directory and ancestors must be trusted.
    ///
    /// No references or reflogs change. After success, callers can reopen objects and perform
    /// individual conditional updates through [`crate::refs::References::update_without_reflog`],
    /// explicitly with no reflog. Multiple updates are separate operations: a later failure
    /// leaves earlier updates intact. Callers own those outcomes and any retry policy.
    /// Concurrent ref changes do not affect this operation. Repository readers' limits must
    /// accommodate the newly installed pack.
    ///
    /// # Errors
    ///
    /// I/O, cancellation, and conflicting existing artifacts fail without overwriting or removing
    /// any existing object. Failure after pack publication can leave an unindexed pack; it is
    /// ignored by readers and a retry of this result can finish publication. Temporary files
    /// are cleaned up on ordinary errors; process crashes can leave temporary files. Directory
    /// entries are not fsynced, so success does not promise survival across power loss. Callers
    /// must coordinate with pruning/GC; no keep-file or garbage-collection exclusion is
    /// provided.
    pub fn install(
        &self,
        repository: &Repository,
        cancel: &AtomicBool,
    ) -> Result<FetchInstalled, FetchError> {
        check_cancelled(cancel)?;
        let result = FetchInstalled {
            checksum: self.checksum,
            objects: self.objects,
        };
        let Some(checksum) = self.checksum else {
            return Ok(result);
        };
        let directory = repository.object_dir().join("pack");
        fs::create_dir_all(&directory)?;
        let mut pack = tempfile::NamedTempFile::new_in(&directory)?;
        let mut index = tempfile::NamedTempFile::new_in(&directory)?;
        pack.write_all(&self.pack)?;
        pack.as_file().sync_all()?;
        index.write_all(&self.index)?;
        index.as_file().sync_all()?;
        let basename = directory.join(format!("pack-{checksum}"));
        check_cancelled(cancel)?;
        publish(pack, &basename.with_extension("pack"), &self.pack, cancel)?;
        check_cancelled(cancel)?;
        publish(index, &basename.with_extension("idx"), &self.index, cancel)?;
        Ok(result)
    }
}

/// Object publication completed; it says nothing about reference changes or reflogs.
#[derive(Debug, Clone, Copy)]
pub struct FetchInstalled {
    /// Installed/reused pack checksum, or `None` for an empty selection.
    pub checksum: Option<ObjectId>,
    /// Verified object count (not the count of objects new to this repository).
    pub objects: usize,
}

fn publish(
    temporary: tempfile::NamedTempFile,
    path: &Path,
    expected: &[u8],
    cancel: &AtomicBool,
) -> Result<(), FetchError> {
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut file = File::open(path)?;
            if file.metadata()?.len() != expected.len() as u64 {
                return Err(FetchError::Existing(path.into()));
            }
            let mut position = 0;
            let mut buffer = [0; 8192];
            loop {
                check_cancelled(cancel)?;
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                if expected.get(position..position + count) != Some(&buffer[..count]) {
                    return Err(FetchError::Existing(path.into()));
                }
                position += count;
            }
            if position != expected.len() {
                return Err(FetchError::Existing(path.into()));
            }
            Ok(())
        }
        Err(error) => Err(error.error.into()),
    }
}
