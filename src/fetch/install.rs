use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::import::Imported;
use super::known::KnownObjects as _;
use super::progress::ValidationObserver;
use super::{Advertisement, FetchError, FetchLimits, KnownHistory, check_cancelled, connectivity};
use crate::{ObjectId, Repository};

/// A validated protocol response with resolved delta bases and selected-tip connectivity.
///
/// Owns the original pack and generated index, but no repository files. Drop it to discard a
/// transfer. Payloads used during validation are released before this result is returned.
/// Incremental results retain IDs of known-local dependencies; installation rechecks them.
#[derive(Debug)]
pub struct ReceivedFetch {
    format: crate::ObjectFormat,
    advertisement: Advertisement,
    wants: Vec<ObjectId>,
    pack: Vec<u8>,
    index: Vec<u8>,
    checksum: Option<ObjectId>,
    objects: usize,
    dependencies: Vec<ObjectId>,
    shallow: Vec<ObjectId>,
    limits: FetchLimits,
}

pub(crate) struct NativeContents {
    pub format: crate::ObjectFormat,
    pub pack: Vec<u8>,
    pub index: Vec<u8>,
    pub checksum: Option<ObjectId>,
    pub objects: usize,
    pub dependencies: Vec<ObjectId>,
    pub shallow: Vec<ObjectId>,
    pub limits: FetchLimits,
}

impl ReceivedFetch {
    pub(super) fn empty(advertisement: Advertisement) -> Self {
        Self {
            format: crate::ObjectFormat::Sha1,
            advertisement,
            wants: vec![],
            pack: vec![],
            index: vec![],
            checksum: None,
            objects: 0,
            dependencies: vec![],
            shallow: vec![],
            limits: FetchLimits::default(),
        }
    }

    pub(crate) fn native(
        advertisement: Advertisement,
        wants: Vec<ObjectId>,
        contents: NativeContents,
    ) -> Self {
        Self {
            format: contents.format,
            advertisement,
            wants,
            pack: contents.pack,
            index: contents.index,
            checksum: contents.checksum,
            objects: contents.objects,
            dependencies: contents.dependencies,
            shallow: contents.shallow,
            limits: contents.limits,
        }
    }

