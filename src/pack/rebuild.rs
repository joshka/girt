//! Regenerate a pack's `.idx` from the pack alone and replace the old index atomically.
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::fetch::{Bytes, FetchError, FetchLimits, index_self_contained};
use crate::pack::index::{Index, verify_hash};
use crate::{ObjectFormat, ObjectId, ObjectReadError};

/// Bounds the work [`rebuild_index`] may do while it verifies and resolves a pack.
///
/// Every limit applies to the whole pack. The defaults admit packs Git writes with ordinary
/// settings up to 256 MiB; raise them for larger packs or deeper delta chains (Git's aggressive
/// repack uses depth 250).
///
/// Choose this budget for a full verification and index reconstruction, not a quick header scan.
/// Even when replacing a tiny index, decoding can visit every pack object and its delta
/// dependencies.
///
/// ```
/// use girt::pack::IndexRebuildLimits;
/// let limits = IndexRebuildLimits::default()
///     .with_max_pack_bytes(32 * 1024 * 1024)
///     .with_max_decode_bytes(128 * 1024 * 1024)
///     .with_max_objects(10_000);
/// assert!(limits.max_decode_bytes > limits.max_pack_bytes); // Reconstruction has its own budget.
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct IndexRebuildLimits {
    /// Maximum `.pack` file size.
    pub max_pack_bytes: usize,
    /// Maximum object count in the pack header.
    pub max_objects: usize,
    /// Maximum inflated size of one non-delta object or reconstructed delta result.
    pub max_object_bytes: usize,
    /// Maximum inflated size of one delta program.
    pub max_delta_bytes: usize,
    /// Maximum total inflated and reconstructed bytes across the pack.
    pub max_decode_bytes: usize,
    /// Maximum delta chain depth.
    pub max_delta_depth: usize,
    /// Maximum delta resolution steps.
    pub max_resolution_steps: usize,
}

impl IndexRebuildLimits {
    /// Sets [`Self::max_pack_bytes`].
    #[must_use]
    pub fn with_max_pack_bytes(mut self, max_pack_bytes: usize) -> Self {
        self.max_pack_bytes = max_pack_bytes;
        self
    }

    /// Sets [`Self::max_objects`].
    #[must_use]
    pub fn with_max_objects(mut self, max_objects: usize) -> Self {
        self.max_objects = max_objects;
        self
    }

    /// Sets [`Self::max_object_bytes`].
    #[must_use]
    pub fn with_max_object_bytes(mut self, max_object_bytes: usize) -> Self {
        self.max_object_bytes = max_object_bytes;
        self
    }

    /// Sets [`Self::max_delta_bytes`].
    #[must_use]
    pub fn with_max_delta_bytes(mut self, max_delta_bytes: usize) -> Self {
        self.max_delta_bytes = max_delta_bytes;
        self
    }

    /// Sets [`Self::max_decode_bytes`].
    #[must_use]
    pub fn with_max_decode_bytes(mut self, max_decode_bytes: usize) -> Self {
        self.max_decode_bytes = max_decode_bytes;
        self
    }

    /// Sets [`Self::max_delta_depth`].
    #[must_use]
    pub fn with_max_delta_depth(mut self, max_delta_depth: usize) -> Self {
        self.max_delta_depth = max_delta_depth;
        self
    }

    /// Sets [`Self::max_resolution_steps`].
    #[must_use]
    pub fn with_max_resolution_steps(mut self, max_resolution_steps: usize) -> Self {
        self.max_resolution_steps = max_resolution_steps;
        self
    }
}

impl Default for IndexRebuildLimits {
    fn default() -> Self {
        let fetch = FetchLimits::default();
        Self {
            max_pack_bytes: fetch.max_pack_bytes,
            max_objects: fetch.max_objects,
            max_object_bytes: fetch.max_object_bytes,
            max_delta_bytes: fetch.max_delta_bytes,
            max_decode_bytes: fetch.max_decode_bytes,
            max_delta_depth: fetch.max_delta_depth,
            max_resolution_steps: fetch.max_resolution_steps,
        }
    }
}

