//! Pinned file pairs with bounded offset tables and explicit streaming pack verification.

#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

mod window;
pub(crate) use window::HeaderScope;

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

use super::index::{Entry, Index, verify_hash, word};
use super::reader::{EntryHeader, Source, check_cancelled, decode};
use crate::object::Hasher;
use crate::{Object, ObjectFormat, ObjectId, ObjectKind, ObjectReadError as Error, ReadLimits};

/// A pinned artifact read with positioned reads, so lookups share no cursor or lock.
#[derive(Debug)]
struct Artifact {
    file: Arc<File>,
    path: PathBuf,
    len: usize,
    stamp: Stamp,
    cache_change: Option<(i64, i64)>,
    window_id: u64,
    // Only complete, immutable OS metadata results are published. OnceLock leaves the cache
    // empty if initialization panics; cached errors never contain caller-supplied payloads.
    truncation_check: std::sync::OnceLock<AssertUnwindSafe<Result<(), Arc<io::Error>>>>,
    #[cfg(test)]
    bytes_read: std::sync::atomic::AtomicUsize,
}

impl Artifact {
    fn open(path: &Path) -> Result<Self, Error> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;

            // Published artifacts are immutable; retain Git's rename/delete interoperability.
            use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};
            options.share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
        }
        let file = options
            .open(path)
            .map_err(|source| path_error(path, source))?;
        let stamp = Stamp::read(&file).map_err(|source| path_error(path, source))?;
        let len = stamp.len;
        let len = usize::try_from(len).map_err(|_| Error::Limit("artifact address space"))?;
        // Missing optional generation metadata disables reuse; ordinary parsing remains valid.
        let cache_change = change_time(&file).unwrap_or(None);
        Ok(Self {
            file: Arc::new(file),
            path: path.to_owned(),
            len,
            stamp,
            cache_change,
            window_id: NEXT_WINDOW_ID.fetch_add(1, Ordering::Relaxed),
            truncation_check: std::sync::OnceLock::new(),
            #[cfg(test)]
            bytes_read: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn read_at(&self, offset: usize, output: &mut [u8]) -> Result<(), Error> {
        self.read_file(offset, output)
            .map_err(|source| path_error(&self.path, source))
    }

    /// Reads exactly `output.len()` bytes at `offset` with one positioned read where possible.
    fn read_file(&self, offset: usize, output: &mut [u8]) -> io::Result<()> {
        #[cfg(test)]
        {
            self.bytes_read
                .fetch_add(output.len(), std::sync::atomic::Ordering::Relaxed);
            counters::OS_READS.with(|reads| reads.set(reads.get() + 1));
        }
        read_exact_at(&self.file, offset as u64, output)
    }

    fn bytes(&self, start: usize, end: usize) -> Result<Vec<u8>, Error> {
        let mut bytes = vec![0; end - start];
        self.read_at(start, &mut bytes)?;
        Ok(bytes)
    }

    fn check_unchanged(&self) -> Result<(), Error> {
        let current = Stamp::read(&self.file).map_err(|source| path_error(&self.path, source))?;
        if current != self.stamp {
            return Err(Error::PackChanged {
                path: self.path.clone(),
            });
        }
        Ok(())
    }

    /// Retained index bytes belong to this immutable artifact snapshot. Check its live length
    /// once before lookup; later in-place mutation violates the snapshot contract. Fresh byte
    /// reads still report short reads, and explicit verification checks the live artifacts.
    fn check_snapshot_not_truncated(&self) -> Result<(), Error> {
        let result = self.truncation_check.get_or_init(|| {
            AssertUnwindSafe((|| {
                let current = self.file.metadata().map_err(Arc::new)?.len();
                if current < self.len as u64 {
                    return Err(Arc::new(io::ErrorKind::UnexpectedEof.into()));
                }
                Ok(())
            })())
        });
        result.0.as_ref().map(|_| ()).map_err(|source| {
            path_error(
                &self.path,
                io::Error::new(source.kind(), Arc::clone(source)),
            )
        })
    }

    fn check_not_truncated(&self) -> Result<(), Error> {
        let current = self
            .file
            .metadata()
            .map_err(|source| path_error(&self.path, source))?
            .len();
        if current < self.len as u64 {
            return Err(path_error(&self.path, io::ErrorKind::UnexpectedEof.into()));
        }
        Ok(())
    }
}

/// Identity and content-generation metadata of the open handle, never the current pathname.
#[derive(Debug, Clone, Eq, Hash, PartialEq)]
struct Stamp {
    len: u64,
    modified: std::time::SystemTime,
    identity: (u64, u64),
}

impl Stamp {
    fn read(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino())
        };
        #[cfg(windows)]
        let identity = {
            use std::os::windows::io::AsRawHandle;

            use windows_sys::Win32::Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
            };
            let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
            // SAFETY: file owns a live handle and info points to writable, correctly sized storage.
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: a successful call initializes every field of BY_HANDLE_FILE_INFORMATION.
            let info = unsafe { info.assume_init() };
            (
                u64::from(info.dwVolumeSerialNumber),
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            )
        };
        #[cfg(not(any(unix, windows)))]
        let identity = (0, 0);
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
            identity,
        })
    }
}

/// Native change timestamps are cache discriminators, not pinned-reader freshness rules:
/// rename/unlink can change them while the original pinned artifact remains valid.
fn change_time(file: &File) -> io::Result<Option<(i64, i64)>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(Some((metadata.ctime(), metadata.ctime_nsec())))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::Storage::FileSystem::{
            FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
        };
        let mut info = std::mem::MaybeUninit::<FILE_BASIC_INFO>::uninit();
        // SAFETY: file owns a live handle; info has the size and alignment for FileBasicInfo.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileBasicInfo,
                info.as_mut_ptr().cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successful call initializes FILE_BASIC_INFO.
        let info = unsafe { info.assume_init() };
        Ok(Some((info.ChangeTime, 0)))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Ok(None)
    }
}

#[cfg(unix)]
pub(crate) fn read_exact_at(file: &File, offset: u64, output: &mut [u8]) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, output, offset)
}