    pub(super) fn without_pack(
        advertisement: Advertisement,
        wants: Vec<ObjectId>,
        known: &KnownHistory,
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, FetchError> {
        let dependencies = connectivity::validate_with_boundaries(
            &Default::default(),
            known,
            &wants,
            &known.shallow,
            limits,
            cancel,
        )?;
        let format = advertisement.object_format()?;
        let mut result = Self::empty(advertisement);
        result.format = format;
        result.dependencies = dependencies;
        result.shallow = known.shallow.clone();
        result.wants = wants;
        result.limits = limits;
        Ok(result)
    }

    pub(super) fn validate_known(
        advertisement: Advertisement,
        wants: Vec<ObjectId>,
        pack: Vec<u8>,
        known: &KnownHistory,
        shallow: Vec<ObjectId>,
        options: super::FetchOptions,
        observer: &mut ValidationObserver<'_>,
    ) -> Result<Self, FetchError> {
        let super::FetchOptions { limits, depth } = options;
        let cancel = observer.cancel;
        let format = advertisement.object_format()?;
        let mut imported = Imported::read_observed(format, &pack, known, limits, observer)?;
        let mut dependencies = connectivity::validate_with_boundaries(
            &imported.objects,
            known,
            &wants,
            &shallow,
            limits,
            cancel,
        )?;
        let mut objects = imported.objects.len();
        if depth.is_some()
            && !dependencies.is_empty()
            && !imported.objects.is_empty()
            && !known.trusted_complete()
        {
            objects += dependencies.len();
            materialize_dependencies(&mut imported, known, &dependencies, limits, cancel)?;
            dependencies.clear();
        }
        check_cancelled(cancel)?;
        let received = Self {
            format,
            advertisement,
            wants,
            pack: if objects == 0 {
                Vec::new()
            } else {
                imported.complete_pack.unwrap_or(pack)
            },
            index: imported.index,
            checksum: (objects != 0).then_some(imported.checksum),
            objects,
            dependencies,
            shallow,
            limits,
        };
        observer.complete();
        Ok(received)
    }

    /// Server advertisement used to select the transfer; it may already be stale at the server.
    pub fn advertisement(&self) -> &Advertisement {
        &self.advertisement
    }

    /// Deduplicated selected tip IDs, in caller order. These are not reference-update instructions.
    pub fn wants(&self) -> &[ObjectId] {
        &self.wants
    }

    /// Number of verified pack objects, including materialized local dependencies and server
    /// extras.
    pub fn object_count(&self) -> usize {
        self.objects
    }

    /// Installable pack size, including header/checksum; zero for an empty or known-only
    /// selection. A received thin pack is rewritten, so this can differ from wire size.
    pub fn pack_bytes(&self) -> usize {
        self.pack.len()
    }

    /// Resulting shallow commit boundaries reported by upload-pack. These require coordinated
    /// metadata publication with the pack before a depth-limited result can be installed.
    pub fn shallow_roots(&self) -> &[ObjectId] {
        &self.shallow
    }

    /// Publishes the validated pack and index without replacing any existing artifact.
    ///
    /// Direct installation refuses shallow results and destinations. Use [`super::FetchRequest`]
    /// to coordinate object and boundary publication. Callers must exclude concurrent depth
    /// changes for the complete installation.
    /// The destination format must match the received pack. Complete SHA-1 and SHA-256 wire and
    /// native-local transfers may be installed.
    ///
    /// Writes temporary files in the destination pack directory, completes and syncs their
    /// contents, then publishes the pack before its index using no-clobber persistence. The
    /// index is the visibility marker for both Git and girt. Concurrent girt readers see an
    /// older view or the completed pair; call [`crate::Objects::refresh`] to discover new objects.
    /// Identical existing artifacts are reused; different bytes at either final path fail.
    /// Concurrent deletion/repacking by other tools can still make opening fail and requires
    /// retry. The object directory and ancestors must be trusted.
    ///
    /// Before creating artifacts, reopens the destination under `snapshot_limits` and
    /// identity-checks every local object used for connectivity, under the receive call's local
    /// byte/count and per-read bounds. This also applies to a known-only result with no pack.
    /// Objects must remain available through subsequent reference publication; GC coordination
    /// remains the caller's responsibility. Installing into a different repository works only if
    /// its verified local objects satisfy those same dependencies.
    ///
    /// No references or reflogs change. After success, callers can refresh objects and perform
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
        self.install_bytes(
            repository,
            snapshot_limits,
            cancel,
            &self.pack,
            &self.index,
            false,
        )
    }

    /// Installs a complete transfer while retaining its pack against Git repacking.
    ///
    /// Creates an exclusively owned Git `.keep` file before publishing either pack artifact.
    /// The returned retention must live through the caller's reference publication; explicitly
    /// release it afterwards. Dropping it, including on an error, leaves the marker on disk for
    /// recovery. No references are changed. See [`super::FetchRetention`] for the release contract.
    ///
    /// Only complete, nonempty transfers without known-local dependencies are supported. Use
    /// [`super::receive_local`] with empty known history to obtain one. An existing pack or index
    /// is refused: a collector may already have selected that old artifact for deletion before
    /// the marker was created. This operation does not protect shallow metadata, HEAD, worktrees,
    /// or objects outside the received pack. Use [`super::FetchReady::install_retained`] for a
    /// coordinated shallow transfer. The ordinary installation's trusted-path and
    /// durability requirements still apply.
    ///
    /// # Errors
    ///
    /// Returns unsupported input, format, interruption, existing artifact/marker or I/O errors.
    /// Once a marker is acquired, errors return its retention handle as well as the installation
    /// cause. Partial pack artifacts remain protected for inspection. Release the returned handle
    /// only when abandoning that installation or after establishing persistent reference roots.
    pub fn install_retained(
        &self,
        repository: &Repository,
        snapshot_limits: crate::PackLimits,
        cancel: &AtomicBool,
    ) -> Result<(FetchInstalled, super::FetchRetention), super::RetainedFetchError> {
        let checksum = self
            .retention_checksum(repository, cancel)
            .map_err(super::RetainedFetchError::before_retention)?;
        let reject_shallow = || {
            if !self.shallow.is_empty()
                || !repository.shallow_roots().is_empty()
                || repository.common_dir().join("shallow").try_exists()?
            {
                return Err(FetchError::Unsupported("retained shallow installation"));
            }
            Ok(())
        };
        reject_shallow().map_err(super::RetainedFetchError::before_retention)?;
        super::retention::install(self, repository, checksum, snapshot_limits, cancel)
    }