impl IndexRebuildLimits {
    fn fetch(self) -> FetchLimits {
        FetchLimits {
            max_pack_bytes: self.max_pack_bytes,
            max_objects: self.max_objects,
            max_object_bytes: self.max_object_bytes,
            max_delta_bytes: self.max_delta_bytes,
            max_decode_bytes: self.max_decode_bytes,
            max_delta_depth: self.max_delta_depth,
            max_resolution_steps: self.max_resolution_steps,
            ..FetchLimits::default()
        }
    }
}

/// What [`rebuild_index`] did to the index file.
///
/// Use this result for reporting whether derived storage changed. An unchanged index does not
/// establish that some other process has retained the pack; callers still coordinate pack
/// retirement. See [`rebuild_index`] for a runnable reconstruction workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IndexRebuildOutcome {
    /// No index existed; the rebuilt one was published.
    Created,
    /// The existing index differed and was replaced.
    Replaced,
    /// The existing index already matched byte for byte; no file was written.
    Unchanged,
}

/// The result of a successful [`rebuild_index`].
///
/// Inspect the outcome to distinguish derived-data publication from an unchanged index. The pack is
/// retained in every case; [`IndexRebuildError::Unsynced`] can also carry this receipt after index
/// publication. See [`rebuild_index`] for a runnable workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IndexRebuilt {
    /// The pack checksum, which is also the index trailer's pack hash.
    pub checksum: ObjectId,
    /// Objects described by the index.
    pub objects: usize,
    /// The index path.
    pub index: PathBuf,
    /// Whether the index was created, replaced or already correct.
    pub outcome: IndexRebuildOutcome,
}

