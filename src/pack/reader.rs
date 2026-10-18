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
use super::index::Span;
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
        index.validate(index_bytes)?;
        verify_hash(format, index_bytes, "index checksum")?;
        if word(&data, 8)? as usize != index.len() {
            return Err(Error::Corrupt("pack object count"));
        }
        verify_hash(format, &data, "pack checksum")?;
        if *index.pack_hash.as_bytes() != data[end..] {
            return Err(Error::Corrupt("index/pack checksum disagreement"));
        }
        for position in 0..index.len() {
            let entry = index.entry(index_bytes, position)?;
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
        self.index.find(&self.index_bytes, id).ok().flatten()
    }

    pub fn read(&self, position: usize, limits: ReadLimits) -> Result<Object, Error> {
        decode(self, position, limits, &AtomicBool::new(false))
    }
}

pub(crate) trait Source {
    fn format(&self) -> crate::ObjectFormat;
    /// The entry at identity-order `position`.
    fn span(&self, position: usize) -> Result<Span, Error>;
    /// The entry starting at `offset`, the base of an offset delta.
    fn span_at(&self, offset: usize) -> Result<Span, Error>;
    fn identity(&self, position: usize) -> Result<ObjectId, Error>;
    fn find(&self, id: ObjectId) -> Result<Option<usize>, Error>;
    fn prefix(&self, span: &Span) -> Result<Vec<u8>, Error>;
    /// Inflates an entry's payload, returning it with the offset where the entry ends.
    ///
    /// An exact span must end where the zlib stream does; otherwise the stream's end defines the
    /// entry's. The CRC, when the span has one, covers exactly the entry's bytes.
    fn inflate(
        &self,
        span: &Span,
        header: &EntryHeader,
        cancelled: &AtomicBool,
    ) -> Result<(Vec<u8>, usize), Error>;
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

/// Recently decoded pack entries, keyed by scope and entry offset, like Git's delta base cache.
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
///
/// Entries are split by offset across shards, each with its own lock and an equal share of the
/// limit, so threads reading one pack in parallel rarely wait for each other. Each shard evicts
/// its own oldest entries.
pub(crate) struct BaseCache {
    shards: Box<[Mutex<CacheInner>]>,
    /// Decoded bytes each shard retains before its oldest entries are evicted.
    shard_limit: usize,
    next_scope: std::sync::atomic::AtomicU64,
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
}

#[derive(Clone)]
struct Cached {
    kind: ObjectKind,
    data: Arc<Vec<u8>>,
    proof: Proof,
    cost: Cost,
}

/// What decoding a cached entry from its pack cost a read's limits, from the entry down to its
/// chain's whole base.
///
/// A stored pack reader's hit charges this to the reading request, so a read fits its
/// [`ReadLimits`] or not whatever the cache holds: reads in another order, or on other threads,
/// reach the same outcome.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Cost {
    /// Delta edges from the entry to its whole base.
    pub depth: usize,
    /// Compressed entry bytes read.
    pub input: usize,
    /// Inflated and reconstructed bytes.
    pub decode: usize,
}

impl Cost {
    /// The cost of an entry decoded on top of `base`.
    fn above(base: Self, delta: bool, input: usize, decode: usize) -> Self {
        Self {
            depth: base.depth + usize::from(delta),
            input: base.input.saturating_add(input),
            decode: base.decode.saturating_add(decode),
        }
    }
}

/// What a cached payload was checked against when it was decoded.
///
/// Entries are keyed by offset, and damaged index offsets can make two identities name one
/// entry, so an identity check proves only the identity it hashed. A base reached by offset has
/// no index position, so its CRC may be unchecked until a read by identity checks it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Proof {
    /// The identity the payload hashed to, if checked.
    pub identity: Option<ObjectId>,
    /// Whether the entry's CRC was checked, or it has none.
    pub crc: bool,
}

impl Proof {
    fn merge(&mut self, other: Self) {
        self.identity = self.identity.or(other.identity);
        self.crc |= other.crc;
    }
}

/// One reader's entries in a cache.
#[derive(Clone, Copy)]
pub(crate) struct CacheView<'a> {
    cache: &'a BaseCache,
    scope: Scope,
    /// Whether a hit charges the read its entry's [`Cost`]. A receive-owned source's reads share
    /// one budget that already paid for each entry once, so its hits charge nothing.
    charged: bool,
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
        let scope = cache.next_scope.fetch_add(1, Ordering::Relaxed) + 1;
        Self { cache, scope }
    }

    pub(crate) fn view(&self) -> CacheView<'_> {
        CacheView {
            cache: self.cache,
            scope: self.scope,
            charged: true,
        }
    }
}