    pub(super) fn retention_checksum(
        &self,
        repository: &Repository,
        cancel: &AtomicBool,
    ) -> Result<ObjectId, FetchError> {
        check_cancelled(cancel)?;
        if repository.object_format() != self.format {
            return Err(FetchError::FormatMismatch {
                remote: self.format,
                local: repository.object_format(),
            });
        }
        if !self.dependencies.is_empty() {
            return Err(FetchError::Unsupported(
                "retention requires complete history",
            ));
        }
        self.checksum
            .ok_or(FetchError::Unsupported("retention requires a pack"))
    }

    pub(super) fn install_for_workflow(
        &self,
        repository: &Repository,
        snapshot_limits: crate::PackLimits,
        cancel: &AtomicBool,
        shallow_locked: bool,
    ) -> Result<FetchInstalled, FetchError> {
        self.install_bytes(
            repository,
            snapshot_limits,
            cancel,
            &self.pack,
            &self.index,
            shallow_locked,
        )
    }

    pub(crate) fn install_bytes(
        &self,
        repository: &Repository,
        snapshot_limits: crate::PackLimits,
        cancel: &AtomicBool,
        pack_bytes: &[u8],
        index_bytes: &[u8],
        allow_shallow: bool,
    ) -> Result<FetchInstalled, FetchError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "fetch.install",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
            objects = self.objects,
            pack_bytes = pack_bytes.len(),
        );

        let operation = || {
            if repository.object_format() != self.format {
                return Err(FetchError::FormatMismatch {
                    remote: self.format,
                    local: repository.object_format(),
                });
            }
            check_cancelled(cancel)?;
            if !allow_shallow && !self.shallow.is_empty() {
                return Err(FetchError::Unsupported(
                    "shallow installation requires boundary publication",
                ));
            }
            if !allow_shallow
                && (!repository.shallow_roots().is_empty()
                    || repository.common_dir().join("shallow").try_exists()?)
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
            pack.write_all(pack_bytes)?;
            pack.as_file().sync_all()?;
            index.write_all(index_bytes)?;
            index.as_file().sync_all()?;
            let basename = directory.join(format!("pack-{checksum}"));
            check_cancelled(cancel)?;
            publish(pack, &basename.with_extension("pack"), pack_bytes, cancel)?;
            #[cfg(feature = "tracing")]
            span.record("effects", "pack_visible");
            check_cancelled(cancel)?;
            publish(index, &basename.with_extension("idx"), index_bytes, cancel)?;
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

/// Include the verified snapshot's reachable dependencies in the pack retained by publication.
fn materialize_dependencies(
    imported: &mut Imported,
    known: &KnownHistory,
    dependencies: &[ObjectId],
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<(), FetchError> {
    let format = imported.checksum.format();
    // The verified snapshot owns these bytes through validation. Copy each reachable
    // dependency into the new pack so retention protects its complete shallow closure.
    let mut inputs: Vec<_> = imported
        .objects
        .iter()
        .map(|(&id, object)| crate::PackObject {
            id,
            kind: object.kind(),
            data: object.data(),
        })
        .collect();
    inputs.extend(dependencies.iter().filter_map(|&id| {
        let object = known.objects.get(&id)?;
        Some(crate::PackObject {
            id,
            kind: object.kind(),
            data: object.data(),
        })
    }));
    inputs.sort_unstable_by_key(|object| object.id);
    let mut complete_pack = Vec::new();
    let mut index = Vec::new();
    let written = crate::pack::write_controlled(
        format,
        &inputs,
        &mut complete_pack,
        &mut index,
        crate::PackWriteLimits {
            max_objects: limits.max_objects.try_into().unwrap_or(u32::MAX),
            max_object_bytes: limits.max_object_bytes as u64,
            max_input_bytes: limits.max_decode_bytes as u64,
            max_pack_bytes: limits.max_pack_bytes as u64,
            ..Default::default()
        },
        crate::PackCompression::Ordinary,
        &mut || {
            check_cancelled(cancel).map_err(|_| {
                crate::PackWriteError::Io(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "fetch cancelled",
                ))
            })
        },
    )
    .map_err(|error| {
        check_cancelled(cancel)
            .err()
            .unwrap_or_else(|| error.into())
    })?;
    imported.index = index;
    imported.checksum = written.checksum;
    imported.complete_pack = Some(complete_pack);
    Ok(())
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

    fn dependency_transfer(
        format: crate::ObjectFormat,
        max_objects: usize,
    ) -> Result<ReceivedFetch, FetchError> {
        let object = crate::Object {
            format,
            kind: crate::ObjectKind::Blob,
            data: b"owned local payload".to_vec(),
        };
        let id = object.id();
        let known = KnownHistory {
            objects: [(id, object)].into(),
            ..Default::default()
        };
        let extra = crate::ObjectId::for_blob(format, b"wire payload");
        let mut pack = Vec::new();
        crate::write_pack(
            format,
            &[crate::PackObject {
                id: extra,
                kind: crate::ObjectKind::Blob,
                data: b"wire payload",
            }],
            &mut pack,
            &mut Vec::new(),
            crate::PackWriteLimits::default(),
        )
        .unwrap();
        ReceivedFetch::validate_known(
            Advertisement {
                refs: vec![],
                capabilities: vec![format!("object-format={format}").into_bytes()],
            },
            vec![id],
            pack,
            &known,
            vec![],
            super::super::FetchOptions {
                depth: std::num::NonZeroU32::new(2),
                limits: FetchLimits {
                    max_objects,
                    ..Default::default()
                },
            },
            &mut ValidationObserver::new(&AtomicBool::new(false), &mut |_| {}),
        )
    }

    #[rstest::rstest]
    #[case::sha1(crate::ObjectFormat::Sha1)]
    #[case::sha256(crate::ObjectFormat::Sha256)]
    fn depth_transfer_retains_local_dependencies_after_snapshot_drops(
        #[case] format: crate::ObjectFormat,
    ) {
        let received = dependency_transfer(format, 2).unwrap();
        let root = tempfile::tempdir().unwrap();
        let repo =
            Repository::init(format, root.path().join("repo"), crate::InitKind::Bare).unwrap();
        let (_, retention) = received
            .install_retained(&repo, crate::PackLimits::default(), &AtomicBool::new(false))
            .unwrap();
        let objects = repo.objects(crate::PackLimits::default()).unwrap();
        let object = objects
            .read(
                crate::ObjectId::for_blob(format, b"owned local payload"),
                crate::ReadLimits::default(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(object.data(), b"owned local payload");
        retention.release().unwrap();
    }

    #[rstest::rstest]
    #[case::sha1(crate::ObjectFormat::Sha1)]
    #[case::sha256(crate::ObjectFormat::Sha256)]
    fn materialized_depth_pack_obeys_combined_object_limit(#[case] format: crate::ObjectFormat) {
        assert!(matches!(
            dependency_transfer(format, 1),
            Err(FetchError::Index(crate::PackWriteError::Limit(_)))
        ));
    }

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