/// Errors from [`rebuild_index`].
///
/// The `.pack` is never changed. Every error except [`Self::Unsynced`] leaves any existing `.idx`
/// unchanged.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IndexRebuildError {
    /// The path does not name a `*.pack` file.
    /// Select the actual .pack artifact, not an index or a directory.
    #[error("not a .pack path: {0}")]
    NotPack(PathBuf),
    /// The pack uses a version this library does not index; only version 2 is supported.
    /// Use a supported pack-v2 artifact or a compatible external indexer; retrying unchanged bytes
    /// will not add support.
    #[error("unsupported pack version {0}")]
    PackVersion(u32),
    /// The pack failed verification: its trailer, an entry, a delta or an identity.
    /// Retain the original pack for diagnosis and recover a verified copy from a trusted source.
    #[error("pack verification")]
    Corrupt(#[source] ObjectReadError),
    /// A rebuild limit was exhausted.
    /// Select a justified decoding budget; the pack and old index remain unchanged.
    #[error("index rebuild limit exceeded: {0}")]
    Limit(&'static str),
    /// The cancellation flag was set.
    /// Make a new cancellation decision before retrying; no new index was published.
    #[error("index rebuild cancelled")]
    Cancelled,
    /// Reading the pack or writing the index failed before publication.
    /// Inspect the filesystem source; correct access or capacity before a fresh attempt.
    #[error("index rebuild I/O")]
    Io(#[source] io::Error),
    /// The rebuilt index was renamed into place, but syncing the directory failed, so the
    /// rename may not survive a crash. Either index then describes the same pack; retrying is
    /// safe and reports [`IndexRebuildOutcome::Unchanged`] once the new index is in place.
    /// Inspect the returned receipt: publication already happened. Coordinate with writers before
    /// retrying durability or rebuilding.
    #[error("rebuilt index published, but the pack directory sync failed")]
    Unsynced {
        /// The published index, as a successful rebuild would have reported it.
        rebuilt: Box<IndexRebuilt>,
        /// The directory sync failure.
        #[source]
        source: io::Error,
    },
}

impl From<io::Error> for IndexRebuildError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<FetchError> for IndexRebuildError {
    fn from(error: FetchError) -> Self {
        match error {
            FetchError::Pack(ObjectReadError::Limit(limit)) | FetchError::Limit(limit) => {
                Self::Limit(limit)
            }
            FetchError::Pack(ObjectReadError::Cancelled) | FetchError::Cancelled => Self::Cancelled,
            FetchError::Pack(ObjectReadError::PackVersion(version)) => Self::PackVersion(version),
            FetchError::Pack(error) => Self::Corrupt(error),
            FetchError::Io(error) => Self::Io(error),
            FetchError::Index(_) => Self::Limit("index encoding"),
            other => Self::Io(io::Error::other(other)),
        }
    }
}

/// Regenerates the v2 `.idx` beside `pack` and atomically replaces any existing index.
///
/// A pack's index is derived data, so a damaged or missing index never loses objects. This
/// reads a private copy of the pack, checks its trailer hash, the framing and CRC of every
/// entry, decodes every object and resolves every delta inside the pack. Then it encodes the
/// index exactly as `git index-pack` does by default (version 2, 64-bit offsets for entries at or
/// past 2 GiB) and parses it back before publication. The new index is written to a
/// `tmp_idx_*` file in the pack directory and synced, renamed over `<stem>.idx`, and the
/// directory is synced. Readers therefore see the old or the new complete index, never a
/// partial one. If the existing index already matches byte for byte, nothing is written.
///
/// The `.pack` is only read. `format` must be the repository's object format; a mismatch fails
/// as [`IndexRebuildError::Corrupt`]. A thin pack (a delta whose base is outside the pack) is
/// refused, because a stored pack must be self-contained.
///
/// Do not run this concurrently with maintenance that retires the same pack: like
/// `git index-pack`, a rename after the retirement can leave an `.idx` without its `.pack`. No
/// object data is lost; removing that index restores the store.
///
/// # Errors
///
/// Returns [`IndexRebuildError::Corrupt`] when the pack fails verification,
/// [`IndexRebuildError::PackVersion`] for a pack other than version 2 (Git also indexes version
/// 3), and limit, cancellation or I/O errors. On these errors any existing index is unchanged;
/// at most a `tmp_pack_*` or `tmp_idx_*` file can remain after a crash, which Git ignores.
/// [`IndexRebuildError::Unsynced`] reports that the new index was published but its directory
/// entry may not be durable.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::pack::{IndexRebuildLimits, IndexRebuildOutcome, rebuild_index};
/// use girt::{ObjectFormat, ObjectId, ObjectKind, PackObject, PackWriteLimits, write_pack};
/// let input = PackObject {
///     id: ObjectId::for_blob(ObjectFormat::Sha1, b"manifest"),
///     kind: ObjectKind::Blob,
///     data: b"manifest",
/// };
/// let (mut bytes, mut index) = (Vec::new(), Vec::new());
/// let written = write_pack(
///     ObjectFormat::Sha1,
///     &[input],
///     &mut bytes,
///     &mut index,
///     PackWriteLimits::default(),
/// )?;
/// let directory = tempfile::tempdir()?;
/// let path = directory.path().join("private.pack");
/// std::fs::write(&path, &bytes)?;
/// let cancel = AtomicBool::new(false);
/// let report = rebuild_index(
///     &path,
///     ObjectFormat::Sha1,
///     IndexRebuildLimits::default(),
///     &cancel,
/// )?;
/// assert!(report.index.exists());
/// assert_eq!(report.checksum, written.checksum);
/// assert_eq!(report.objects, 1);
/// assert_eq!(report.outcome, IndexRebuildOutcome::Created);
/// let again = rebuild_index(
///     &path,
///     ObjectFormat::Sha1,
///     IndexRebuildLimits::default(),
///     &cancel,
/// )?;
/// assert_eq!(again.outcome, IndexRebuildOutcome::Unchanged);
/// // Damage only this private fixture's derived index, then reconstruct it from the pack.
/// std::fs::write(&report.index, b"damaged index")?;
/// let repaired = rebuild_index(
///     &path,
///     ObjectFormat::Sha1,
///     IndexRebuildLimits::default(),
///     &cancel,
/// )?;
/// assert_eq!(repaired.outcome, IndexRebuildOutcome::Replaced);
/// assert_eq!(std::fs::read(&path)?, bytes); // Only derived index data was written.
///
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn rebuild_index(
    pack: &Path,
    format: ObjectFormat,
    limits: IndexRebuildLimits,
    cancel: &AtomicBool,
) -> Result<IndexRebuilt, IndexRebuildError> {
    rebuild_with(pack, format, limits, cancel, &Publish::default())
}

/// Publication steps, replaceable in tests to inject faults on either side of the rename.
struct Publish {
    rename: fn(tempfile::NamedTempFile, &Path) -> io::Result<()>,
    sync_directory: fn(&Path) -> io::Result<()>,
}

impl Default for Publish {
    fn default() -> Self {
        Self {
            rename: |temporary, path| {
                // Windows refuses to replace a read-only file, and Git writes indexes read-only.
                #[cfg(windows)]
                if let Ok(metadata) = std::fs::metadata(path) {
                    let mut permissions = metadata.permissions();
                    if permissions.readonly() {
                        #[allow(clippy::permissions_set_readonly_false)]
                        permissions.set_readonly(false);
                        std::fs::set_permissions(path, permissions)?;
                    }
                }
                // `std::fs::rename` replaces with POSIX semantics where Windows supports them, so
                // readers holding the old index open do not block the replacement.
                // Disarm deletion before the rename: afterwards the temporary name is gone, and
                // Windows reports that as an error even though the index was published.
                let temporary = temporary
                    .into_temp_path()
                    .keep()
                    .map_err(|error| error.error)?;
                std::fs::rename(&temporary, path).inspect_err(|_| {
                    let _ = std::fs::remove_file(&temporary);
                })
            },
            sync_directory: |directory| {
                #[cfg(unix)]
                File::open(directory)?.sync_all()?;
                #[cfg(not(unix))]
                let _ = directory;
                Ok(())
            },
        }
    }
}

const CHUNK: usize = 64 * 1024;

fn rebuild_with(
    pack: &Path,
    format: ObjectFormat,
    limits: IndexRebuildLimits,
    cancel: &AtomicBool,
    publish: &Publish,
) -> Result<IndexRebuilt, IndexRebuildError> {
    if pack.extension().is_none_or(|extension| extension != "pack") {
        return Err(IndexRebuildError::NotPack(pack.into()));
    }
    check_cancelled(cancel)?;
    let directory = pack
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let index_path = pack.with_extension("idx");
    // Verify a private copy so a concurrent writer to the pack cannot change what the index
    // describes after verification, and so nothing here can write through to the pack.
    let mut source = File::open(pack)?;
    let mode = source.metadata()?.permissions();
    let mut copy = tempfile::Builder::new()
        .prefix("tmp_pack_")
        .tempfile_in(directory)?;
    let mut copied = 0usize;
    let mut buffer = vec![0; CHUNK];
    loop {
        check_cancelled(cancel)?;
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        copied += n;
        if copied > limits.max_pack_bytes {
            return Err(IndexRebuildError::Limit("pack bytes"));
        }
        copy.write_all(&buffer[..n])?;
    }
    let (index, checksum) =
        index_self_contained(format, Bytes::File(copy, copied), limits.fetch(), cancel)?;
    let parsed = Index::parse(format, &index, copied - format.digest_len())
        .map_err(IndexRebuildError::Corrupt)?;
    verify_hash(format, &index, "index checksum").map_err(IndexRebuildError::Corrupt)?;
    debug_assert_eq!(parsed.pack_hash, checksum);
    let mut rebuilt = IndexRebuilt {
        checksum,
        objects: parsed.len(),
        index: index_path.clone(),
        outcome: IndexRebuildOutcome::Unchanged,
    };
    rebuilt.outcome = match same_bytes(&index_path, &index, cancel)? {
        Some(true) => return Ok(rebuilt),
        Some(false) => IndexRebuildOutcome::Replaced,
        None => IndexRebuildOutcome::Created,
    };
    let mut temporary = tempfile::Builder::new()
        .prefix("tmp_idx_")
        .tempfile_in(directory)?;
    temporary.write_all(&index)?;
    // Git gives an index the same mode as its pack, which reflects `core.sharedRepository`.
    temporary.as_file().set_permissions(mode)?;
    temporary.as_file().sync_all()?;
    check_cancelled(cancel)?;
    (publish.rename)(temporary, &index_path)?;
    // The rename has happened; from here an error must say so rather than claim no change.
    (publish.sync_directory)(directory).map_err(|source| IndexRebuildError::Unsynced {
        rebuilt: Box::new(rebuilt.clone()),
        source,
    })?;
    Ok(rebuilt)
}

fn check_cancelled(cancel: &AtomicBool) -> Result<(), IndexRebuildError> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        Err(IndexRebuildError::Cancelled)
    } else {
        Ok(())
    }
}