impl Drop for CacheScope {
    fn drop(&mut self) {
        let scope = self.scope;
        for shard in &self.cache.shards {
            let mut inner = lock(shard);
            let CacheInner {
                entries,
                order,
                bytes,
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
}

impl BaseCache {
    /// Total decoded bytes retained, matching Git's default `core.deltaBaseCacheLimit`.
    pub(crate) const LIMIT: usize = 96 * 1024 * 1024;

    /// Shards of a cache shared between threads. Each holds an eighth of the limit, the largest
    /// payload any cache admits, so sharding admits the same payloads.
    const SHARDS: usize = 8;

    pub(crate) fn new() -> Self {
        Self::sharded(Self::SHARDS, Self::LIMIT / Self::SHARDS)
    }

    /// An unsharded private cache retaining at most `limit` decoded bytes, for one worker that
    /// shares the process budget with others.
    #[cfg(any(unix, test))]
    pub(crate) fn with_limit(limit: usize) -> Self {
        Self::sharded(1, limit)
    }

    fn sharded(shards: usize, shard_limit: usize) -> Self {
        Self {
            shards: (0..shards).map(|_| Mutex::default()).collect(),
            shard_limit,
            next_scope: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Payloads larger than this are never cached, so one large object can't evict every base.
    fn max_payload(&self) -> usize {
        (self.shard_limit * self.shards.len() / 8).min(self.shard_limit)
    }

    fn shard(&self, offset: usize) -> &Mutex<CacheInner> {
        // Entry offsets are arbitrary byte positions; multiplying spreads neighbours evenly.
        let mixed = (offset as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 32;
        let index = usize::try_from(mixed).unwrap_or(0) % self.shards.len();
        #[expect(clippy::indexing_slicing, reason = "index is below the shard count")]
        &self.shards[index]
    }

    /// This cache as a receive-owned source's private view.
    pub(crate) fn view(&self) -> CacheView<'_> {
        CacheView {
            cache: self,
            scope: 0,
            charged: false,
        }
    }

    /// This cache as one stored-pack reader's private view, whose hits charge their [`Cost`] as
    /// the shared cache's do.
    #[cfg(any(unix, test))]
    pub(crate) fn charged_view(&self) -> CacheView<'_> {
        CacheView {
            charged: true,
            ..self.view()
        }
    }

    #[cfg(test)]
    fn get(&self, offset: usize) -> Option<Cached> {
        self.view().get(offset)
    }

    /// Caches an object a receive-owned source resolved.
    pub(crate) fn insert(&self, offset: usize, object: &Object, proof: Proof) {
        self.view()
            .insert_shared(offset, object.kind, &object.data, proof, Cost::default());
    }

    #[cfg(test)]
    fn insert_shared(&self, offset: usize, kind: ObjectKind, data: &Arc<Vec<u8>>, proof: Proof) {
        self.view()
            .insert_shared(offset, kind, data, proof, Cost::default());
    }

    #[cfg(test)]
    fn mark_verified(&self, offset: usize, id: ObjectId) {
        self.view().mark_verified(offset, id);
    }
}

impl CacheView<'_> {
    fn get(&self, offset: usize) -> Option<Cached> {
        let inner = lock(self.cache.shard(offset));
        inner.entries.get(&(self.scope, offset)).cloned()
    }

    /// Shares a decoded payload with the cache without copying it.
    fn insert_shared(
        &self,
        offset: usize,
        kind: ObjectKind,
        data: &Arc<Vec<u8>>,
        proof: Proof,
        cost: Cost,
    ) {
        let size = data.len();
        // One large object shouldn't evict every shared base.
        if size > self.cache.max_payload() {
            return;
        }
        let key = (self.scope, offset);
        let mut inner = lock(self.cache.shard(offset));
        if let Some(existing) = inner.entries.get_mut(&key) {
            existing.proof.merge(proof);
            return;
        }
        while inner.bytes + size > self.cache.shard_limit {
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
                proof,
                cost,
            },
        );
    }

    fn mark_verified(&self, offset: usize, id: ObjectId) {
        let mut inner = lock(self.cache.shard(offset));
        if let Some(entry) = inner.entries.get_mut(&(self.scope, offset)) {
            entry.proof.identity = Some(id);
        }
    }
}

fn lock(shard: &Mutex<CacheInner>) -> std::sync::MutexGuard<'_, CacheInner> {
    shard.lock().unwrap_or_else(|error| error.into_inner())
}

#[cfg(test)]
impl Source for Pack {
    fn format(&self) -> crate::ObjectFormat {
        self.index.pack_hash.format()
    }
    // Opening validated the whole index, so its offset order exists and every span is exact.
    fn span(&self, position: usize) -> Result<Span, Error> {
        self.index.span(&self.index_bytes, position)
    }
    fn span_at(&self, offset: usize) -> Result<Span, Error> {
        self.index.span_at(&self.index_bytes, offset)
    }
    fn identity(&self, position: usize) -> Result<ObjectId, Error> {
        Ok(self.index.identity(&self.index_bytes, position))
    }
    fn find(&self, id: ObjectId) -> Result<Option<usize>, Error> {
        self.index.find(&self.index_bytes, id)
    }
    fn prefix(&self, span: &Span) -> Result<Vec<u8>, Error> {
        Ok(self.data[span.offset..span.end.min(span.offset.saturating_add(64))].to_vec())
    }
    fn inflate(
        &self,
        span: &Span,
        header: &EntryHeader,
        _: &AtomicBool,
    ) -> Result<(Vec<u8>, usize), Error> {
        let data = inflate(&self.data[header.data_start..span.end], header.length)?;
        Ok((data, span.end))
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
/// Every other check is unconditional: entry framing, zlib termination, declared sizes, delta
/// programs, [`ReadLimits`], and the v2 CRC of every entry whose index position the read knows.
/// That excludes an offset delta's base until the index's offset order exists, as Git checks no
/// CRC on reads. Only [`crate::UnverifiedObject`] reads pass `Skip`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Identity {
    Verify,
    Skip,
}

pub(crate) fn decode_checked(
    source: &impl Source,
    position: usize,
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
    let mut span = source.span(position)?;
    // Entries are keyed by offset: a delta names its base by offset, and finding the base's
    // index position would need the offset order.
    let requested = span.offset;
    // A receive-owned source can vouch for an entry's identity; the CRC is checked when known.
    let proof_at = |span: &Span| -> Result<Proof, Error> {
        let identity = match span.position {
            Some(position) if source.identity_verified(position) => {
                Some(source.identity(position)?)
            }
            _ => None,
        };
        // Reading an entry with an index position checks its CRC, if it has one.
        Ok(Proof {
            identity,
            crc: span.position.is_some(),
        })
    };
    // Bases are cached for later reads. A requested blob is returned without being cached: large
    // payloads would evict bases that later reads share; measured on the large fixture, caching
    // them cost more re-inflation than it saved. Requested commits, trees and tags stay cached,
    // since walks often read their delta children next.
    let retain = |offset: usize, kind: ObjectKind| offset != requested || kind != ObjectKind::Blob;
    // Payloads are shared with the cache, never copied out of it.
    let (kind, mut data, mut cost) = loop {
        let offset = span.offset;
        if seen.contains(&offset) {
            return Err(Error::DeltaCycle);
        }
        check_cancelled(cancelled)?;
        // The requested entry's CRC is always checked: a base cached without one is decoded again.
        if let Some(cache) = cache
            && let Some(hit) = cache
                .get(offset)
                .filter(|hit| offset != requested || hit.proof.crc || span.crc.is_none())
        {
            // A payload cached under looser limits must still fit this read's, and the hit
            // charges what decoding it from the pack would have.
            if hit.data.len() > limits.max_object_bytes {
                return Err(Error::Limit("object bytes"));
            }
            if cache.charged {
                if pending.len() + hit.cost.depth > limits.max_delta_depth {
                    return Err(Error::Limit("delta depth"));
                }
                if hit.cost.input > input_remaining {
                    return Err(Error::Limit("compressed entry bytes"));
                }
                charge(remaining, hit.cost.decode)?;
            }
            if verify && offset == requested {
                let id = source.identity(position)?;
                if hit.proof.identity != Some(id) {
                    verify_identity(format, hit.kind, &hit.data, id)?;
                    cache.mark_verified(offset, id);
                }
            }
            break (hit.kind, hit.data, hit.cost);
        }
        // An entry without an exact end ends with its zlib stream; a stream that runs past the
        // remaining input budget exceeds the limit.
        let capped = !span.exact && span.end - offset > input_remaining;
        if span.exact {
            input_remaining = input_remaining
                .checked_sub(span.end - offset)
                .ok_or(Error::Limit("compressed entry bytes"))?;
        } else if capped {
            span.end = offset + input_remaining;
        }
        let over_budget = |error| match error {
            Error::Corrupt(
                "truncated packed zlib stream"
                | "truncated delta or entry"
                | "truncated base identity",
            ) if capped => Error::Limit("compressed entry bytes"),
            other => other,
        };
        let header = entry_header(source, &span).map_err(over_budget)?;
        let kind = header.kind;
        let length = header.length;
        let delta = header.base.is_some();
        let limit = if delta {
            limits.max_delta_bytes
        } else {
            limits.max_object_bytes
        };
        if length > limit {
            return Err(Error::Limit(if delta {
                "delta program bytes"
            } else {
                "object bytes"
            }));
        }
        if delta {
            if pending.len() >= limits.max_delta_depth {
                return Err(Error::Limit("delta depth"));
            }
            // Only delta nodes can be revisited: whole objects and cache hits terminate the
            // descent. Ordinary small-object reads need no cycle-set allocation.
            seen.insert(offset);
        }
        charge(remaining, length)?;
        let (data, end) = source
            .inflate(&span, &header, cancelled)
            .map_err(over_budget)?;
        if !span.exact {
            // The stream ended within the span, which the budget capped.
            input_remaining -= end - offset;
        }
        // An exact span ends where its stream does, so this is what either branch charged.
        let consumed = end - offset;
        if let Some(base) = header.base {
            pending.push((offset, proof_at(&span)?, data, consumed, length));
            span = base;
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
            let mut proof = proof_at(&span)?;
            if verify && offset == requested {
                let id = source.identity(position)?;
                verify_identity(format, kind, &data, id)?;
                proof.identity = Some(id);
            }
            let data = Arc::new(data);
            let cost = Cost::above(Cost::default(), false, consumed, length);
            if let Some(cache) = cache.filter(|_| retain(offset, kind)) {
                cache.insert_shared(offset, kind, &data, proof, cost);
            }
            break (kind, data, cost);
        }
    };
    while let Some((offset, mut proof, program, consumed, length)) = pending.pop() {
        check_cancelled(cancelled)?;
        data = Arc::new(apply(&data, &program, limits.max_object_bytes, remaining)?);
        cost = Cost::above(cost, true, consumed, length + data.len());
        if verify && offset == requested {
            let id = source.identity(position)?;
            verify_identity(format, kind, &data, id)?;
            proof.identity = Some(id);
        }
        if let Some(cache) = cache.filter(|_| retain(offset, kind)) {
            cache.insert_shared(offset, kind, &data, proof, cost);
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
    /// The delta base within the same pack, for delta entries.
    pub base: Option<Span>,
    /// Offset of the first zlib byte.
    pub data_start: usize,
    /// Header and read-ahead payload share the source's allocation, without splitting/copying it.
    prefix: Vec<u8>,
    header_len: usize,
}

impl EntryHeader {
    pub(crate) fn encoded(&self) -> &[u8] {
        self.prefix.get(..self.header_len).unwrap_or_default()
    }

    pub(crate) fn ahead(&self) -> &[u8] {
        self.prefix.get(self.header_len..).unwrap_or_default()
    }
}

/// Parses an entry header and resolves a delta base to an entry in the same pack.
pub(crate) fn entry_header(source: &impl Source, entry: &Span) -> Result<EntryHeader, Error> {
    let width = source.format().digest_len();
    let prefix = source.prefix(entry)?;
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
            Some(source.span_at(offset)?)
        }
        7 => {
            let (raw, rest) = input
                .split_at_checked(width)
                .ok_or(Error::Corrupt("truncated base identity"))?;
            let id = ObjectId::from_bytes(source.format(), raw)
                .map_err(|_| Error::Corrupt("truncated base identity"))?;
            input = rest;
            let position = source.find(id)?.ok_or(Error::MissingBase(id))?;
            Some(source.span(position)?)
        }
        other => return Err(Error::ObjectType(other)),
    };
    let header_len = prefix.len() - input.len();
    Ok(EntryHeader {
        kind,
        length,
        base,
        data_start: entry.offset + header_len,
        prefix,
        header_len,
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

    const CHECKED: Proof = Proof {
        identity: None,
        crc: true,
    };

    fn object(data: Vec<u8>) -> Object {
        Object {
            kind: ObjectKind::Blob,
            format: crate::ObjectFormat::Sha1,
            data: Arc::new(data),
        }
    }

    /// Offsets other than 0 and 3 that between them reach every shard of `cache`.
    fn fill_every_shard(cache: &BaseCache) -> Vec<usize> {
        let mut reached = vec![false; cache.shards.len()];
        let mut offsets = Vec::new();
        for offset in 4..4096 {
            let shard = cache.shard(offset);
            let index = cache
                .shards
                .iter()
                .position(|candidate| std::ptr::eq(candidate, shard))
                .unwrap();
            if !reached[index] {
                reached[index] = true;
                offsets.push(offset);
            }
        }
        assert!(
            reached.iter().all(|&reached| reached),
            "every shard is reachable"
        );
        offsets
    }

    /// Threads reading at different offsets take different shard locks.
    #[test]
    fn neighbouring_offsets_spread_across_shards() {
        let cache = BaseCache::new();
        let used: std::collections::HashSet<_> = (0..64)
            .map(|entry| std::ptr::from_ref(cache.shard(12 + entry * 200)))
            .collect();
        assert_eq!(used.len(), BaseCache::SHARDS);
    }

    /// Eviction drops only the cache's share: payloads held by callers stay intact, and taking
    /// one copies rather than aliasing another holder.
    #[test]
    fn evicted_payloads_stay_owned_by_their_holders() {
        let cache = BaseCache::new();
        let held = object(vec![7; 1024]);
        let clone = held.clone();
        cache.insert(0, &held, CHECKED);
        let filler = Arc::new(vec![0; BaseCache::LIMIT / 8]);
        for position in fill_every_shard(&cache) {
            cache.insert_shared(position, ObjectKind::Blob, &filler, CHECKED);
        }
        assert!(cache.get(0).is_none(), "the oldest entry is evicted");
        let mut total = 0;
        for shard in &cache.shards {
            let inner = shard.lock().unwrap();
            assert!(inner.bytes <= cache.shard_limit);
            assert_eq!(
                inner.bytes,
                inner.entries.values().map(|e| e.data.len()).sum::<usize>()
            );
            total += inner.bytes;
        }
        assert!(total <= BaseCache::LIMIT);
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
        first
            .view()
            .insert_shared(3, ObjectKind::Blob, &data, CHECKED, Cost::default());
        assert!(first.view().get(3).is_some_and(|hit| hit.proof.crc));
        assert!(second.view().get(3).is_none(), "another reader misses");
        let filler = Arc::new(vec![0; BaseCache::LIMIT / 8]);
        for position in fill_every_shard(first.cache) {
            second.view().insert_shared(
                position,
                ObjectKind::Blob,
                &filler,
                CHECKED,
                Cost::default(),
            );
        }
        assert!(
            first.view().get(3).is_none(),
            "one limit evicts across readers"
        );
        let cache = first.cache;
        drop(second);
        for shard in &cache.shards {
            let inner = lock(shard);
            assert_eq!(inner.bytes, 0);
            assert!(inner.entries.is_empty());
        }
    }

    /// Payloads up to an eighth of the limit are admitted; larger ones are not.
    #[test]
    fn admits_payloads_up_to_an_eighth_of_the_limit() {
        let cache = BaseCache::new();
        let largest = Arc::new(vec![0; BaseCache::LIMIT / 8]);
        let oversized = Arc::new(vec![0; BaseCache::LIMIT / 8 + 1]);
        cache.insert_shared(0, ObjectKind::Blob, &largest, Proof::default());
        cache.insert_shared(1, ObjectKind::Blob, &oversized, Proof::default());
        assert!(cache.get(0).is_some_and(|hit| hit.proof.identity.is_none()));
        assert!(cache.get(1).is_none());
        let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"");
        cache.mark_verified(0, id);
        assert!(
            cache
                .get(0)
                .is_some_and(|hit| hit.proof.identity == Some(id))
        );
    }
}
