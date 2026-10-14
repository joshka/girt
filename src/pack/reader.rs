#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(test)]
use flate2::{Decompress, FlushDecompress, Status};

use super::delta::{apply, byte, charge, size};
#[cfg(test)]
use super::index::{Index, verify_hash, word};
use crate::{Object, ObjectId, ObjectKind, ObjectReadError as Error, ReadLimits};

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct Pack {
    index: Index,
    index_bytes: Vec<u8>,
    data: Vec<u8>,
}

#[cfg(test)]
impl Pack {
    pub fn open(
        format: crate::ObjectFormat,
        index_bytes: &[u8],
        data: Vec<u8>,
    ) -> Result<Self, Error> {
        if data.len() < 12 + format.digest_len() || &data[..4] != b"PACK" {
            return Err(Error::Corrupt("pack header"));
        }
        let version = word(&data, 4)?;
        if !matches!(version, 2 | 3) {
            return Err(Error::PackVersion(version));
        }
        let end = data.len() - format.digest_len();
        let index = Index::parse(format, index_bytes, end)?;
        verify_hash(format, index_bytes, "index checksum")?;
        if word(&data, 8)? as usize != index.len() {
            return Err(Error::Corrupt("pack object count"));
        }
        verify_hash(format, &data, "pack checksum")?;
        if *index.pack_hash.as_bytes() != data[end..] {
            return Err(Error::Corrupt("index/pack checksum disagreement"));
        }
        for position in 0..index.len() {
            let entry = index.entry(index_bytes, position);
            if entry
                .crc
                .is_some_and(|crc| crc32fast::hash(&data[entry.offset..entry.end]) != crc)
            {
                return Err(Error::Corrupt("entry CRC"));
            }
        }
        Ok(Self {
            index,
            index_bytes: index_bytes.to_vec(),
            data,
        })
    }

    pub fn find(&self, id: ObjectId) -> Option<usize> {
        self.index.find(&self.index_bytes, id)
    }

    pub fn read(&self, position: usize, limits: ReadLimits) -> Result<Object, Error> {
        decode(self, position, limits, &AtomicBool::new(false))
    }
}

pub(crate) trait Source {
    fn format(&self) -> crate::ObjectFormat;
    fn entry(&self, position: usize) -> Result<super::index::Entry, Error>;
    fn find(&self, id: ObjectId) -> Result<Option<usize>, Error>;
    fn at_offset(&self, offset: usize) -> Result<usize, Error>;
    fn prefix(&self, entry: &super::index::Entry) -> Result<Vec<u8>, Error>;
    fn inflate(
        &self,
        entry: &super::index::Entry,
        header: &EntryHeader,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>, Error>;
    /// Receive-owned sources can retain identity proof after evicting the decoded payload.
    /// Such sources must keep their representation private and check its recorded entry bytes
    /// on re-read. Ordinary storage/content sources always use the default mandatory hash.
    fn identity_verified(&self, _position: usize) -> bool {
        false
    }

    /// Decoded objects kept to avoid re-inflating shared delta bases, if this source has one.
    fn base_cache(&self) -> Option<CacheView<'_>> {
        None
    }
}

/// Recently decoded pack entries, keyed by scope and entry position, like Git's delta base cache.
///
/// Reading a deltified object reconstructs its whole chain, and neighbouring objects usually
/// share bases, so caching them avoids inflating the same bases repeatedly. Entries record
/// whether their identity was checked; an unchecked entry is checked before being returned as
/// the requested object.
///
/// Stored pack readers share one byte limit per process, as Git has one cache per process, so
/// opening more stores does not multiply the memory it can hold. Each reader still sees only the
/// entries it decoded from its own pinned files, and dropping it releases them. A receive-owned
/// source keeps a private cache.
pub(crate) struct BaseCache {
    inner: Mutex<CacheInner>,
}

impl std::fmt::Debug for BaseCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BaseCache").finish_non_exhaustive()
    }
}

/// The reader that owns an entry; a private cache uses only scope zero.
type Scope = u64;

#[derive(Default)]
struct CacheInner {
    entries: HashMap<(Scope, usize), Cached>,
    order: VecDeque<(Scope, usize)>,
    bytes: usize,
    next_scope: Scope,
}

#[derive(Clone)]
struct Cached {
    kind: ObjectKind,
    data: Arc<Vec<u8>>,
    verified: bool,
}