/// Compares an existing index with `expected` without reading more than `expected.len()` bytes,
/// so an oversized damaged index costs no more than a correct one. `None` means it is absent.
fn same_bytes(
    path: &Path,
    expected: &[u8],
    cancel: &AtomicBool,
) -> Result<Option<bool>, IndexRebuildError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() != expected.len() as u64 {
        return Ok(Some(false));
    }
    let mut buffer = vec![0; CHUNK];
    for chunk in expected.chunks(CHUNK) {
        check_cancelled(cancel)?;
        let actual = &mut buffer[..chunk.len()];
        match file.read_exact(actual) {
            Ok(()) if actual == chunk => {}
            Ok(()) => return Ok(Some(false)),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(Some(false)),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(file.read(&mut [0])? == 0))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{ObjectKind, PackObject, PackWriteLimits, write_pack};

    /// Writes a one-blob SHA-1 pack and its index, then damages the index checksum.
    fn damaged() -> (tempfile::TempDir, PathBuf, Vec<u8>, Vec<u8>) {
        let directory = tempfile::tempdir().unwrap();
        let data = b"rebuild me";
        let object = PackObject {
            id: ObjectId::for_blob(ObjectFormat::Sha1, data),
            kind: ObjectKind::Blob,
            data,
        };
        let (mut pack, mut index) = (Vec::new(), Vec::new());
        write_pack(
            ObjectFormat::Sha1,
            &[object],
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
        )
        .unwrap();
        let path = directory.path().join("pack-test.pack");
        fs::write(&path, &pack).unwrap();
        let mut bad = index.clone();
        *bad.last_mut().unwrap() ^= 1;
        fs::write(path.with_extension("idx"), &bad).unwrap();
        (directory, path, index, bad)
    }

    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn rebuild(
        path: &Path,
        limits: IndexRebuildLimits,
        publish: &Publish,
    ) -> Result<IndexRebuilt, IndexRebuildError> {
        rebuild_with(
            path,
            ObjectFormat::Sha1,
            limits,
            &AtomicBool::new(false),
            publish,
        )
    }

    #[test]
    fn failed_rename_leaves_the_old_index_and_no_temporary_files() {
        let (directory, path, _, bad) = damaged();
        let before = names(directory.path());
        let publish = Publish {
            rename: |_, _| Err(io::Error::other("injected rename failure")),
            ..Publish::default()
        };

        let result = rebuild(&path, IndexRebuildLimits::default(), &publish);

        assert!(matches!(result, Err(IndexRebuildError::Io(_))));
        assert_eq!(fs::read(path.with_extension("idx")).unwrap(), bad);
        assert_eq!(names(directory.path()), before);
    }

    #[test]
    fn failed_directory_sync_reports_the_published_index() {
        let (_directory, path, good, _) = damaged();
        let publish = Publish {
            sync_directory: |_| Err(io::Error::other("injected sync failure")),
            ..Publish::default()
        };

        let result = rebuild(&path, IndexRebuildLimits::default(), &publish);

        let Err(IndexRebuildError::Unsynced { rebuilt, .. }) = result else {
            panic!("expected Unsynced, got {result:?}");
        };
        assert_eq!(rebuilt.outcome, IndexRebuildOutcome::Replaced);
        assert_eq!(fs::read(path.with_extension("idx")).unwrap(), good);
    }

    #[test]
    fn pack_byte_limit_is_exact_and_stops_the_copy() {
        let (directory, path, good, bad) = damaged();
        let before = names(directory.path());
        let len = fs::metadata(&path).unwrap().len() as usize;
        let limits = |max_pack_bytes| IndexRebuildLimits {
            max_pack_bytes,
            ..IndexRebuildLimits::default()
        };

        let refused = rebuild(&path, limits(len - 1), &Publish::default());
        assert!(matches!(
            refused,
            Err(IndexRebuildError::Limit("pack bytes"))
        ));
        assert_eq!(fs::read(path.with_extension("idx")).unwrap(), bad);
        assert_eq!(names(directory.path()), before);

        rebuild(&path, limits(len), &Publish::default()).unwrap();
        assert_eq!(fs::read(path.with_extension("idx")).unwrap(), good);
    }

    #[test]
    fn oversized_damaged_index_is_replaced_without_reading_it() {
        let (_directory, path, good, _) = damaged();
        // Sparse on the filesystems CI uses; reading it whole would need 4 GiB.
        File::options()
            .write(true)
            .open(path.with_extension("idx"))
            .unwrap()
            .set_len(1 << 32)
            .unwrap();

        let rebuilt = rebuild(&path, IndexRebuildLimits::default(), &Publish::default()).unwrap();

        assert_eq!(rebuilt.outcome, IndexRebuildOutcome::Replaced);
        assert_eq!(fs::read(path.with_extension("idx")).unwrap(), good);
    }

    #[test]
    fn index_comparison_is_bounded_by_the_expected_length() {
        let (directory, _, good, _) = damaged();
        let index = directory.path().join("compare.idx");
        let longer = [good.as_slice(), b"x"].concat();
        let cancel = AtomicBool::new(false);

        fs::write(&index, &good).unwrap();
        assert_eq!(same_bytes(&index, &good, &cancel).unwrap(), Some(true));
        fs::write(&index, &longer).unwrap();
        assert_eq!(same_bytes(&index, &good, &cancel).unwrap(), Some(false));
        fs::write(&index, &good[..good.len() - 1]).unwrap();
        assert_eq!(same_bytes(&index, &good, &cancel).unwrap(), Some(false));
        fs::remove_file(&index).unwrap();
        assert_eq!(same_bytes(&index, &good, &cancel).unwrap(), None);
    }

    /// Git 2.56.0 `index-pack` accepts this empty version-3 pack; girt reports it unsupported,
    /// not corrupt.
    #[test]
    fn version_3_pack_is_unsupported_not_corrupt() {
        let directory = tempfile::tempdir().unwrap();
        let mut pack = b"PACK\0\0\0\x03\0\0\0\0".to_vec();
        let mut hasher = crate::object::Hasher::new(ObjectFormat::Sha1);
        hasher.update(&pack);
        pack.extend_from_slice(hasher.finalize().as_bytes());
        let path = directory.path().join("pack-v3.pack");
        fs::write(&path, &pack).unwrap();

        let result = rebuild(&path, IndexRebuildLimits::default(), &Publish::default());

        assert!(matches!(result, Err(IndexRebuildError::PackVersion(3))));
        assert!(!path.with_extension("idx").exists());
    }
}