#[cfg(windows)]
pub(crate) fn read_exact_at(file: &File, mut offset: u64, mut output: &mut [u8]) -> io::Result<()> {
    // Positioned reads on Windows move the shared cursor, which no other read here relies on.
    while !output.is_empty() {
        match std::os::windows::fs::FileExt::seek_read(file, output, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => {
                output = output.get_mut(count..).ok_or(io::ErrorKind::InvalidData)?;
                offset += count as u64;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Per-thread observations of pack I/O, so tests can bound how often a walk touches storage.
#[cfg(test)]
pub(crate) mod counters {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    thread_local! {
        /// Reads issued to the operating system for any pinned artifact.
        pub(crate) static OS_READS: Cell<usize> = const { Cell::new(0) };
        /// Inflations started per pack entry offset, including partial delta-header inflations.
        pub(crate) static INFLATIONS: RefCell<HashMap<usize, usize>> = RefCell::new(HashMap::new());
    }

    pub(crate) fn inflated(offset: usize) {
        INFLATIONS.with(|map| *map.borrow_mut().entry(offset).or_default() += 1);
    }

    pub(crate) fn reset() {
        OS_READS.with(|reads| reads.set(0));
        INFLATIONS.with(|map| map.borrow_mut().clear());
    }
}

fn path_error(path: &Path, source: io::Error) -> Error {
    Error::Path {
        path: path.to_owned(),
        source,
    }
}

/// Validated index bytes and bounded entry tables remain in memory; the original file stays pinned.
#[derive(Debug)]
pub(crate) struct FilePack {
    index: Artifact,
    index_bytes: Arc<Vec<u8>>,
    pack: Artifact,
    format: ObjectFormat,
    table: Arc<Index>,
    /// Shared validation is available to the weak cache only while readers pin this generation.
    _shared_index: Option<Arc<CachedIndex>>,
    /// Set once the index's own checksum has been verified, before its identities label reused
    /// bytes.
    index_verified: std::sync::OnceLock<()>,
    base_cache: super::reader::CacheScope,
    /// Resolved base kind and depth of each delta entry, by index position; see [`ChainKinds`].
    chain_kinds: ChainKinds,
    damage: Arc<Damage>,
}

/// Readers own shared validation and independent pins; the process LRU holds only weak pointers.
/// The pins prevent file-ID reuse while a generation can be reused, and drop with its last reader.
/// Each reader keeps its own checksum/damage/decoder state.
#[derive(Debug)]
struct CachedIndex {
    format: ObjectFormat,
    index_stamp: Stamp,
    pack_stamp: Stamp,
    index_change: Option<(i64, i64)>,
    pack_change: Option<(i64, i64)>,
    _index_file: File,
    _pack_file: File,
    bytes: Arc<Vec<u8>>,
    table: Arc<Index>,
}

impl CachedIndex {
    fn matches(&self, format: ObjectFormat, index: &Artifact, pack: &Artifact) -> bool {
        self.format == format
            && self.index_stamp == index.stamp
            && self.pack_stamp == pack.stamp
            && self.index_change == index.cache_change
            && self.pack_change == pack.cache_change
    }
}

const INDEX_CACHE_PAIRS: usize = 8;
static INDEX_CACHE: Mutex<IndexCache> = Mutex::new(IndexCache(VecDeque::new()));

#[derive(Debug)]
struct IndexCache(VecDeque<Weak<CachedIndex>>);

impl IndexCache {
    fn prune(&mut self) {
        self.0.retain(|entry| entry.strong_count() != 0);
    }

    fn insert(&mut self, entry: Arc<CachedIndex>) {
        self.prune();
        while self.0.len() >= INDEX_CACHE_PAIRS {
            self.0.pop_front();
        }
        self.0.push_back(Arc::downgrade(&entry));
    }
}

fn cached_index(
    cache: &Mutex<IndexCache>,
    format: ObjectFormat,
    index: &Artifact,
    pack: &Artifact,
) -> Option<Arc<CachedIndex>> {
    // Without native identity, metadata from unrelated files cannot establish a generation.
    if index.stamp.identity == (0, 0)
        || pack.stamp.identity == (0, 0)
        || index.cache_change.is_none()
        || pack.cache_change.is_none()
    {
        return None;
    }
    let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
    cache.prune();
    let (position, entry) = cache
        .0
        .iter()
        .enumerate()
        .filter_map(|(position, entry)| entry.upgrade().map(|entry| (position, entry)))
        .find(|(_, entry)| entry.matches(format, index, pack))?;
    let weak = cache.0.remove(position)?;
    cache.0.push_back(weak);
    Some(entry)
}

fn retain_index(
    cache: &Mutex<IndexCache>,
    format: ObjectFormat,
    index: &Artifact,
    pack: &Artifact,
    bytes: &Arc<Vec<u8>>,
    table: &Arc<Index>,
) -> Option<Arc<CachedIndex>> {
    if index.stamp.identity == (0, 0)
        || pack.stamp.identity == (0, 0)
        || index.cache_change.is_none()
        || pack.cache_change.is_none()
    {
        return None;
    }
    // Independent pins keep even a transient cache lookup safe against file-ID recycling,
    // without extending the lifetime of reader-local damage evidence through Arc<File>.
    let (Ok(index_file), Ok(pack_file)) = (index.file.try_clone(), pack.file.try_clone()) else {
        return None;
    };
    let entry = Arc::new(CachedIndex {
        format,
        index_stamp: index.stamp.clone(),
        pack_stamp: pack.stamp.clone(),
        index_change: index.cache_change,
        pack_change: pack.cache_change,
        _index_file: index_file,
        _pack_file: pack_file,
        bytes: bytes.clone(),
        table: table.clone(),
    });
    let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
    cache.prune();
    if let Some(previous) = cache
        .0
        .iter()
        .filter_map(Weak::upgrade)
        .find(|entry| entry.matches(format, index, pack))
    {
        return Some(previous);
    }
    cache.insert(entry.clone());
    Some(entry)
}

/// Damage belongs to the pinned artifact generation, not its filename. The empty atomic avoids
/// locking on healthy reads; errors are bounded by the number of indexed offsets encountered.
#[derive(Debug, Default)]
struct Damage {
    present: AtomicBool,
    entries: Mutex<BTreeMap<usize, Arc<Error>>>,
}

/// Refresh retains generation evidence without retaining old file handles or index tables.
#[derive(Debug, Clone, Eq, Hash, PartialEq)]
struct PackGeneration {
    stamp: Stamp,
    checksum: ObjectId,
    /// Platforms without native file identity distinguish artifacts by their canonical paths.
    fallback_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct PackDamage {
    generation: PackGeneration,
    damage: Arc<Damage>,
    /// Weak handles prove that native identities cannot have been recycled. Metadata-only
    /// freshening of a still-pinned pack must not clear its damage evidence.
    pinned: Vec<Weak<File>>,
}

impl PackDamage {
    fn same_pinned_pack(&self, generation: &PackGeneration) -> bool {
        self.generation.stamp.identity != (0, 0)
            && self.generation.stamp.identity == generation.stamp.identity
            && self.generation.stamp.len == generation.stamp.len
            && self.generation.checksum == generation.checksum
            && self.pinned.iter().any(|file| file.upgrade().is_some())
    }
}

/// Shared across clone/refresh generations. Entries grow with distinct packs opened during the
/// handle family's lifetime, while each damage map grows only with failed offsets in that pack.
#[derive(Debug, Default)]
pub(crate) struct DamageHistory(Mutex<HashMap<PackGeneration, PackDamage>>);

impl DamageHistory {
    pub(crate) fn share(&self, pack: &mut FilePack) {
        let mut record = pack.damage_record();
        let mut history = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(previous) = history.get_mut(&record.generation) {
            previous.pinned.retain(|file| file.strong_count() != 0);
            previous.pinned.append(&mut record.pinned);
            pack.damage = previous.damage.clone();
            return;
        }
        if let Some(previous) = history
            .values()
            .find(|previous| previous.same_pinned_pack(&record.generation))
        {
            record.damage = previous.damage.clone();
        }
        pack.damage = record.damage.clone();
        history.insert(record.generation.clone(), record);
    }
}

#[derive(Debug)]
pub(crate) enum ReadFailure {
    Damaged(Error),
    Other(Error),
}

#[derive(Debug, thiserror::Error)]
#[error("damaged packed entry at offset {offset}")]
struct BadEntry {
    offset: usize,
    #[source]
    cause: Arc<Error>,
}

#[cfg(feature = "tracing")]
pub(crate) fn is_damage_io(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<BadEntry>())
}

impl FilePack {
    fn damage_record(&self) -> PackDamage {
        PackDamage {
            generation: PackGeneration {
                stamp: self.pack.stamp.clone(),
                checksum: self.table.pack_hash,
                fallback_path: (self.pack.stamp.identity == (0, 0)).then(|| self.pack.path.clone()),
            },
            damage: self.damage.clone(),
            pinned: vec![Arc::downgrade(&self.pack.file)],
        }
    }

    #[cfg(test)]
    fn inherit_damage(&mut self, records: &[PackDamage]) -> bool {
        let generation = self.damage_record().generation;
        if let Some(record) = records
            .iter()
            .find(|record| record.generation == generation)
        {
            self.damage = record.damage.clone();
            return true;
        }
        false
    }

    pub(crate) fn check_damage(&self) -> Result<(), Error> {
        if self.damage.present.load(Ordering::Acquire) {
            let entries = self
                .damage
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some((&offset, cause)) = entries.first_key_value() {
                return Err(self.damage_error(offset, cause.clone()));
            }
        }
        Ok(())
    }

    fn damage_error(&self, offset: usize, cause: Arc<Error>) -> Error {
        path_error(
            &self.pack.path,
            io::Error::new(io::ErrorKind::InvalidData, BadEntry { offset, cause }),
        )
    }

    fn known_damage(&self, position: usize) -> Option<Error> {
        if !self.damage.present.load(Ordering::Acquire) {
            return None;
        }
        let entries = self
            .damage
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let offset = self.offset(position);
        entries
            .get(&offset)
            .map(|cause| self.damage_error(offset, cause.clone()))
    }

    fn failed_read(&self, position: usize, error: Error) -> ReadFailure {
        if !matches!(error, Error::Corrupt(_) | Error::DeltaCycle) {
            return ReadFailure::Other(error);
        }
        let mut entries = self
            .damage
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let offset = self.offset(position);
        let cause = entries
            .entry(offset)
            .or_insert_with(|| Arc::new(error))
            .clone();
        self.damage.present.store(true, Ordering::Release);
        ReadFailure::Damaged(self.damage_error(offset, cause))
    }

    pub(crate) fn read_candidate(
        &self,
        position: usize,
        limits: ReadLimits,
        cancel: &AtomicBool,
    ) -> Result<Object, ReadFailure> {
        if let Some(error) = self.known_damage(position) {
            return Err(ReadFailure::Damaged(error));
        }
        self.read(position, limits, cancel)
            .map_err(|error| self.failed_read(position, error))
    }

    pub(crate) fn read_candidate_unverified(
        &self,
        position: usize,
        limits: ReadLimits,
        cancel: &AtomicBool,
    ) -> Result<crate::UnverifiedObject, ReadFailure> {
        if let Some(error) = self.known_damage(position) {
            return Err(ReadFailure::Damaged(error));
        }
        self.read_unverified(position, limits, cancel)
            .map_err(|error| self.failed_read(position, error))
    }
    pub(crate) fn check_unchanged(&self) -> Result<(), Error> {
        self.pack.check_unchanged()?;
        self.index.check_unchanged()?;
        self.check_damage()
    }

    pub(crate) fn open(
        format: ObjectFormat,
        path: &Path,
        budget: &mut crate::PackLimits,
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        Self::open_after_index(format, path, budget, cancelled, || {})
    }

    // The checkpoint permits deterministic publication interleavings without sleeps or global
    // hooks. Production has no callback work between the two artifact opens.
    fn open_after_index(
        format: ObjectFormat,
        path: &Path,
        budget: &mut crate::PackLimits,
        cancelled: &AtomicBool,
        after_index: impl FnOnce(),
    ) -> Result<Self, Error> {
        Self::open_cached_after_index(format, path, budget, cancelled, after_index, &INDEX_CACHE)
    }

    fn open_cached_after_index(
        format: ObjectFormat,
        path: &Path,
        budget: &mut crate::PackLimits,
        cancelled: &AtomicBool,
        after_index: impl FnOnce(),
        cache: &Mutex<IndexCache>,
    ) -> Result<Self, Error> {
        check_cancelled(cancelled)?;
        let index_file = Artifact::open(path)?;
        after_index();
        let pack = Artifact::open(&path.with_extension("pack"))?;
        let bytes = index_file
            .len
            .checked_add(pack.len)
            .ok_or(Error::Limit("pack snapshot bytes"))?;
        budget.max_bytes = budget
            .max_bytes
            .checked_sub(bytes)
            .ok_or(Error::Limit("pack snapshot bytes"))?;
        // Charge the input before reading. Derived tables have at most one entry per 24 input
        // bytes, so a conservative fourfold bound also covers parsing and conversion overlap.
        budget.max_index_bytes = budget
            .max_index_bytes
            .checked_sub(index_file.len)
            .ok_or(Error::Limit("pack index bytes"))?;
        if pack.len < 12 + format.digest_len() {
            return Err(Error::Corrupt("pack header"));
        }
        let header = pack.bytes(0, 12)?;
        if !header.starts_with(b"PACK") {
            return Err(Error::Corrupt("pack header"));
        }
        let version = word(&header, 4)?;
        if !matches!(version, 2 | 3) {
            return Err(Error::PackVersion(version));
        }
        let end = pack.len - format.digest_len();
        let mut shared_index = cached_index(cache, format, &index_file, &pack);
        let (index_bytes, index) = if let Some(cached) = &shared_index {
            (cached.bytes.clone(), cached.table.clone())
        } else {
            let bytes = index_file.bytes(0, index_file.len)?;
            check_cancelled(cancelled)?;
            let table = Index::parse(format, &bytes, end)?;
            (Arc::new(bytes), Arc::new(table))
        };
        check_cancelled(cancelled)?;
        if word(&header, 8)? as usize != index.len() {
            return Err(Error::Corrupt("pack object count"));
        }
        let trailer = pack.bytes(end, pack.len)?;
        if index.pack_hash.as_bytes() != trailer {
            return Err(Error::Corrupt("index/pack checksum disagreement"));
        }
        index_file.check_unchanged()?;
        pack.check_unchanged()?;
        if shared_index.is_none() {
            shared_index = retain_index(cache, format, &index_file, &pack, &index_bytes, &index);
        }
        check_cancelled(cancelled)?;
        Ok(Self {
            index: index_file,
            index_bytes,
            pack,
            format,
            table: index,
            _shared_index: shared_index,
            index_verified: std::sync::OnceLock::new(),
            base_cache: super::reader::CacheScope::register(),
            chain_kinds: ChainKinds::default(),
            damage: Arc::new(Damage::default()),
        })
    }

    /// Binds receive-time identity evidence to this pinned representation, not a pathname or
    /// claimed checksum. The index retained by this reader must agree byte for byte as well.
    #[expect(
        clippy::indexing_slicing,
        reason = "every buffer slice ends at a count bounded by that buffer's length"
    )]
    pub(crate) fn matches_received(
        &self,
        index: &[u8],
        mut received: impl Read,
        len: usize,
        cancel: &AtomicBool,
    ) -> Result<bool, Error> {
        if self.index_bytes.as_slice() != index || self.pack.len != len {
            return Ok(false);
        }
        self.index.check_not_truncated()?;
        self.pack.check_not_truncated()?;
        let mut expected = [0; 64 * 1024];
        let mut actual = [0; 64 * 1024];
        let mut offset = 0;
        let end = len - self.format.digest_len();
        let mut hash = Hasher::new(self.format);
        while offset < len {
            check_cancelled(cancel)?;
            let count = expected.len().min(len - offset);
            received
                .read_exact(&mut expected[..count])
                .map_err(|source| path_error(&self.pack.path, source))?;
            self.pack.read_at(offset, &mut actual[..count])?;
            if expected[..count] != actual[..count] {
                return Ok(false);
            }
            if offset < end {
                hash.update(&actual[..count.min(end - offset)]);
            }
            offset += count;
        }
        if Some(hash.finalize().as_bytes()) != pack_hash(self.format, index) {
            return Ok(false);
        }
        check_cancelled(cancel)?;
        Ok(true)
    }

    /// Checks the index checksum, the entire pinned pack checksum and every v2 entry CRC without
    /// decoding objects.
    #[expect(
        clippy::indexing_slicing,
        reason = "every buffer slice ends at a count bounded by that buffer's length"
    )]
    pub(crate) fn verify(&self, cancelled: &AtomicBool) -> Result<(), Error> {
        check_cancelled(cancelled)?;
        self.verify_index()?;
        self.pack.check_not_truncated()?;
        let pack = &self.pack;
        let format = self.format;
        let end = pack.len - format.digest_len();
        let mut hash = Hasher::new(format);
        hash.update(&pack.bytes(0, 12)?);
        let mut buffer = [0; 64 * 1024];
        for position in self.table.by_offset() {
            let entry = self.entry(position)?;
            let mut offset = entry.offset;
            let mut crc = crc32fast::Hasher::new();
            while offset < entry.end {
                check_cancelled(cancelled)?;
                let count = buffer.len().min(entry.end - offset);
                pack.read_at(offset, &mut buffer[..count])?;
                hash.update(&buffer[..count]);
                crc.update(&buffer[..count]);
                offset += count;
            }
            if entry.crc.is_some_and(|expected| crc.finalize() != expected) {
                return Err(Error::Corrupt("entry CRC"));
            }
        }
        let trailer = pack.bytes(end, pack.len)?;
        if hash.finalize().as_bytes() != trailer {
            return Err(Error::Corrupt("pack checksum"));
        }
        if pack_hash(format, &self.index_bytes) != Some(&trailer[..]) {
            return Err(Error::Corrupt("index/pack checksum disagreement"));
        }
        check_cancelled(cancelled)
    }

    /// Checks the pinned index's own trailing checksum, once per reader.
    ///
    /// Opening validates only the index structure. Anything that trusts the index's identities
    /// without hashing the objects they name, such as copying entries into a new pack, checks this
    /// first so a damaged name cannot label other bytes.
    pub(crate) fn verify_index(&self) -> Result<(), Error> {
        if self.index_verified.get().is_none() {
            self.index.check_not_truncated()?;
            verify_hash(self.format, &self.index_bytes, "index checksum")?;
            let _ = self.index_verified.set(());
        }
        self.check_damage()
    }

    pub(crate) fn find(&self, id: ObjectId) -> Result<Option<usize>, Error> {
        if id.format() != self.format {
            return Ok(None);
        }
        self.index.check_snapshot_not_truncated()?;
        Ok(self.table.find(&self.index_bytes, id))
    }

    pub(crate) fn prefix_matches(
        &self,
        prefix: crate::ObjectIdPrefix,
    ) -> Result<Vec<ObjectId>, Error> {
        self.index.check_snapshot_not_truncated()?;
        let range = self.table.prefix_range(&self.index_bytes, prefix)?;
        range.map(|position| self.identity(position)).collect()
    }

    pub(crate) fn object_count(&self) -> usize {
        self.table.len()
    }

    /// Pack offset of the entry at index `position`, which must be below the index's length.
    pub(crate) fn offset(&self, position: usize) -> usize {
        self.table.offset(&self.index_bytes, position)
    }

    fn identity(&self, position: usize) -> Result<ObjectId, Error> {
        Ok(self.table.identity(&self.index_bytes, position))
    }

    /// Iterates identities from the validated pinned index without decoding their payloads.
    pub(crate) fn identities(&self) -> impl Iterator<Item = Result<ObjectId, Error>> + '_ {
        (0..self.table.len()).map(|position| self.identity(position))
    }

    /// Reads entry/delta headers only. Payload identity and CRC remain the reader's job.
    pub(crate) fn locate(
        &self,
        position: usize,
        limits: ReadLimits,
        cancel: &AtomicBool,
    ) -> Result<(ObjectKind, u64), Error> {
        self.locate_memoized(position, limits, cancel, &mut BaseKinds::new())
    }

    /// Locates like [`Self::locate`], sharing resolved delta chains across calls through `known`.
    ///
    /// Objects in one delta family share bases, so a batch reads each chain header once. `known`
    /// must only be used with this pack.
    pub(crate) fn locate_memoized(
        &self,
        mut position: usize,
        limits: ReadLimits,
        cancel: &AtomicBool,
        known: &mut BaseKinds,
    ) -> Result<(ObjectKind, u64), Error> {
        let mut seen = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut result_size = None;
        let resolved = loop {
            check_cancelled(cancel)?;
            // The requested entry is always read: its own header or delta prefix gives its size.
            if !seen.is_empty()
                && let Some(&(kind, depth)) = known.get(&position)
            {
                // A known chain is acyclic and within its own depth; check the combined depth.
                if seen.len() + depth > limits.max_delta_depth {
                    return Err(Error::Limit("delta depth"));
                }
                break (kind, depth + 1);
            }
            if !visited.insert(position) {
                return Err(Error::Corrupt("delta cycle"));
            }
            seen.push(position);
            let entry = self.entry(position)?;
            let header = super::reader::entry_header(self, &entry)?;
            if let Some(base) = header.base {
                if seen.len() > limits.max_delta_depth {
                    return Err(Error::Limit("delta depth"));
                }
                if result_size.is_none() {
                    result_size = Some(self.delta_result_size(&entry, &header)? as u64);
                }
                position = base;
            } else {
                let kind = match header.kind {
                    1 => ObjectKind::Commit,
                    2 => ObjectKind::Tree,
                    3 => ObjectKind::Blob,
                    4 => ObjectKind::Tag,
                    _ => return Err(Error::Corrupt("pack entry kind")),
                };
                if result_size.is_none() {
                    result_size = Some(header.length as u64);
                }
                break (kind, 0);
            }
        };
        let (kind, base_depth) = resolved;
        for (depth, position) in seen.iter().rev().enumerate() {
            known.insert(*position, (kind, base_depth + depth));
        }
        // The first iteration always reads the requested entry and records its size.
        let size = result_size.ok_or(Error::Corrupt("pack entry size"))?;
        if size > limits.max_object_bytes as u64 {
            return Err(Error::Limit("object bytes"));
        }
        Ok((kind, size))
    }

    pub(crate) fn read(
        &self,
        position: usize,
        limits: ReadLimits,
        cancelled: &AtomicBool,
    ) -> Result<Object, Error> {
        decode(self, position, limits, cancelled)
    }

    /// Inflates only a delta entry's two leading size varints and returns the result size.
    ///
    /// The varints are the first output bytes, so the zlib bytes read with the header nearly
    /// always hold them; otherwise the entry is streamed from the file. No instructions or base
    /// bytes are inflated, and the entry CRC is left to full reads.
    fn delta_result_size(&self, entry: &Entry, header: &EntryHeader) -> Result<usize, Error> {
        thread_local! {
            // Reused across entries: creating an inflater allocates and clears its window.
            static INFLATER: std::cell::RefCell<flate2::Decompress> =
                std::cell::RefCell::new(flate2::Decompress::new(true));
        }
        #[cfg(test)]
        counters::inflated(entry.offset);
        // Two size varints, each at most ten bytes on a 64-bit host.
        let mut prefix = [0; 20];
        let produced = INFLATER.with(|inflater| {
            let mut inflater = inflater.borrow_mut();
            inflater.reset(true);
            let status = inflater
                .decompress(header.ahead(), &mut prefix, flate2::FlushDecompress::None)
                .map_err(|_| Error::Corrupt("delta size prefix"))?;
            let produced = inflater.total_out() as usize;
            let complete = produced == prefix.len() || status == flate2::Status::StreamEnd;
            Ok::<_, Error>(
                (complete || header.ahead().len() == entry.end - header.data_start)
                    .then_some(produced),
            )
        })?;
        let produced = match produced {
            Some(produced) => produced,
            None => {
                let range = Range::entry(&self.pack, entry, header, None);
                let mut decoder = flate2::read::ZlibDecoder::new_with_buf(range, vec![0; 256]);
                let mut produced = 0;
                while produced < prefix.len() {
                    let Some(unfilled) = prefix.get_mut(produced..) else {
                        break;
                    };
                    match decoder.read(unfilled) {
                        Ok(0) => break,
                        Ok(count) => produced += count,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => return Err(Error::Corrupt("delta size prefix")),
                    }
                }
                produced
            }
        };
        let mut input = prefix
            .get(..produced)
            .ok_or(Error::Corrupt("delta size prefix"))?;
        super::delta::size(&mut input, 0, 0, true)?;
        super::delta::size(&mut input, 0, 0, true)
    }

    /// Decodes with every check of [`Self::read`] except the requested object's identity hash.
    pub(crate) fn read_unverified(
        &self,
        position: usize,
        limits: ReadLimits,
        cancelled: &AtomicBool,
    ) -> Result<crate::UnverifiedObject, Error> {
        let mut remaining = limits.max_decode_bytes;
        let object = super::reader::decode_checked(
            self,
            position,
            limits,
            cancelled,
            &mut remaining,
            super::reader::Identity::Skip,
        )?;
        Ok(crate::UnverifiedObject {
            kind: object.kind,
            data: object.data,
        })
    }
}