/// One reader's entries in a cache.
#[derive(Clone, Copy)]
pub(crate) struct CacheView<'a> {
    cache: &'a BaseCache,
    scope: Scope,
}

/// A stored pack reader's entries in the process-wide cache, released when it is dropped.
pub(crate) struct CacheScope {
    cache: &'static BaseCache,
    scope: Scope,
}

impl std::fmt::Debug for CacheScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheScope").finish_non_exhaustive()
    }
}

#[cfg(not(test))]
static SHARED: std::sync::LazyLock<BaseCache> = std::sync::LazyLock::new(BaseCache::new);

#[cfg(test)]
thread_local! {
    /// Tests count inflations per thread, so each test thread gets its own process-wide cache.
    static SHARED: &'static BaseCache = Box::leak(Box::new(BaseCache::new()));
}

impl CacheScope {
    /// Registers a new reader in the process-wide cache.
    pub(crate) fn register() -> Self {
        #[cfg(not(test))]
        let cache: &'static BaseCache = &SHARED;
        #[cfg(test)]
        let cache = SHARED.with(|cache| *cache);
        let mut inner = cache.lock();
        inner.next_scope += 1;
        let scope = inner.next_scope;
        Self { cache, scope }
    }

    pub(crate) fn view(&self) -> CacheView<'_> {
        CacheView {
            cache: self.cache,
            scope: self.scope,
        }
    }
}

impl Drop for CacheScope {
    fn drop(&mut self) {
        let scope = self.scope;
        let mut inner = self.cache.lock();
        let CacheInner {
            entries,
            order,
            bytes,
            ..
        } = &mut *inner;
        entries.retain(|&(owner, _), cached| {
            let keep = owner != scope;
            if !keep {
                *bytes -= cached.data.len();
            }
            keep
        });
        order.retain(|&(owner, _)| owner != scope);
    }
}

impl BaseCache {
    /// Total decoded bytes retained, matching Git's default `core.deltaBaseCacheLimit`.
    const LIMIT: usize = 96 * 1024 * 1024;

    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(CacheInner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheInner> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// This cache as a receive-owned source's private view.
    pub(crate) fn view(&self) -> CacheView<'_> {
        CacheView {
            cache: self,
            scope: 0,
        }
    }

    #[cfg(test)]
    fn get(&self, position: usize) -> Option<Cached> {
        self.view().get(position)
    }

    pub(crate) fn insert(&self, position: usize, object: &Object, verified: bool) {
        self.view()
            .insert_shared(position, object.kind, &object.data, verified);
    }

    #[cfg(test)]
    fn insert_shared(
        &self,
        position: usize,
        kind: ObjectKind,
        data: &Arc<Vec<u8>>,
        verified: bool,
    ) {
        self.view().insert_shared(position, kind, data, verified);
    }

    #[cfg(test)]
    fn mark_verified(&self, position: usize) {
        self.view().mark_verified(position);
    }
}

impl CacheView<'_> {
    fn get(&self, position: usize) -> Option<Cached> {
        let inner = self.cache.lock();
        inner.entries.get(&(self.scope, position)).cloned()
    }

    /// Shares a decoded payload with the cache without copying it.
    fn insert_shared(
        &self,
        position: usize,
        kind: ObjectKind,
        data: &Arc<Vec<u8>>,
        verified: bool,
    ) {
        let size = data.len();
        // One large object shouldn't evict every shared base.
        if size > BaseCache::LIMIT / 8 {
            return;
        }
        let key = (self.scope, position);
        let mut inner = self.cache.lock();
        if let Some(existing) = inner.entries.get_mut(&key) {
            existing.verified |= verified;
            return;
        }
        while inner.bytes + size > BaseCache::LIMIT {
            let Some(oldest) = inner.order.pop_front() else {
                break;
            };
            if let Some(evicted) = inner.entries.remove(&oldest) {
                inner.bytes -= evicted.data.len();
            }
        }
        inner.bytes += size;
        inner.order.push_back(key);
        inner.entries.insert(
            key,
            Cached {
                kind,
                data: Arc::clone(data),
                verified,
            },
        );
    }

    fn mark_verified(&self, position: usize) {
        let mut inner = self.cache.lock();
        if let Some(entry) = inner.entries.get_mut(&(self.scope, position)) {
            entry.verified = true;
        }
    }
}

