use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::import::Imported;
use super::{Advertisement, FetchError, FetchLimits, KnownHistory, check_cancelled, connectivity};
use crate::{ObjectId, Repository};

/// A validated protocol response with internal delta bases and selected-tip connectivity.
///
/// Owns the original pack and generated index, but no repository files. Drop it to discard a
/// transfer. Payloads used during validation are released before this result is returned.
/// Incremental results retain IDs of known-local dependencies; installation rechecks them.
#[derive(Debug)]
pub struct ReceivedFetch {
    advertisement: Advertisement,
    wants: Vec<ObjectId>,
    pack: Vec<u8>,
    index: Vec<u8>,
    checksum: Option<ObjectId>,
    objects: usize,
    dependencies: Vec<ObjectId>,
    limits: FetchLimits,
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
            dependencies: vec![],
            limits: FetchLimits::default(),
        }
    }

    pub(super) fn without_pack(
        advertisement: Advertisement,
        wants: Vec<ObjectId>,
        known: &KnownHistory,
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, FetchError> {
        let dependencies = connectivity::validate_with_known(
            &Default::default(),
            &known.objects,
            &wants,
            limits,
            cancel,
        )?;
        let mut result = Self::empty(advertisement);
        result.dependencies = dependencies;
        result.wants = wants;
        result.limits = limits;
        Ok(result)
    }

    pub(super) fn validate_known(
        advertisement: Advertisement,
        wants: Vec<ObjectId>,
        pack: Vec<u8>,
        known: &KnownHistory,
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, FetchError> {
        let imported = Imported::read(crate::ObjectFormat::Sha1, &pack, limits, cancel)?;
        let dependencies = connectivity::validate_with_known(
            &imported.objects,
            &known.objects,
            &wants,
            limits,
            cancel,
        )?;
        check_cancelled(cancel)?;
        Ok(Self {
            advertisement,
            wants,
            pack,
            index: imported.index,
            checksum: Some(imported.checksum),
            objects: imported.objects.len(),
            dependencies,
            limits,
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

    /// Original pack size, including pack header/checksum; zero for an empty or known-only
    /// selection.
    pub fn pack_bytes(&self) -> usize {
        self.pack.len()
    }

    /// Publishes the validated SHA-1 pack and index without replacing any existing artifact.
    ///
    /// Shallow destinations (including a live shallow file created after opening) are refused.
    /// Callers must exclude concurrent depth changes for the complete installation.
    /// SHA-256 destinations return [`FetchError::Unsupported`] before filesystem mutation, even
    /// for empty transfers. Object-format negotiation and SHA-256 installation are not supported.
    ///
    /// Writes temporary files in the destination pack directory, completes and syncs their
    /// contents, then publishes the pack before its index using no-clobber persistence. The
    /// index is the visibility marker for both Git and girt. Concurrent girt readers see an
    /// older snapshot or the completed pair; reopen to discover new objects. Identical existing
    /// artifacts are reused; different bytes at either final path fail. Concurrent
    /// deletion/repacking by other tools can still make opening fail and requires retry. The
    /// object directory and ancestors must be trusted.
    ///
    /// Before creating artifacts, reopens the destination under `snapshot_limits` and
    /// identity-checks every local object used for connectivity, under the receive call's local
    /// byte/count and per-read bounds. This also applies to a known-only result with no pack.
    /// Objects must remain available through subsequent reference publication; GC coordination
    /// remains the caller's responsibility. Installing into a different repository works only if
    /// its verified local objects satisfy those same dependencies.
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
        snapshot_limits: crate::PackLimits,
        cancel: &AtomicBool,
    ) -> Result<FetchInstalled, FetchError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "fetch.install",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
            objects = self.objects,
            pack_bytes = self.pack.len(),
        );

        let operation = || {
            if repository.object_format() != crate::ObjectFormat::Sha1 {
                return Err(FetchError::Unsupported("SHA-256 pack installation"));
            }
            check_cancelled(cancel)?;
            if !repository.shallow_roots().is_empty()
                || repository.common_dir().join("shallow").try_exists()?
            {
                return Err(FetchError::Unsupported("shallow pack installation"));
            }
            let result = FetchInstalled {
                checksum: self.checksum,
                objects: self.objects,
            };
            if !self.dependencies.is_empty() {
                let objects = repository
                    .objects(snapshot_limits)
                    .map_err(FetchError::Destination)?;
                let mut bytes = self.limits.max_known_bytes;
                for &id in &self.dependencies {
                    check_cancelled(cancel)?;
                    let mut read = self.limits.known_read;
                    read.max_object_bytes = read.max_object_bytes.min(bytes);
                    let object = objects
                        .read(id, read)
                        .map_err(|source| FetchError::LocalRead { id, source })?
                        .ok_or(FetchError::Missing(id))?;
                    bytes = bytes
                        .checked_sub(object.data().len())
                        .ok_or(FetchError::Limit("known bytes"))?;
                }
            }
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
            #[cfg(feature = "tracing")]
            span.record("effects", "pack_visible");
            check_cancelled(cancel)?;
            publish(index, &basename.with_extension("idx"), &self.index, cancel)?;
            #[cfg(feature = "tracing")]
            span.record("effects", "pack_and_index_visible");
            Ok(result)
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::fetch);

        result
    }
}

/// Object publication completed; it says nothing about reference changes or reflogs.
#[derive(Debug, Clone, Copy)]
pub struct FetchInstalled {
    /// Installed/reused pack checksum, or `None` when no pack was needed.
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

#[cfg(test)]
mod shallow_tests {
    use super::*;

    #[test]
    fn stale_handle_cannot_install_into_new_shallow_metadata() {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("repo"),
            crate::InitKind::Bare,
        )
        .unwrap();
        std::fs::write(repo.common_dir().join("shallow"), b"invalid\n").unwrap();
        let received = ReceivedFetch::empty(Advertisement {
            refs: Vec::new(),
            capabilities: Vec::new(),
        });
        assert!(matches!(
            received.install(&repo, crate::PackLimits::default(), &AtomicBool::new(false)),
            Err(FetchError::Unsupported("shallow pack installation"))
        ));
        assert_eq!(
            std::fs::read(repo.common_dir().join("shallow")).unwrap(),
            b"invalid\n"
        );
        assert_eq!(
            std::fs::read_dir(repo.object_dir().join("pack"))
                .unwrap()
                .count(),
            0
        );
    }
}