/// Resolved kinds and delta depths of entries in one pack, keyed by entry index.
pub(crate) type BaseKinds = std::collections::HashMap<usize, (ObjectKind, usize)>;

/// Base kinds and depths of delta entries whose chains have been resolved, by index position.
///
/// A pinned pack's bytes never change, so a resolved chain stays valid for the pack's lifetime.
/// Each entry takes one byte: zero while unknown, else the kind's pack type code in the low three
/// bits and the depth above it. Chains deeper than the byte can record are resolved each time.
#[derive(Debug, Default)]
struct ChainKinds(std::sync::OnceLock<Box<[std::sync::atomic::AtomicU8]>>);

impl ChainKinds {
    const MAX_DEPTH: usize = u8::MAX as usize >> 3;

    fn get(&self, position: usize) -> Option<(ObjectKind, usize)> {
        let value = self
            .0
            .get()?
            .get(position)?
            .load(std::sync::atomic::Ordering::Relaxed);
        let kind = match value & 7 {
            1 => ObjectKind::Commit,
            2 => ObjectKind::Tree,
            3 => ObjectKind::Blob,
            4 => ObjectKind::Tag,
            _ => return None,
        };
        Some((kind, usize::from(value >> 3)))
    }

    fn remember(&self, entries: usize, position: usize, kind: ObjectKind, depth: usize) {
        if depth > Self::MAX_DEPTH {
            return;
        }
        let code: u8 = match kind {
            ObjectKind::Commit => 1,
            ObjectKind::Tree => 2,
            ObjectKind::Blob => 3,
            ObjectKind::Tag => 4,
        };
        let slots = self
            .0
            .get_or_init(|| (0..entries).map(|_| Default::default()).collect());
        if let Some(slot) = slots.get(position) {
            slot.store(
                code | (depth as u8) << 3,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }
}

/// Keeps source corruption distinct from output failures during raw reuse.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CopyError {
    #[error(transparent)]
    Read(#[from] Error),
    #[error("write reused entry")]
    Write(#[source] io::Error),
}

impl FilePack {
    /// Reads only framing and delta size prefixes, never a reconstructed blob payload.
    ///
    /// The requested entry's header is always read. A delta's kind comes from its chain's base,
    /// which is resolved once per pack and then remembered for every entry on the chain.
    pub(crate) fn object_header(
        &self,
        position: usize,
        limits: ReadLimits,
    ) -> Result<(crate::ObjectKind, usize), Error> {
        let mut current = position;
        let mut length = None;
        let mut chain = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let (kind, base_depth) = loop {
            if !chain.is_empty()
                && let Some((kind, depth)) = self.chain_kinds.get(current)
            {
                // A remembered chain is acyclic; only the combined depth needs checking.
                if chain.len() + depth > limits.max_delta_depth {
                    return Err(Error::Limit("delta depth"));
                }
                break (kind, depth);
            }
            if !seen.insert(current) {
                return Err(Error::Corrupt("delta cycle"));
            }
            let entry = self.entry(current)?;
            let header = super::reader::entry_header(self, &entry)?;
            if let Some(base) = header.base {
                if seen.len() > limits.max_delta_depth {
                    return Err(Error::Limit("delta depth"));
                }
                if header.length > limits.max_delta_bytes {
                    return Err(Error::Limit("delta bytes"));
                }
                if length.is_none() {
                    length = Some(self.delta_result_size(&entry, &header)?);
                }
                chain.push(current);
                current = base;
            } else {
                let kind = match header.kind {
                    1 => crate::ObjectKind::Commit,
                    2 => crate::ObjectKind::Tree,
                    3 => crate::ObjectKind::Blob,
                    4 => crate::ObjectKind::Tag,
                    other => return Err(Error::ObjectType(other)),
                };
                if length.is_none() {
                    length = Some(header.length);
                } else {
                    self.chain_kinds
                        .remember(self.table.len(), current, kind, 0);
                }
                break (kind, 0);
            }
        };
        for (above, &delta) in chain.iter().rev().enumerate() {
            self.chain_kinds
                .remember(self.table.len(), delta, kind, base_depth + above + 1);
        }
        let length = length.ok_or(Error::Corrupt("pack entry size"))?;
        if length > limits.max_object_bytes {
            return Err(Error::Limit("object bytes"));
        }
        Ok((kind, length))
    }

    /// Copies an entry's bytes while checking the original framing and stream CRC.
    pub(crate) fn copy_raw(
        &self,
        entry: &Entry,
        header: &EntryHeader,
        output: &mut impl io::Write,
    ) -> Result<(), CopyError> {
        let delta = header.base.is_some();
        let start = if delta {
            header.data_start
        } else {
            entry.offset
        };
        let crc = entry.crc.map(|_| {
            let mut crc = crc32fast::Hasher::new();
            if delta {
                crc.update(header.encoded());
            }
            crc
        });
        let mut ahead = Vec::new();
        if !delta {
            ahead.extend_from_slice(header.encoded());
        }
        ahead.extend_from_slice(header.ahead());
        let input = Range {
            artifact: &self.pack,
            ahead: &ahead,
            position: start + ahead.len(),
            end: entry.end,
            crc,
        };
        let mut input = BufReader::with_capacity(64 * 1024, input);
        loop {
            let bytes = input
                .fill_buf()
                .map_err(|source| path_error(&self.pack.path, source))?;
            if bytes.is_empty() {
                break;
            }
            output.write_all(bytes).map_err(CopyError::Write)?;
            let count = bytes.len();
            input.consume(count);
        }
        input.get_ref().verify_crc(entry.crc)?;
        Ok(())
    }
}

impl Source for FilePack {
    fn format(&self) -> ObjectFormat {
        self.format
    }

    fn entry(&self, position: usize) -> Result<Entry, Error> {
        Ok(self.table.entry(&self.index_bytes, position))
    }

    fn find(&self, id: ObjectId) -> Result<Option<usize>, Error> {
        self.find(id)
    }

    fn base_cache(&self) -> Option<super::reader::CacheView<'_>> {
        Some(self.base_cache.view())
    }

    fn at_offset(&self, offset: usize) -> Result<usize, Error> {
        self.table.at_offset(&self.index_bytes, offset)
    }

    /// Reads the header with up to 4 KiB of the entry, so a typical entry needs one read.
    fn prefix(&self, entry: &Entry) -> Result<Vec<u8>, Error> {
        window::bytes(
            &self.pack,
            entry.offset,
            entry.end.min(entry.offset.saturating_add(4096)),
        )
    }

    #[expect(
        clippy::indexing_slicing,
        reason = "every buffer slice ends at a count bounded by that buffer's length"
    )]
    fn inflate(
        &self,
        entry: &Entry,
        header: &EntryHeader,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>, Error> {
        // One decoder per thread reuses zlib's window without serializing independent readers.
        // Reset before every entry, including after a failed or cancelled stream.
        thread_local! {
            static INFLATER: std::cell::RefCell<flate2::Decompress> =
                std::cell::RefCell::new(flate2::Decompress::new(true));
        }
        INFLATER.with(|inflater| {
            let mut decoder = inflater.borrow_mut();
            #[cfg(test)]
            counters::inflated(entry.offset);
            let start = header.data_start;
            let end = entry.end;
            let expected = header.length;
            let mut crc = entry.crc.map(|_| {
                let mut crc = crc32fast::Hasher::new();
                crc.update(header.encoded());
                crc.update(header.ahead());
                crc
            });
            // Input comes from the bytes read with the header, then from exact-sized positioned
            // reads of the rest of the entry; neither is zero-filled beyond what is read.
            const CHUNK: usize = 256 * 1024;
            let mut chunk = Vec::new();
            let mut from_ahead = true;
            let mut consumed_in_chunk = 0;
            let mut position = start + header.ahead().len();
            decoder.reset(true);
            // The declared size is unproven until the stream produces it: reserve a bounded start
            // and grow with the output, so a tiny malformed entry cannot request a huge buffer.
            let mut data = Vec::with_capacity(expected.min(super::delta::INITIAL_RESERVATION));
            let mut overflow = [0; 1];
            loop {
                check_cancelled(cancelled)?;
                let source = if from_ahead {
                    header.ahead()
                } else {
                    &chunk[..]
                };
                if consumed_in_chunk == source.len() && position < end {
                    let count = CHUNK.min(end - position);
                    chunk.clear();
                    chunk.resize(count, 0);
                    self.pack.read_at(position, &mut chunk)?;
                    if let Some(crc) = &mut crc {
                        crc.update(&chunk);
                    }
                    position += count;
                    from_ahead = false;
                    consumed_in_chunk = 0;
                }
                let input = if from_ahead {
                    &header.ahead()[consumed_in_chunk..]
                } else {
                    &chunk[consumed_in_chunk..]
                };
                let before_in = decoder.total_in();
                let before_out = decoder.total_out();
                if data.len() == data.capacity() && data.len() < expected {
                    data.reserve_exact((expected - data.len()).min(data.len().max(4096)));
                }
                let status = if data.len() < expected {
                    decoder.decompress_vec(input, &mut data, flate2::FlushDecompress::None)
                } else {
                    decoder.decompress(input, &mut overflow, flate2::FlushDecompress::None)
                }
                .map_err(|_| Error::Corrupt("invalid packed zlib stream"))?;
                let consumed = (decoder.total_in() - before_in) as usize;
                let count = (decoder.total_out() - before_out) as usize;
                consumed_in_chunk += consumed;
                if decoder.total_out() > expected as u64 {
                    return Err(Error::Corrupt("inflated entry exceeds declared size"));
                }
                if status == flate2::Status::StreamEnd {
                    if data.len() != expected || decoder.total_in() != (end - start) as u64 {
                        return Err(Error::Corrupt("packed entry length or trailing data"));
                    }
                    break;
                }
                if consumed == 0 && count == 0 {
                    return Err(Error::Corrupt("truncated packed zlib stream"));
                }
            }
            if let (Some(expected), Some(crc)) = (entry.crc, crc)
                && crc.finalize() != expected
            {
                return Err(Error::Corrupt("entry CRC"));
            }
            Ok(data)
        })
    }
}

/// The pack checksum an index records just before its own trailing checksum.
fn pack_hash(format: ObjectFormat, index: &[u8]) -> Option<&[u8]> {
    let width = format.digest_len();
    let start = index.len().checked_sub(2 * width)?;
    index.get(start..start + width)
}

/// A bounded cursor whose reads never disturb another lookup's logical position.
///
/// Bytes already read with the entry header are served from `ahead` before the file is read.
struct Range<'a> {
    artifact: &'a Artifact,
    ahead: &'a [u8],
    position: usize,
    end: usize,
    crc: Option<crc32fast::Hasher>,
}