#[cfg(test)]
impl Source for Pack {
    fn format(&self) -> crate::ObjectFormat {
        self.index.pack_hash.format()
    }
    fn entry(&self, position: usize) -> Result<super::index::Entry, Error> {
        Ok(self.index.entry(&self.index_bytes, position))
    }
    fn find(&self, id: ObjectId) -> Result<Option<usize>, Error> {
        Ok(self.find(id))
    }
    fn at_offset(&self, offset: usize) -> Result<usize, Error> {
        self.index.at_offset(offset)
    }
    fn prefix(&self, entry: &super::index::Entry) -> Result<Vec<u8>, Error> {
        Ok(self.data[entry.offset..entry.end.min(entry.offset.saturating_add(64))].to_vec())
    }
    fn inflate(
        &self,
        entry: &super::index::Entry,
        header: &EntryHeader,
        _: &AtomicBool,
    ) -> Result<Vec<u8>, Error> {
        inflate(&self.data[header.data_start..entry.end], header.length)
    }
}

pub(crate) fn check_cancelled(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

pub(crate) fn decode(
    source: &impl Source,
    position: usize,
    limits: ReadLimits,
    cancelled: &AtomicBool,
) -> Result<Object, Error> {
    let mut remaining = limits.max_decode_bytes;
    decode_budgeted(source, position, limits, cancelled, &mut remaining)
}

pub(crate) fn decode_budgeted(
    source: &impl Source,
    position: usize,
    limits: ReadLimits,
    cancelled: &AtomicBool,
    remaining: &mut usize,
) -> Result<Object, Error> {
    decode_checked(
        source,
        position,
        limits,
        cancelled,
        remaining,
        Identity::Verify,
    )
}

/// Whether a decode hashes the requested object to check the identity its index entry claims.
///
/// Every other check is unconditional: entry framing, v2 CRCs, zlib termination, declared sizes,
/// delta programs and [`ReadLimits`]. Only [`crate::UnverifiedObject`] reads pass `Skip`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Identity {
    Verify,
    Skip,
}

pub(crate) fn decode_checked(
    source: &impl Source,
    mut position: usize,
    limits: ReadLimits,
    cancelled: &AtomicBool,
    remaining: &mut usize,
    identity: Identity,
) -> Result<Object, Error> {
    let verify = identity == Identity::Verify && !source.identity_verified(position);
    let format = source.format();
    let mut input_remaining = limits.max_input_bytes;
    let mut pending = Vec::new();
    let mut seen = HashSet::new();
    let cache = source.base_cache();
    let requested = position;
    // Bases are cached for later reads. A requested blob is returned without being cached: large
    // payloads would evict bases that later reads share; measured on the large fixture, caching
    // them cost more re-inflation than it saved. Requested commits, trees and tags stay cached,
    // since walks often read their delta children next.
    let retain =
        |position: usize, kind: ObjectKind| position != requested || kind != ObjectKind::Blob;
    // Payloads are shared with the cache, never copied out of it.
    let (kind, mut data) = loop {
        if !seen.insert(position) {
            return Err(Error::DeltaCycle);
        }
        check_cancelled(cancelled)?;
        if let Some(cache) = cache
            && let Some(hit) = cache.get(position)
        {
            // A payload cached under looser limits must still fit this read's.
            if hit.data.len() > limits.max_object_bytes {
                return Err(Error::Limit("object bytes"));
            }
            if verify && position == requested && !hit.verified {
                verify_identity(format, hit.kind, &hit.data, source.entry(position)?.id)?;
                cache.mark_verified(position);
            }
            break (hit.kind, hit.data);
        }
        let entry = source.entry(position)?;
        input_remaining = input_remaining
            .checked_sub(entry.end - entry.offset)
            .ok_or(Error::Limit("compressed entry bytes"))?;
        let header = entry_header(source, &entry)?;
        let kind = header.kind;
        let length = header.length;
        let base = header.base;
        let limit = if base.is_some() {
            limits.max_delta_bytes
        } else {
            limits.max_object_bytes
        };
        if length > limit {
            return Err(Error::Limit(if base.is_some() {
                "delta program bytes"
            } else {
                "object bytes"
            }));
        }
        if base.is_some() && pending.len() >= limits.max_delta_depth {
            return Err(Error::Limit("delta depth"));
        }
        charge(remaining, length)?;
        let data = source.inflate(&entry, &header, cancelled)?;
        if let Some(base) = base {
            pending.push((position, data));
            position = base;
        } else {
            let kind = match kind {
                1 => ObjectKind::Commit,
                2 => ObjectKind::Tree,
                3 => ObjectKind::Blob,
                4 => ObjectKind::Tag,
                other => return Err(Error::ObjectType(other)),
            };
            // Only the requested object's identity is checked: a corrupt base changes every
            // object reconstructed from it, so the final check still catches it.
            let check_identity = verify && position == requested;
            let verified = check_identity || source.identity_verified(position);
            if check_identity {
                verify_identity(format, kind, &data, entry.id)?;
            }
            let data = Arc::new(data);
            if let Some(cache) = cache.filter(|_| retain(position, kind)) {
                cache.insert_shared(position, kind, &data, verified);
            }
            break (kind, data);
        }
    };
    while let Some((position, program)) = pending.pop() {
        check_cancelled(cancelled)?;
        data = Arc::new(apply(&data, &program, limits.max_object_bytes, remaining)?);
        let check_identity = verify && position == requested;
        let verified = check_identity || source.identity_verified(position);
        if check_identity {
            verify_identity(format, kind, &data, source.entry(position)?.id)?;
        }
        if let Some(cache) = cache.filter(|_| retain(position, kind)) {
            cache.insert_shared(position, kind, &data, verified);
        }
    }
    // The payload stays shared with the cache; callers copy it only to take ownership.
    Ok(Object { kind, data, format })
}

/// A pack entry's parsed header: type code, declared size, resolved base and payload offset.
pub(crate) struct EntryHeader {
    /// Pack type code: 1-4 for whole objects, 6 or 7 for deltas.
    pub kind: u8,
    /// Declared inflated size of the object or delta program.
    pub length: usize,
    /// Position of the delta base within the same pack, for delta entries.
    pub base: Option<usize>,
    /// Offset of the first zlib byte.
    pub data_start: usize,
    /// Original encoded header and base reference, used to seed the entry CRC without rereading.
    pub encoded: Vec<u8>,
    /// Entry bytes from `data_start` that the source returned with the header, if any.
    pub ahead: Vec<u8>,
}

/// Parses an entry header and resolves a delta base to a position in the same pack.
pub(crate) fn entry_header(
    source: &impl Source,
    entry: &super::index::Entry,
) -> Result<EntryHeader, Error> {
    let width = source.format().digest_len();
    let mut prefix = source.prefix(entry)?;
    let mut input = prefix.as_slice();
    let header = byte(&mut input)?;
    let kind = (header >> 4) & 7;
    let length = size(&mut input, (header & 15) as usize, 4, header & 128 != 0)?;
    let base = match kind {
        1..=4 => None,
        6 => {
            let mut next = byte(&mut input)?;
            let mut distance = (next & 127) as usize;
            while next & 128 != 0 {
                next = byte(&mut input)?;
                distance = distance
                    .checked_add(1)
                    .and_then(|n| n.checked_mul(128))
                    .and_then(|n| n.checked_add((next & 127) as usize))
                    .ok_or(Error::Corrupt("delta offset overflow"))?;
            }
            if distance == 0 {
                return Err(Error::Corrupt("zero delta distance"));
            }
            let offset = entry
                .offset
                .checked_sub(distance)
                .ok_or(Error::Corrupt("delta offset before pack"))?;
            Some(source.at_offset(offset)?)
        }
        7 => {
            let (raw, rest) = input
                .split_at_checked(width)
                .ok_or(Error::Corrupt("truncated base identity"))?;
            let id = ObjectId::from_bytes(source.format(), raw)
                .map_err(|_| Error::Corrupt("truncated base identity"))?;
            input = rest;
            Some(source.find(id)?.ok_or(Error::MissingBase(id))?)
        }
        other => return Err(Error::ObjectType(other)),
    };
    let header_len = prefix.len() - input.len();
    let ahead = prefix.split_off(header_len);
    Ok(EntryHeader {
        kind,
        length,
        base,
        data_start: entry.offset + header_len,
        encoded: prefix,
        ahead,
    })
}

fn verify_identity(
    format: crate::ObjectFormat,
    kind: ObjectKind,
    data: &[u8],
    expected: ObjectId,
) -> Result<(), Error> {
    if format.hash_object(kind, data) != expected {
        return Err(Error::Corrupt("object identity"));
    }
    Ok(())
}