impl<'a> Range<'a> {
    /// Streams an entry's zlib data from `data_start`, starting with the header's read-ahead.
    fn entry(
        artifact: &'a Artifact,
        entry: &Entry,
        header: &'a EntryHeader,
        crc: Option<crc32fast::Hasher>,
    ) -> Self {
        Self {
            artifact,
            ahead: header.ahead(),
            position: header.data_start + header.ahead().len(),
            end: entry.end,
            crc,
        }
    }
}

impl Range<'_> {
    fn verify_crc(&self, expected: Option<u32>) -> Result<(), Error> {
        if let Some(expected) = expected {
            // Entries with a recorded CRC always stream with a hasher; fail closed otherwise.
            let crc = self.crc.as_ref().ok_or(Error::Corrupt("entry CRC"))?;
            let actual = crc.clone().finalize();
            if actual != expected {
                return Err(Error::Corrupt("entry CRC"));
            }
        }
        Ok(())
    }
}

impl Read for Range<'_> {
    #[expect(
        clippy::indexing_slicing,
        reason = "every buffer slice ends at a count bounded by that buffer's length"
    )]
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if !self.ahead.is_empty() {
            let count = output.len().min(self.ahead.len());
            output[..count].copy_from_slice(&self.ahead[..count]);
            self.ahead = &self.ahead[count..];
            if let Some(crc) = &mut self.crc {
                crc.update(&output[..count]);
            }
            return Ok(count);
        }
        let count = output.len().min(self.end - self.position);
        if count == 0 {
            return Ok(0);
        }
        self.artifact
            .read_file(self.position, &mut output[..count])?;
        if let Some(crc) = &mut self.crc {
            crc.update(&output[..count]);
        }
        self.position += count;
        Ok(count)
    }
}