#[cfg(test)]
fn inflate(input: &[u8], expected: usize) -> Result<Vec<u8>, Error> {
    let (data, consumed) = inflate_prefix(input, expected)?;
    if consumed != input.len() {
        return Err(Error::Corrupt("packed entry length or trailing data"));
    }
    Ok(data)
}

#[cfg(test)]
pub(crate) fn inflate_prefix(mut input: &[u8], expected: usize) -> Result<(Vec<u8>, usize), Error> {
    let mut inflater = Decompress::new(true);
    let mut output = [0; 8192];
    let mut data = Vec::new();
    loop {
        let before_in = inflater.total_in();
        let before_out = inflater.total_out();
        let status = inflater
            .decompress(input, &mut output, FlushDecompress::None)
            .map_err(|_| Error::Corrupt("invalid packed zlib stream"))?;
        let consumed = (inflater.total_in() - before_in) as usize;
        let produced = (inflater.total_out() - before_out) as usize;
        input = &input[consumed..];
        if produced > expected.saturating_sub(data.len()) {
            return Err(Error::Corrupt("inflated entry exceeds declared size"));
        }
        data.extend_from_slice(&output[..produced]);
        if status == Status::StreamEnd {
            if data.len() != expected {
                return Err(Error::Corrupt("packed entry length or trailing data"));
            }
            return Ok((data, inflater.total_in() as usize));
        }
        if produced == 0 && consumed == 0 {
            return Err(Error::Corrupt("truncated packed zlib stream"));
        }
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    fn object(data: Vec<u8>) -> Object {
        Object {
            kind: ObjectKind::Blob,
            format: crate::ObjectFormat::Sha1,
            data: Arc::new(data),
        }
    }

    /// Eviction drops only the cache's share: payloads held by callers stay intact, and taking
    /// one copies rather than aliasing another holder.
    #[test]
    fn evicted_payloads_stay_owned_by_their_holders() {
        let cache = BaseCache::new();
        let held = object(vec![7; 1024]);
        let clone = held.clone();
        cache.insert(0, &held, true);
        let filler = Arc::new(vec![0; BaseCache::LIMIT / 8]);
        for position in 1..=8 {
            cache.insert_shared(position, ObjectKind::Blob, &filler, true);
        }
        assert!(cache.get(0).is_none(), "the oldest entry is evicted");
        let inner = cache.inner.lock().unwrap();
        assert!(inner.bytes <= BaseCache::LIMIT);
        assert_eq!(
            inner.bytes,
            inner.entries.values().map(|e| e.data.len()).sum::<usize>()
        );
        drop(inner);
        let mut taken = held.into_data();
        taken.fill(0);
        assert_eq!(clone.data(), &[7; 1024][..]);
        assert_eq!(clone.into_data(), vec![7; 1024]);
    }

    /// Pack readers share one byte limit but never each other's entries, and dropping a
    /// reader releases its entries.
    #[test]
    fn readers_share_the_limit_but_not_entries() {
        let first = CacheScope::register();
        let second = CacheScope::register();
        let data = Arc::new(b"cached".to_vec());
        first.view().insert_shared(3, ObjectKind::Blob, &data, true);
        assert!(first.view().get(3).is_some_and(|hit| hit.verified));
        assert!(second.view().get(3).is_none(), "another reader misses");
        let filler = Arc::new(vec![0; BaseCache::LIMIT / 8]);
        for position in 0..8 {
            second
                .view()
                .insert_shared(position, ObjectKind::Blob, &filler, true);
        }
        assert!(
            first.view().get(3).is_none(),
            "one limit evicts across readers"
        );
        let cache = first.cache;
        drop(second);
        let inner = cache.lock();
        assert_eq!(inner.bytes, 0);
        assert!(inner.entries.is_empty());
    }

    /// Payloads up to an eighth of the limit are admitted; larger ones are not.
    #[test]
    fn admits_payloads_up_to_an_eighth_of_the_limit() {
        let cache = BaseCache::new();
        let largest = Arc::new(vec![0; BaseCache::LIMIT / 8]);
        let oversized = Arc::new(vec![0; BaseCache::LIMIT / 8 + 1]);
        cache.insert_shared(0, ObjectKind::Blob, &largest, false);
        cache.insert_shared(1, ObjectKind::Blob, &oversized, false);
        assert!(cache.get(0).is_some_and(|hit| !hit.verified));
        assert!(cache.get(1).is_none());
        cache.mark_verified(0);
        assert!(cache.get(0).is_some_and(|hit| hit.verified));
    }
}