#[cfg(test)]
mod publication_tests {
    use std::fs;

    use rstest::rstest;

    use super::*;
    use crate::{ObjectKind, PackLimits, PackObject, PackWriteLimits};

    #[cfg(not(windows))]
    #[test]
    fn truncation_check_is_once_per_artifact_but_short_reads_still_fail() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("artifact");
        fs::write(&path, b"snapshot").unwrap();
        let artifact = Artifact::open(&path).unwrap();
        artifact.check_snapshot_not_truncated().unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(0)
            .unwrap();
        artifact.check_snapshot_not_truncated().unwrap();
        assert!(
            matches!(artifact.read_at(0, &mut [0; 1]), Err(Error::Path { source, .. }) if source.kind() == io::ErrorKind::UnexpectedEof)
        );
        assert!(artifact.check_unchanged().is_err());
    }

    fn pair(root: &Path, name: &str, format: ObjectFormat, data: &[u8]) -> PathBuf {
        let object = PackObject {
            id: ObjectId::for_blob(format, data),
            kind: ObjectKind::Blob,
            data,
        };
        let (mut pack, mut index) = (Vec::new(), Vec::new());
        crate::write_pack(
            format,
            &[object],
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
        )
        .unwrap();
        let path = root.join(name).with_extension("idx");
        fs::write(&path, index).unwrap();
        fs::write(path.with_extension("pack"), pack).unwrap();
        path
    }

    fn open_pair(path: &Path, format: ObjectFormat) -> FilePack {
        FilePack::open(
            format,
            path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn repeated_open_shares_index_without_reading_it(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "shared", format, b"content");
        // Isolate the LRU from unrelated tests opening other pack generations concurrently.
        let cache = Mutex::new(IndexCache(VecDeque::new()));
        let open = || {
            FilePack::open_cached_after_index(
                format,
                &path,
                &mut PackLimits::default(),
                &AtomicBool::new(false),
                || {},
                &cache,
            )
            .unwrap()
        };
        let first = open();
        let second = open();
        assert_eq!(first.index_bytes.as_ptr(), second.index_bytes.as_ptr());
        assert!(Arc::ptr_eq(&first.table, &second.table));
        assert_eq!(second.index.bytes_read.load(Ordering::Relaxed), 0);
        drop(first);
        drop(second);
        let reopened = open();
        assert_eq!(
            reopened.index.bytes_read.load(Ordering::Relaxed),
            reopened.index.len
        );
        reopened.verify(&AtomicBool::new(false)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn restored_index_mtime_cannot_reuse_validation() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "restored", ObjectFormat::Sha1, b"content");
        let cache = Mutex::new(IndexCache(VecDeque::new()));
        let first = FilePack::open_cached_after_index(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
            || {},
            &cache,
        )
        .unwrap();
        let modified = first.index.stamp.modified;
        let mut bytes = fs::read(&path).unwrap();
        bytes[11] = 1;
        fs::write(&path, bytes).unwrap();
        File::open(&path).unwrap().set_modified(modified).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
        assert!(matches!(
            FilePack::open_cached_after_index(
                ObjectFormat::Sha1,
                &path,
                &mut PackLimits::default(),
                &AtomicBool::new(false),
                || {},
                &cache
            ),
            Err(Error::Corrupt("index fanout"))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn restored_pack_mtime_cannot_reuse_index_validation() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "restored-pack", ObjectFormat::Sha1, b"content");
        let cache = Mutex::new(IndexCache(VecDeque::new()));
        let open = || {
            FilePack::open_cached_after_index(
                ObjectFormat::Sha1,
                &path,
                &mut PackLimits::default(),
                &AtomicBool::new(false),
                || {},
                &cache,
            )
            .unwrap()
        };
        let first = open();
        let pack_path = path.with_extension("pack");
        let mut bytes = fs::read(&pack_path).unwrap();
        bytes[12] ^= 1;
        fs::write(&pack_path, bytes).unwrap();
        File::open(&pack_path)
            .unwrap()
            .set_modified(first.pack.stamp.modified)
            .unwrap();
        assert_eq!(Stamp::read(&first.pack.file).unwrap(), first.pack.stamp);
        let second = open();
        assert_eq!(
            second.index.bytes_read.load(Ordering::Relaxed),
            second.index.len
        );
        assert!(second.verify(&AtomicBool::new(false)).is_err());
    }

    #[test]
    fn index_cache_does_not_outlive_last_reader() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "lifetime", ObjectFormat::Sha1, b"content");
        let cache = Mutex::new(IndexCache(VecDeque::new()));
        let open = || {
            FilePack::open_cached_after_index(
                ObjectFormat::Sha1,
                &path,
                &mut PackLimits::default(),
                &AtomicBool::new(false),
                || {},
                &cache,
            )
            .unwrap()
        };
        let first = open();
        let second = open();
        drop(first);
        let third = open();
        assert_eq!(third.index.bytes_read.load(Ordering::Relaxed), 0);
        drop(second);
        drop(third);
        let index = Artifact::open(&path).unwrap();
        let pack = Artifact::open(&path.with_extension("pack")).unwrap();
        assert!(cached_index(&cache, ObjectFormat::Sha1, &index, &pack).is_none());
        let reopened = open();
        assert_eq!(
            reopened.index.bytes_read.load(Ordering::Relaxed),
            reopened.index.len
        );
    }

    #[cfg(windows)]
    #[test]
    fn restored_index_mtime_invalidates_windows_change_time_key() {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::Storage::FileSystem::{
            FILE_BASIC_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            FILE_WRITE_ATTRIBUTES, FileBasicInfo, GetFileInformationByHandleEx,
            SetFileInformationByHandle,
        };
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "restored", ObjectFormat::Sha1, b"content");
        let cache = Mutex::new(IndexCache(VecDeque::new()));
        let open = || {
            FilePack::open_cached_after_index(
                ObjectFormat::Sha1,
                &path,
                &mut PackLimits::default(),
                &AtomicBool::new(false),
                || {},
                &cache,
            )
            .unwrap()
        };
        let first = open();
        let attributes = fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&path)
            .unwrap();
        attributes.set_modified(std::time::UNIX_EPOCH).unwrap();
        attributes.set_modified(first.index.stamp.modified).unwrap();
        // SetFileTime's mtime round trip does not necessarily change ChangeTime. Inject a
        // metadata generation change explicitly, preserving the restored mtime and other fields.
        let mut info = std::mem::MaybeUninit::<FILE_BASIC_INFO>::uninit();
        // SAFETY: the reader handle is live and info matches the requested class and size.
        assert_ne!(
            unsafe {
                GetFileInformationByHandleEx(
                    first.index.file.as_raw_handle(),
                    FileBasicInfo,
                    info.as_mut_ptr().cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            },
            0
        );
        // SAFETY: successful GetFileInformationByHandleEx initialized the structure.
        let mut info = unsafe { info.assume_init() };
        info.ChangeTime = info.ChangeTime.checked_add(10_000).unwrap();
        // SAFETY: the live handle has attribute-write access and info is a valid FileBasicInfo.
        assert_ne!(
            unsafe {
                SetFileInformationByHandle(
                    attributes.as_raw_handle(),
                    FileBasicInfo,
                    (&info as *const FILE_BASIC_INFO).cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            },
            0
        );
        drop(attributes);
        assert_eq!(Stamp::read(&first.index.file).unwrap(), first.index.stamp);
        assert_ne!(
            change_time(&first.index.file).unwrap(),
            first.index.cache_change
        );
        let second = open();
        assert_eq!(
            second.index.bytes_read.load(Ordering::Relaxed),
            second.index.len
        );
    }

    #[cfg(windows)]
    #[test]
    fn dropping_last_reader_releases_delete_pending_names() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "deleted", ObjectFormat::Sha1, b"content");
        let cache = Mutex::new(IndexCache(VecDeque::new()));
        let open = || {
            FilePack::open_cached_after_index(
                ObjectFormat::Sha1,
                &path,
                &mut PackLimits::default(),
                &AtomicBool::new(false),
                || {},
                &cache,
            )
            .unwrap()
        };
        let first = open();
        let second = open();
        fs::remove_file(&path).unwrap();
        fs::remove_file(path.with_extension("pack")).unwrap();
        drop(first);
        drop(second);
        fs::write(&path, b"replacement index").unwrap();
        fs::write(path.with_extension("pack"), b"replacement pack").unwrap();
    }

    #[test]
    fn cached_open_still_charges_index_budget() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "budget", ObjectFormat::Sha1, b"content");
        let cache = Mutex::new(IndexCache(VecDeque::new()));
        let open = |budget: &mut PackLimits| {
            FilePack::open_cached_after_index(
                ObjectFormat::Sha1,
                &path,
                budget,
                &AtomicBool::new(false),
                || {},
                &cache,
            )
        };
        let first = open(&mut PackLimits::default()).unwrap();
        let mut exact = PackLimits {
            max_index_bytes: first.index.len,
            ..PackLimits::default()
        };
        let second = open(&mut exact).unwrap();
        assert_eq!(second.index.bytes_read.load(Ordering::Relaxed), 0);
        assert_eq!(exact.max_index_bytes, 0);
        let mut exhausted = PackLimits {
            max_index_bytes: first.index.len - 1,
            ..PackLimits::default()
        };
        assert!(matches!(
            open(&mut exhausted),
            Err(Error::Limit("pack index bytes"))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn cached_index_truncation_is_rejected_by_live_and_fresh_readers() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "truncated", ObjectFormat::Sha1, b"content");
        let first = open_pair(&path, ObjectFormat::Sha1);
        let second = open_pair(&path, ObjectFormat::Sha1);
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(8)
            .unwrap();
        let id = ObjectId::for_blob(ObjectFormat::Sha1, b"content");
        assert!(first.find(id).is_err());
        assert!(second.find(id).is_err());
        assert!(first.check_unchanged().is_err());
        assert!(
            FilePack::open(
                ObjectFormat::Sha1,
                &path,
                &mut PackLimits::default(),
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn cached_pair_replacement_keeps_old_reader_and_validates_new_pair(
        #[case] format: ObjectFormat,
    ) {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "original", format, b"first");
        let replacement = pair(root.path(), "replacement", format, b"other");
        let old = open_pair(&path, format);
        fs::rename(&path, path.with_extension("old-index")).unwrap();
        fs::rename(path.with_extension("pack"), path.with_extension("old-pack")).unwrap();
        fs::rename(&replacement, &path).unwrap();
        fs::rename(
            replacement.with_extension("pack"),
            path.with_extension("pack"),
        )
        .unwrap();
        let new = open_pair(&path, format);
        let old_id = ObjectId::for_blob(format, b"first");
        let new_id = ObjectId::for_blob(format, b"other");
        old.check_unchanged().unwrap();
        assert_eq!(
            old.read(
                old.find(old_id).unwrap().unwrap(),
                ReadLimits::default(),
                &AtomicBool::new(false)
            )
            .unwrap()
            .data(),
            b"first"
        );
        assert_eq!(
            new.read(
                new.find(new_id).unwrap().unwrap(),
                ReadLimits::default(),
                &AtomicBool::new(false)
            )
            .unwrap()
            .data(),
            b"other"
        );
        assert!(new.find(old_id).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn changed_index_generation_cannot_reuse_validation() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "changed", ObjectFormat::Sha1, b"content");
        let _first = open_pair(&path, ObjectFormat::Sha1);
        let mut bytes = fs::read(&path).unwrap();
        // Break the first fanout bucket without changing the file's size.
        bytes[11] = 1;
        fs::write(&path, bytes).unwrap();
        File::open(&path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH)
            .unwrap();
        assert!(matches!(
            FilePack::open(
                ObjectFormat::Sha1,
                &path,
                &mut PackLimits::default(),
                &AtomicBool::new(false)
            ),
            Err(Error::Corrupt("index fanout"))
        ));
    }

    #[test]
    fn index_cache_bounds_pairs_not_representation_size() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "bounds", ObjectFormat::Sha1, b"content");
        let reader = open_pair(&path, ObjectFormat::Sha1);
        let entry = |bytes: Arc<Vec<u8>>| {
            Arc::new(CachedIndex {
                format: reader.format,
                index_stamp: reader.index.stamp.clone(),
                pack_stamp: reader.pack.stamp.clone(),
                index_change: reader.index.cache_change,
                pack_change: reader.pack.cache_change,
                _index_file: reader.index.file.try_clone().unwrap(),
                _pack_file: reader.pack.file.try_clone().unwrap(),
                bytes,
                table: reader.table.clone(),
            })
        };
        let small = entry(reader.index_bytes.clone());
        let mut cache = IndexCache(VecDeque::new());
        for _ in 0..INDEX_CACHE_PAIRS + 1 {
            cache.insert(small.clone());
        }
        assert_eq!(cache.0.len(), INDEX_CACHE_PAIRS);
        // Weak records retain nothing, so a Git-scale representation is shared like any other.
        let large = entry(Arc::new(Vec::with_capacity(128 * 1024 * 1024)));
        cache.insert(large.clone());
        assert_eq!(cache.0.len(), INDEX_CACHE_PAIRS);
        assert!(cache.0.back().unwrap().ptr_eq(&Arc::downgrade(&large)));
    }

    // Original malformed framing, with independent artifact checksums and CRCs rebuilt so the
    // encoded-entry parser, rather than the outer checksum validator, rejects it.
    fn malformed_pair(root: &Path, framing: &[u8]) -> PathBuf {
        let format = ObjectFormat::Sha1;
        let id = ObjectId::for_blob(format, b"fixture");
        let mut bytes = b"PACK\0\0\0\x02\0\0\0\x01".to_vec();
        bytes.extend_from_slice(framing);
        let checksum = format.checksum(&bytes);
        bytes.extend_from_slice(checksum.as_bytes());
        let entry = crate::pack::write::Entry {
            id,
            offset: 12,
            crc: crc32fast::hash(framing),
        };
        let index = crate::pack::write::encode_index(&[entry], checksum).unwrap();
        let path = root.join("malformed.idx");
        fs::write(&path, index).unwrap();
        fs::write(path.with_extension("pack"), bytes).unwrap();
        path
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn remembered_damage_skips_io_and_refuses_mutation(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "damaged", format, b"content");
        let pack_path = path.with_extension("pack");
        let mut bytes = fs::read(&pack_path).unwrap();
        let end = bytes.len() - format.digest_len();
        bytes[end - 1] ^= 1;
        fs::write(&pack_path, bytes).unwrap();
        let cancel = AtomicBool::new(false);
        let pack = FilePack::open(format, &path, &mut PackLimits::default(), &cancel).unwrap();
        let position = pack
            .find(ObjectId::for_blob(format, b"content"))
            .unwrap()
            .unwrap();
        assert!(matches!(
            pack.read_candidate(position, ReadLimits::default(), &cancel),
            Err(ReadFailure::Damaged(_))
        ));
        counters::reset();
        assert!(matches!(
            pack.read_candidate(position, ReadLimits::default(), &cancel),
            Err(ReadFailure::Damaged(_))
        ));
        assert!(matches!(
            pack.read_candidate_unverified(position, ReadLimits::default(), &cancel),
            Err(ReadFailure::Damaged(_))
        ));
        #[cfg(feature = "tracing")]
        assert!(
            matches!(pack.read_candidate(position, ReadLimits::default(), &cancel), Err(ReadFailure::Damaged(ref error)) if crate::trace::object(error) == "corrupt")
        );
        assert_eq!(counters::OS_READS.with(|reads| reads.get()), 0);
        assert!(pack.check_unchanged().is_err());
        assert!(pack.verify_index().is_err());
        let mut refreshed =
            FilePack::open(format, &path, &mut PackLimits::default(), &cancel).unwrap();
        assert!(refreshed.inherit_damage(&[pack.damage_record()]));
        counters::reset();
        assert!(matches!(
            refreshed.read_candidate(position, ReadLimits::default(), &cancel),
            Err(ReadFailure::Damaged(_))
        ));
        assert_eq!(counters::OS_READS.with(|reads| reads.get()), 0);
    }

    #[rstest]
    #[case::cancelled(Error::Cancelled)]
    #[case::limit(Error::Limit("object bytes"))]
    #[case::unsupported(Error::ObjectType(5))]
    #[case::io(Error::Path { path: PathBuf::from("fixture.pack"), source: io::ErrorKind::PermissionDenied.into() })]
    fn transient_failure_keeps_the_entry_readable(#[case] error: Error) {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "healthy", ObjectFormat::Sha1, b"content");
        let cancel = AtomicBool::new(false);
        let pack = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &cancel,
        )
        .unwrap();
        let position = pack
            .find(ObjectId::for_blob(ObjectFormat::Sha1, b"content"))
            .unwrap()
            .unwrap();
        assert!(matches!(
            pack.failed_read(position, error),
            ReadFailure::Other(_)
        ));
        assert_eq!(
            pack.read_candidate(position, ReadLimits::default(), &cancel)
                .unwrap()
                .data(),
            b"content"
        );
        pack.check_unchanged().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn freshening_a_pinned_pack_preserves_damage() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "pack", ObjectFormat::Sha1, b"content");
        let cancel = AtomicBool::new(false);
        let history = DamageHistory::default();
        let mut before = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &cancel,
        )
        .unwrap();
        history.share(&mut before);
        let _ = before.failed_read(0, Error::Corrupt("entry CRC"));
        before
            .pack
            .file
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        let mut after = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &cancel,
        )
        .unwrap();
        history.share(&mut after);
        assert_ne!(before.pack.stamp, after.pack.stamp);
        counters::reset();
        assert!(matches!(
            after.read_candidate(0, ReadLimits::default(), &cancel),
            Err(ReadFailure::Damaged(_))
        ));
        assert_eq!(counters::OS_READS.with(|reads| reads.get()), 0);
    }

    #[cfg(unix)]
    #[test]
    fn unpinned_metadata_change_cannot_transfer_old_damage() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "pack", ObjectFormat::Sha1, b"content");
        let cancel = AtomicBool::new(false);
        let history = DamageHistory::default();
        let mut before = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &cancel,
        )
        .unwrap();
        history.share(&mut before);
        let _ = before.failed_read(0, Error::Corrupt("entry CRC"));
        let identity = before.pack.stamp.identity;
        drop(before);
        File::open(path.with_extension("pack"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        let mut after = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &cancel,
        )
        .unwrap();
        assert_eq!(after.pack.stamp.identity, identity);
        history.share(&mut after);
        assert_eq!(
            after
                .read_candidate(0, ReadLimits::default(), &cancel)
                .unwrap()
                .data(),
            b"content"
        );
        after.check_unchanged().unwrap();
    }

    #[test]
    fn reused_native_identity_does_not_inherit_another_generation() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "old", ObjectFormat::Sha1, b"old");
        let new_path = pair(
            root.path(),
            "new",
            ObjectFormat::Sha1,
            b"new different content",
        );
        let cancel = AtomicBool::new(false);
        let old = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &cancel,
        )
        .unwrap();
        let _ = old.failed_read(0, Error::Corrupt("entry CRC"));
        let mut new = FilePack::open(
            ObjectFormat::Sha1,
            &new_path,
            &mut PackLimits::default(),
            &cancel,
        )
        .unwrap();
        let mut record = old.damage_record();
        // Model inode reuse after all old handles are released; generation metadata and the
        // trailer checksum must still distinguish the unrelated artifact.
        record.generation.stamp.identity = new.pack.stamp.identity;
        assert!(!new.inherit_damage(&[record]));
        assert_eq!(
            new.read_candidate(0, ReadLimits::default(), &cancel)
                .unwrap()
                .data(),
            b"new different content"
        );
    }

    #[test]
    fn replaced_index_keeps_damage_attached_to_pack_offset() {
        let root = tempfile::tempdir().unwrap();
        let format = ObjectFormat::Sha1;
        let objects = [
            PackObject {
                id: ObjectId::for_blob(format, b"one"),
                kind: ObjectKind::Blob,
                data: b"one",
            },
            PackObject {
                id: ObjectId::for_blob(format, b"two"),
                kind: ObjectKind::Blob,
                data: b"two",
            },
        ];
        let (mut bytes, mut index) = (Vec::new(), Vec::new());
        crate::write_pack(
            format,
            &objects,
            &mut bytes,
            &mut index,
            PackWriteLimits::default(),
        )
        .unwrap();
        let path = root.path().join("pair.idx");
        fs::write(&path, &index).unwrap();
        fs::write(path.with_extension("pack"), bytes).unwrap();
        let cancel = AtomicBool::new(false);
        let old = FilePack::open(format, &path, &mut PackLimits::default(), &cancel).unwrap();
        let position = old.find(objects[0].id).unwrap().unwrap();
        let offset = old.offset(position);
        let _ = old.failed_read(position, Error::Corrupt("entry CRC"));
        let crcs = 1032 + 2 * 20;
        let offsets = crcs + 2 * 4;
        for byte in 0..4 {
            index.swap(crcs + byte, crcs + 4 + byte);
            index.swap(offsets + byte, offsets + 4 + byte);
        }
        let end = index.len() - 20;
        let checksum = format.checksum(&index[..end]);
        index[end..].copy_from_slice(checksum.as_bytes());
        let replacement = root.path().join("replacement.idx");
        fs::write(&replacement, index).unwrap();
        fs::rename(replacement, &path).unwrap();
        let mut new = FilePack::open(format, &path, &mut PackLimits::default(), &cancel).unwrap();
        assert!(new.inherit_damage(&[old.damage_record()]));
        let moved_position = new.table.at_offset(&new.index_bytes, offset).unwrap();
        assert_ne!(moved_position, position);
        counters::reset();
        assert!(matches!(
            new.read_candidate(moved_position, ReadLimits::default(), &cancel),
            Err(ReadFailure::Damaged(_))
        ));
        assert_eq!(counters::OS_READS.with(|reads| reads.get()), 0);
    }

    #[test]
    fn atomic_replacement_does_not_inherit_old_damage() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "original", ObjectFormat::Sha1, b"content");
        let cancel = AtomicBool::new(false);
        let old = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &cancel,
        )
        .unwrap();
        let id = ObjectId::for_blob(ObjectFormat::Sha1, b"content");
        let position = old.find(id).unwrap().unwrap();
        let _ = old.failed_read(position, Error::Corrupt("entry CRC"));
        let replacement = pair(root.path(), "replacement", ObjectFormat::Sha1, b"content");
        fs::rename(&replacement, &path).unwrap();
        fs::rename(
            replacement.with_extension("pack"),
            path.with_extension("pack"),
        )
        .unwrap();
        let mut new = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &cancel,
        )
        .unwrap();
        assert!(!new.inherit_damage(&[old.damage_record()]));
        assert_eq!(
            new.read_candidate(position, ReadLimits::default(), &cancel)
                .unwrap()
                .data(),
            b"content"
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn open_does_not_read_pack_body(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let mut state = 123456789u64;
        let data: Vec<u8> = (0..2 * 1024 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let path = pair(root.path(), "large", format, &data);
        let reader = FilePack::open(
            format,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(reader.pack.len > 1024 * 1024);
        let opened_bytes = reader
            .pack
            .bytes_read
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(opened_bytes, 12 + format.digest_len());
        reader.verify(&AtomicBool::new(false)).unwrap();
        assert_eq!(
            reader
                .pack
                .bytes_read
                .load(std::sync::atomic::Ordering::Relaxed),
            opened_bytes + reader.pack.len
        );
    }

    /// A failed zlib stream must not contaminate a different pack read on the same thread.
    #[test]
    fn reusable_inflater_recovers_after_corrupt_stream() {
        let root = tempfile::tempdir().unwrap();
        let bad_path = malformed_pair(root.path(), &[0x37, 0x78, 0x9c, 0xff]);
        let good_path = pair(root.path(), "good", ObjectFormat::Sha1, b"healthy");
        let cancel = AtomicBool::new(false);
        let bad = FilePack::open(
            ObjectFormat::Sha1,
            &bad_path,
            &mut PackLimits::default(),
            &cancel,
        )
        .unwrap();
        let good = FilePack::open(
            ObjectFormat::Sha1,
            &good_path,
            &mut PackLimits::default(),
            &cancel,
        )
        .unwrap();
        assert!(bad.read(0, ReadLimits::default(), &cancel).is_err());
        assert_eq!(
            good.read(0, ReadLimits::default(), &cancel).unwrap().data(),
            b"healthy"
        );
    }

    #[rstest]
    #[case::reserved(&[0x50, 0])]
    #[case::truncated_size(&[0xb0])]
    #[case::zero_distance(&[0x60, 0])]
    #[case::before_pack(&[0x60, 127])]
    #[case::truncated_reference(&[0x70, 1])]
    #[case::missing_reference(&[0x70, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])]
    fn stored_entry_rejects_malformed_framing(#[case] framing: &[u8]) {
        let root = tempfile::tempdir().unwrap();
        let path = malformed_pair(root.path(), framing);
        let pack = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let _scope = super::super::HeaderScope::new();
        assert!(super::super::reader::entry_header(&pack, &pack.entry(0).unwrap()).is_err());
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn removal_between_opens_is_path_error(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "a", format, b"first");
        let result = FilePack::open_after_index(
            format,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
            || {
                fs::remove_file(path.with_extension("pack")).unwrap();
            },
        );
        assert!(
            matches!(result, Err(Error::Path { source, .. }) if source.kind() == io::ErrorKind::NotFound)
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn different_valid_pair_between_opens_is_rejected(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "a", format, b"first");
        let replacement = pair(root.path(), "b", format, b"other");
        let result = FilePack::open_after_index(
            format,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
            || {
                fs::remove_file(path.with_extension("pack")).unwrap();
                fs::rename(
                    replacement.with_extension("pack"),
                    path.with_extension("pack"),
                )
                .unwrap();
            },
        );
        assert!(matches!(result, Err(Error::Corrupt(_))));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn index_path_replacement_keeps_pinned_index(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let path = pair(root.path(), "a", format, b"first");
        let replacement = pair(root.path(), "b", format, b"other");
        let reader = FilePack::open_after_index(
            format,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
            || {
                fs::rename(&path, path.with_extension("old")).unwrap();
                fs::rename(replacement, &path).unwrap();
            },
        )
        .unwrap();
        let id = ObjectId::for_blob(format, b"first");
        let value = reader
            .read(
                reader.find(id).unwrap().unwrap(),
                ReadLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(value.data(), b"first");
        assert!(matches!(
            FilePack::open(
                format,
                &path,
                &mut PackLimits::default(),
                &AtomicBool::new(false)
            ),
            Err(Error::Corrupt(_))
        ));
    }
    #[test]
    fn header_windows_are_bounded_across_many_packs_and_drop_with_scope() {
        let root = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..32)
            .map(|i| {
                pair(
                    root.path(),
                    &format!("p{i}"),
                    ObjectFormat::Sha1,
                    &[i as u8; 128],
                )
            })
            .collect();
        let packs: Vec<_> = paths
            .iter()
            .map(|path| {
                FilePack::open(
                    ObjectFormat::Sha1,
                    path,
                    &mut PackLimits::trusted(),
                    &AtomicBool::new(false),
                )
                .unwrap()
            })
            .collect();
        let scope = super::super::HeaderScope::new();
        for pack in &packs {
            pack.prefix(&pack.entry(0).unwrap()).unwrap();
            assert!(super::window::allocated() <= 256 * 1024);
        }
        drop(scope);
        assert_eq!(super::window::allocated(), 0);
    }

    #[rstest]
    #[case::cross_window(256 * 1024 - 2, 256 * 1024 + 4)]
    #[case::final_bytes(512 * 1024 - 3, 512 * 1024)]
    #[case::empty_eof(512 * 1024, 512 * 1024)]
    fn header_window_boundaries(#[case] start: usize, #[case] end: usize) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bytes.pack");
        let bytes: Vec<_> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
        fs::write(&path, &bytes).unwrap();
        let artifact = super::Artifact::open(&path).unwrap();
        let _scope = super::super::HeaderScope::new();
        super::window::bytes(&artifact, 0, 4096).unwrap();
        let _ordered = super::super::HeaderScope::ordered();
        super::window::bytes(&artifact, 0, 8192).unwrap();
        assert_eq!(
            super::window::bytes(&artifact, start, end).unwrap(),
            bytes[start..end]
        );
        assert!(super::window::bytes(&artifact, 512 * 1024, 512 * 1024 + 1).is_err());
        assert!(super::window::bytes(&artifact, usize::MAX, 0).is_err());
    }

    #[test]
    fn truncation_cache_retries_after_initializer_panic() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bytes.pack");
        fs::write(&path, b"pinned bytes").unwrap();
        let artifact = super::Artifact::open(&path).unwrap();

        let panic = std::panic::catch_unwind(|| {
            artifact.truncation_check.get_or_init(|| {
                panic!("interrupted metadata check");
            });
        });
        assert!(panic.is_err());
        assert!(artifact.truncation_check.get().is_none());
        artifact.check_snapshot_not_truncated().unwrap();
        assert!(artifact.truncation_check.get().unwrap().0.is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn pinned_pack_denies_writers_but_allows_rename_and_delete() {
        use std::os::windows::fs::OpenOptionsExt;

        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("shared.pack");
        fs::write(&path, b"pinned bytes").unwrap();
        let artifact = super::Artifact::open(&path).unwrap();
        let writer = fs::OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&path);
        assert!(writer.is_err());
        let renamed = root.path().join("renamed.pack");
        fs::rename(&path, &renamed).unwrap();
        fs::remove_file(&renamed).unwrap();
        artifact.check_unchanged().unwrap();
        assert_eq!(artifact.bytes(0, 12).unwrap(), b"pinned bytes");
    }

    #[cfg(windows)]
    #[test]
    fn pinned_pack_allows_attribute_only_freshening_and_detects_it() {
        use std::os::windows::fs::OpenOptionsExt;

        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES,
        };
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("freshened.pack");
        fs::write(&path, b"pinned bytes").unwrap();
        let artifact = super::Artifact::open(&path).unwrap();
        let attributes = fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&path)
            .unwrap();
        attributes
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(42))
            .unwrap();
        assert_eq!(artifact.bytes(0, 12).unwrap(), b"pinned bytes");
        assert!(
            matches!(artifact.check_unchanged(), Err(Error::PackChanged { path: changed }) if changed == path)
        );
    }

    #[cfg(unix)]
    #[test]
    fn warmed_window_rename_replacement_keeps_pinned_original() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("old.pack");
        let replacement = root.path().join("new.pack");
        fs::write(&path, b"original").unwrap();
        fs::write(&replacement, b"replaced").unwrap();
        let artifact = super::Artifact::open(&path).unwrap();
        let _scope = super::super::HeaderScope::new();
        assert_eq!(super::window::bytes(&artifact, 0, 8).unwrap(), b"original");
        fs::rename(replacement, &path).unwrap();
        artifact.check_unchanged().unwrap();
        assert_eq!(super::window::bytes(&artifact, 0, 8).unwrap(), b"original");
    }
}
