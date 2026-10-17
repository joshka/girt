//! Index received packs on disk, retaining metadata and a bounded decoded delta-base cache.
mod finish;

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use flate2::{Decompress, FlushDecompress, Status};

use super::bytes::Bytes;
use super::known::KnownObjects;
use super::progress::ValidationObserver;
use super::{FetchError, FetchLimits, check_cancelled};
use crate::object::Hasher;
use crate::pack::delta::{apply, byte, charge, size};
use crate::pack::index::{Span, word};
use crate::pack::reader::{BaseCache, Proof, Source, decode_budgeted};

/// Receive checks each entry's CRC; identities are proven as entries resolve.
const RECEIVED: Proof = Proof {
    identity: None,
    crc: true,
};
use crate::pack::write::{Entry, encode_index};
use crate::{IdMap, Object, ObjectId, ObjectKind, ObjectReadError as Error};

/// Verifies a self-contained pack exactly as a received one and returns its index and checksum.
pub(crate) fn index_self_contained(
    format: crate::ObjectFormat,
    pack: Bytes,
    limits: FetchLimits,
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, ObjectId), FetchError> {
    let imported = Imported::read_owned(
        format,
        pack,
        &IdMap::default(),
        limits,
        &mut ValidationObserver::new(cancel, &mut |_| {}),
    )?;
    Ok((imported.index, imported.checksum))
}

pub(crate) struct Imported {
    pub objects: IndexedObjects,
    pub index: Vec<u8>,
    pub checksum: ObjectId,
}

impl Imported {
    #[cfg(test)]
    pub fn read(
        format: crate::ObjectFormat,
        data: &[u8],
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, FetchError> {
        Self::read_with_known(format, data, &IdMap::default(), limits, cancel)
    }

    #[cfg(test)]
    pub fn read_with_known(
        format: crate::ObjectFormat,
        data: &[u8],
        known: &(impl KnownObjects + ?Sized),
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, FetchError> {
        Self::read_observed(
            format,
            data,
            known,
            limits,
            &mut ValidationObserver::new(cancel, &mut |_| {}),
        )
    }

    #[cfg(test)]
    pub fn read_observed(
        format: crate::ObjectFormat,
        data: &[u8],
        known: &(impl KnownObjects + ?Sized),
        limits: FetchLimits,
        observer: &mut ValidationObserver<'_>,
    ) -> Result<Self, FetchError> {
        let mut pack = Bytes::temporary()?;
        pack.append(data)?;
        Self::read_owned(format, pack, known, limits, observer)
    }

    pub fn read_owned(
        format: crate::ObjectFormat,
        pack: Bytes,
        known: &(impl KnownObjects + ?Sized),
        limits: FetchLimits,
        observer: &mut ValidationObserver<'_>,
    ) -> Result<Self, FetchError> {
        let cancel = observer.cancel;
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(target: "girt", "fetch.import", object_format = %format,
            outcome = "incomplete", failure_class = tracing::field::Empty,
            effects = tracing::field::Empty, pack_bytes = pack.len(),
            decoded_bytes = tracing::field::Empty, objects = tracing::field::Empty);
        let operation = || {
            check_cancelled(cancel)?;
            if pack.len() > limits.max_pack_bytes {
                return Err(FetchError::Limit("pack bytes"));
            }
            let Bytes::File(temporary, len) = pack else {
                unreachable!("received packs are spooled before indexing")
            };
            if len < 12 + format.digest_len() {
                return Err(Error::Corrupt("pack header").into());
            }
            let path = temporary.path().to_owned();
            let mut reader = BufReader::with_capacity(64 * 1024, temporary.reopen()?);
            let mut header = [0; 12];
            reader.read_exact(&mut header)?;
            if &header[..4] != b"PACK" {
                return Err(Error::Corrupt("pack header").into());
            }
            let version = word(&header, 4)?;
            if version != 2 {
                return Err(Error::PackVersion(version).into());
            }
            let count = word(&header, 8)? as usize;
            if count > limits.max_objects {
                return Err(FetchError::Limit("pack objects"));
            }
            // Verify the complete wire artifact before trusting any entry. Reads remain bounded.
            reader.seek(SeekFrom::Start(0))?;
            let end = len - format.digest_len();
            let mut hash = Hasher::new(format);
            let mut left = end;
            let mut buffer = [0; 64 * 1024];
            while left != 0 {
                check_cancelled(cancel)?;
                let n = left.min(buffer.len());
                reader.read_exact(&mut buffer[..n])?;
                hash.update(&buffer[..n]);
                left -= n;
            }
            let mut trailer = vec![0; format.digest_len()];
            reader.read_exact(&mut trailer)?;
            let checksum = hash.finalize();
            if checksum.as_bytes() != trailer {
                return Err(Error::Corrupt("received pack checksum").into());
            }
            reader.seek(SeekFrom::Start(12))?;
            if count > (end - 12) / 3 {
                return Err(Error::Corrupt("pack object count").into());
            }
            let mut objects = IndexedObjects {
                pack: Bytes::File(temporary, len),
                file: reader.get_ref().try_clone()?,
                format,
                entries: Vec::with_capacity(count),
                identities: IdMap::default(),
                verified: super::verified::VerifiedObjects::default(),
                cache: BaseCache::new(),
                count,
                limits,
                remaining: Mutex::new(0),
            };
            finish::with_finisher(count, limits.max_threads, |finisher| {
                let mut remaining = limits.max_decode_bytes;
                let mut offset = 12;
                observer.decoding(count);
                for _ in 0..count {
                    check_cancelled(cancel)?;
                    let mut crc = crc32fast::Hasher::new();
                    let (base, length, prefix) =
                        read_header(format, &mut reader, offset, &mut crc)?;
                    let maximum = if matches!(base, Base::Kind(_)) {
                        limits.max_object_bytes
                    } else {
                        limits.max_delta_bytes
                    };
                    if length > maximum {
                        return Err(FetchError::Limit("inflated entry bytes"));
                    }
                    charge(&mut remaining, length)?;
                    let (payload, consumed) =
                        inflate_stream(&mut reader, length, &mut crc, cancel, &path)
                            .map_err(fetch_read_error)?;
                    let entry_end = offset + prefix + consumed;
                    if entry_end > end {
                        return Err(Error::Corrupt("trailing pack entries").into());
                    }
                    let position = objects.entries.len();
                    let object = if let Base::Kind(kind) = base {
                        let object = Object {
                            format,
                            kind,
                            data: payload.into(),
                        };
                        objects.cache.insert(offset, &object, RECEIVED);
                        Some(object)
                    } else {
                        None
                    };
                    objects.entries.push(ReceivedEntry {
                        base,
                        offset,
                        start: offset + prefix,
                        end: entry_end,
                        length,
                        crc: crc.finalize(),
                        resolved: object
                            .as_ref()
                            .map(|object| (ObjectId::null(format), 0, object.kind())),
                    });
                    if let Some(object) = object {
                        finisher.submit(&mut objects, position, object, cancel)?;
                    }
                    offset = entry_end;
                    observer.decoded();
                }
                if offset != end {
                    return Err(Error::Corrupt("trailing pack entries").into());
                }
                let deltas = objects
                    .entries
                    .iter()
                    .filter(|e| e.resolved.is_none())
                    .count();
                observer.resolving(deltas);
                let external = objects.resolve(known, &mut remaining, observer, finisher)?;
                finisher.finish(&mut objects)?;
                let checksum = if external {
                    objects.finish_thin(cancel)?
                } else {
                    checksum
                };
                objects.record_read_costs()?;
                let index = objects.index(checksum)?;
                objects.remaining = Mutex::new(remaining);
                #[cfg(feature = "tracing")]
                span.record("decoded_bytes", limits.max_decode_bytes - remaining)
                    .record("objects", count);
                Ok(Self {
                    objects,
                    index,
                    checksum,
                })
            })
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

struct ReceivedEntry {
    base: Base,
    offset: usize,
    start: usize,
    end: usize,
    length: usize,
    crc: u32,
    resolved: Option<(ObjectId, usize, ObjectKind)>,
}

#[derive(Clone, Copy)]
enum Base {
    Kind(ObjectKind),
    Offset(usize),
    Id(ObjectId),
}

/// Identity and offset tables stay resident; payloads are decoded on demand from the owned file.
pub(crate) struct IndexedObjects {
    pub pack: Bytes,
    /// Read with positioned reads only, so it shares no cursor with appends to `pack`.
    file: File,
    format: crate::ObjectFormat,
    entries: Vec<ReceivedEntry>,
    identities: IdMap<usize>,
    pub verified: super::verified::VerifiedObjects,
    cache: BaseCache,
    count: usize,
    limits: FetchLimits,
    remaining: Mutex<usize>,
}

impl IndexedObjects {
    pub(super) fn record_read_costs(&mut self) -> Result<(), FetchError> {
        let mut positions: Vec<_> = (0..self.entries.len()).collect();
        positions.sort_unstable_by_key(|&p| self.entries[p].resolved.unwrap().1);
        for position in positions {
            let entry = &self.entries[position];
            let (id, depth, _) = entry.resolved.unwrap();
            let base = match entry.base {
                Base::Kind(_) => None,
                Base::Offset(offset) => Some(
                    self.entries
                        .binary_search_by_key(&offset, |e| e.offset)
                        .unwrap(),
                ),
                Base::Id(id) => Some(self.identities[&id]),
            };
            let mut read = crate::ReadLimits {
                max_object_bytes: self.verified.objects[&id].bytes,
                max_delta_bytes: 0,
                max_delta_depth: depth,
                max_decode_bytes: self.verified.objects[&id].bytes,
                max_input_bytes: 0,
            };
            read.max_input_bytes = entry.end - entry.offset;
            if let Some(base) = base {
                let base = self.verified.objects[&self.entries[base].resolved.unwrap().0].read;
                read.max_object_bytes = read.max_object_bytes.max(base.max_object_bytes);
                read.max_delta_bytes = entry.length.max(base.max_delta_bytes);
                read.max_decode_bytes = read
                    .max_decode_bytes
                    .checked_add(entry.length)
                    .and_then(|n| n.checked_add(base.max_decode_bytes))
                    .ok_or(FetchError::Limit("decoded bytes"))?;
                read.max_input_bytes = read
                    .max_input_bytes
                    .checked_add(base.max_input_bytes)
                    .ok_or(FetchError::Limit("pack bytes"))?;
            }
            self.verified.objects.get_mut(&id).unwrap().read = read;
        }
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.count
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub(crate) fn contains_key(&self, id: &ObjectId) -> bool {
        self.identities.get(id).is_some_and(|&p| p < self.count)
    }
    pub(crate) fn get(
        &self,
        id: &ObjectId,
        cancel: &AtomicBool,
    ) -> Result<Option<Object>, FetchError> {
        self.identities
            .get(id)
            .filter(|&&p| p < self.count)
            .map(|&p| self.read(p, cancel))
            .transpose()
    }
    pub(crate) fn kind(&self, id: ObjectId) -> Option<ObjectKind> {
        self.identities
            .get(&id)
            .filter(|&&p| p < self.count)
            .map(|&p| self.entries[p].resolved.unwrap().2)
    }
    fn read(&self, position: usize, cancel: &AtomicBool) -> Result<Object, FetchError> {
        let mut remaining = self.remaining.lock().unwrap_or_else(|e| e.into_inner());
        self.read_budgeted(position, cancel, &mut remaining)
    }

    fn read_budgeted(
        &self,
        position: usize,
        cancel: &AtomicBool,
        remaining: &mut usize,
    ) -> Result<Object, FetchError> {
        let decoded = decode_budgeted(
            self,
            position,
            crate::ReadLimits {
                max_object_bytes: self.limits.max_object_bytes,
                max_delta_bytes: self.limits.max_delta_bytes,
                max_delta_depth: self.limits.max_delta_depth,
                max_decode_bytes: self.limits.max_decode_bytes,
                max_input_bytes: self.limits.max_pack_bytes,
            },
            cancel,
            remaining,
        );
        decoded.map_err(fetch_read_error)
    }

    fn resolve(
        &mut self,
        known: &(impl KnownObjects + ?Sized),
        remaining: &mut usize,
        observer: &mut ValidationObserver<'_>,
        finisher: &mut finish::Finisher,
    ) -> Result<bool, FetchError> {
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); self.count];
        // REF_DELTA base names come straight from the peer.
        let mut waiting: crate::PeerIdMap<Vec<usize>> = crate::PeerIdMap::default();
        let mut ready = VecDeque::new();
        for (p, entry) in self.entries.iter().enumerate() {
            match entry.base {
                Base::Kind(_) => ready.push_back(p),
                // Entries were read in pack order, so their offsets ascend.
                Base::Offset(offset) => children[self
                    .entries
                    .binary_search_by_key(&offset, |entry| entry.offset)
                    .map_err(|_| Error::Corrupt("delta base offset"))?]
                .push(p),
                Base::Id(id) => waiting.entry(id).or_default().push(p),
            }
        }
        // A REF_DELTA names its base by identity, so bases need theirs before they are popped.
        // OFS_DELTA bases are found by position, so their identities can arrive later.
        if !waiting.is_empty() {
            finisher.finish(self)?;
        }
        let mut work = self.limits.max_resolution_steps;
        let mut resolved = self.entries.iter().filter(|e| e.resolved.is_some()).count();
        let mut external = false;
        loop {
            while let Some(parent) = ready.pop_back() {
                check_cancelled(observer.cancel)?;
                work = work
                    .checked_sub(1)
                    .ok_or(FetchError::Limit("delta resolution steps"))?;
                let (id, depth, kind) = self.entries[parent].resolved.unwrap();
                let mut dependents = std::mem::take(&mut children[parent]);
                dependents.extend(waiting.remove(&id).unwrap_or_default());
                if dependents.is_empty() {
                    continue;
                }
                let base = self.read_budgeted(parent, observer.cancel, remaining)?;
                for child in dependents {
                    check_cancelled(observer.cancel)?;
                    work = work
                        .checked_sub(1)
                        .ok_or(FetchError::Limit("delta resolution steps"))?;
                    if depth >= self.limits.max_delta_depth {
                        return Err(FetchError::Limit("delta depth"));
                    }
                    let entry = &self.entries[child];
                    charge(remaining, entry.length)?;
                    let program = self
                        .inflate_entry(entry, observer.cancel)
                        .map_err(fetch_read_error)?;
                    let data = apply(
                        base.data(),
                        &program,
                        self.limits.max_object_bytes,
                        remaining,
                    )?;
                    let object = Object {
                        format: self.format,
                        kind,
                        data: data.into(),
                    };
                    self.entries[child].resolved =
                        Some((ObjectId::null(self.format), depth + 1, kind));
                    self.cache
                        .insert(self.entries[child].offset, &object, RECEIVED);
                    if waiting.is_empty() {
                        finisher.submit(self, child, object, observer.cancel)?;
                    } else {
                        finisher.submit_now(self, child, object)?;
                    }
                    resolved += 1;
                    observer.resolved_delta();
                    ready.push_back(child);
                }
            }
            if resolved == self.count {
                break;
            }
            // Only consult local history after every currently available in-pack base is resolved.
            // Append ordinary bases to preserve the server's deltas in a self-contained pack.
            let mut external_base = None;
            for &id in waiting.keys() {
                check_cancelled(observer.cancel)?;
                work = work
                    .checked_sub(1)
                    .ok_or(FetchError::Limit("delta resolution steps"))?;
                if let Some(object) = known.get(id) {
                    external_base = Some((id, object));
                    break;
                }
            }
            let Some((id, object)) = external_base else {
                return Err(waiting
                    .keys()
                    .next()
                    .copied()
                    .map_or(Error::DeltaCycle, Error::MissingBase)
                    .into());
            };
            if object.id() != id {
                return Err(Error::Corrupt("external delta base identity").into());
            }
            charge(remaining, object.data().len())?;
            let position = self.append_object(&object, observer.cancel)?;
            children.push(Vec::new());
            ready.push_back(position);
            external = true;
        }
        Ok(external)
    }

    pub(crate) fn append_dependency(
        &mut self,
        object: &Object,
        cancel: &AtomicBool,
    ) -> Result<(), FetchError> {
        if self.identities.contains_key(&object.id()) {
            return Ok(());
        }
        {
            let mut remaining = self.remaining.lock().unwrap_or_else(|e| e.into_inner());
            charge(&mut remaining, object.data().len())?;
        }
        self.append_object(object, cancel)?;
        Ok(())
    }

    pub(crate) fn append_object(
        &mut self,
        object: &Object,
        cancel: &AtomicBool,
    ) -> Result<usize, FetchError> {
        check_cancelled(cancel)?;
        let id = object.id();
        if let Some(&position) = self.identities.get(&id) {
            return Ok(position);
        }
        if self.entries.len() >= self.limits.max_objects {
            return Err(crate::PackWriteError::Limit("objects").into());
        }
        if object.data().len() > self.limits.max_object_bytes {
            return Err(crate::PackWriteError::Limit("object bytes").into());
        }
        let Bytes::File(file, len) = &mut self.pack else {
            unreachable!()
        };
        let offset = self.entries.last().map_or(12, |e| e.end);
        file.as_file_mut().seek(SeekFrom::Start(offset as u64))?;
        let mut header = Vec::new();
        let mut size = object.data().len();
        let mut first = (match object.kind() {
            ObjectKind::Commit => 1,
            ObjectKind::Tree => 2,
            ObjectKind::Blob => 3,
            ObjectKind::Tag => 4,
        } << 4)
            | (size & 15) as u8;
        size >>= 4;
        if size != 0 {
            first |= 128;
        }
        header.push(first);
        while size != 0 {
            let mut b = (size & 127) as u8;
            size >>= 7;
            if size != 0 {
                b |= 128;
            }
            header.push(b);
        }
        // Only this ordinary base is recompressed. Check its complete encoded size before
        // extending the file, so a refused thin-pack completion cannot exceed its disk budget.
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(object.data())?;
        let compressed = encoder.finish()?;
        let end = offset + header.len() + compressed.len();
        if end + self.format.digest_len() > self.limits.max_pack_bytes {
            return Err(crate::PackWriteError::Limit("pack bytes").into());
        }
        file.write_all(&header)?;
        file.write_all(&compressed)?;
        *len = end + self.format.digest_len();
        let mut crc = crc32fast::Hasher::new();
        crc.update(&header);
        crc.update(&compressed);
        let position = self.entries.len();
        self.entries.push(ReceivedEntry {
            base: Base::Kind(object.kind()),
            offset,
            start: offset + header.len(),
            end,
            length: object.data().len(),
            crc: crc.finalize(),
            resolved: Some((id, 0, object.kind())),
        });
        self.identities.insert(id, position);
        self.verified.insert(id, object);
        self.cache.insert(
            offset,
            object,
            Proof {
                identity: Some(id),
                crc: true,
            },
        );
        Ok(position)
    }

    pub(crate) fn finish_thin(&mut self, cancel: &AtomicBool) -> Result<ObjectId, FetchError> {
        let Bytes::File(file, len) = &mut self.pack else {
            unreachable!()
        };
        file.as_file_mut().seek(SeekFrom::Start(8))?;
        file.write_all(
            &u32::try_from(self.entries.len())
                .map_err(|_| FetchError::Limit("pack objects"))?
                .to_be_bytes(),
        )?;
        let end = *len - self.format.digest_len();
        file.as_file_mut().seek(SeekFrom::Start(0))?;
        let mut hash = Hasher::new(self.format);
        let mut left = end;
        let mut buffer = [0; 64 * 1024];
        while left != 0 {
            check_cancelled(cancel)?;
            let n = left.min(buffer.len());
            file.read_exact(&mut buffer[..n])?;
            hash.update(&buffer[..n]);
            left -= n;
        }
        let checksum = hash.finalize();
        file.write_all(checksum.as_bytes())?;
        file.as_file_mut().set_len(*len as u64)?;
        Ok(checksum)
    }

    pub(crate) fn index(&self, checksum: ObjectId) -> Result<Vec<u8>, FetchError> {
        let mut entries: Vec<_> = self
            .entries
            .iter()
            .map(|e| Entry {
                id: e.resolved.unwrap().0,
                offset: e.offset as u64,
                crc: e.crc,
            })
            .collect();
        entries.sort_unstable_by_key(|e| e.id);
        Ok(encode_index(&entries, checksum)?)
    }
}

impl Source for IndexedObjects {
    fn format(&self) -> crate::ObjectFormat {
        self.format
    }
    fn span(&self, position: usize) -> Result<Span, Error> {
        let entry = &self.entries[position];
        Ok(Span {
            offset: entry.offset,
            end: entry.end,
            exact: true,
            crc: Some(entry.crc),
            position: Some(position),
        })
    }
    fn span_at(&self, offset: usize) -> Result<Span, Error> {
        self.span(self.at_offset(offset)?)
    }
    fn identity(&self, position: usize) -> Result<ObjectId, Error> {
        Ok(self.entries[position].resolved.unwrap().0)
    }
    fn find(&self, id: ObjectId) -> Result<Option<usize>, Error> {
        Ok(self.identities.get(&id).copied())
    }
    fn prefix(&self, span: &Span) -> Result<Vec<u8>, Error> {
        self.bytes(span.offset, span.end.min(span.offset + 64))
    }
    fn inflate(
        &self,
        span: &Span,
        _header: &crate::pack::reader::EntryHeader,
        cancel: &AtomicBool,
    ) -> Result<(Vec<u8>, usize), Error> {
        let position = match span.position {
            Some(position) => position,
            None => self.at_offset(span.offset)?,
        };
        let entry = &self.entries[position];
        Ok((self.inflate_entry(entry, cancel)?, entry.end))
    }
    fn identity_verified(&self, position: usize) -> bool {
        self.entries[position].resolved.is_some()
    }
    fn base_cache(&self) -> Option<crate::pack::reader::CacheView<'_>> {
        Some(self.cache.view())
    }
}
impl IndexedObjects {
    fn at_offset(&self, offset: usize) -> Result<usize, Error> {
        self.entries
            .binary_search_by_key(&offset, |e| e.offset)
            .map_err(|_| Error::Corrupt("delta base offset"))
    }

    fn inflate_entry(&self, entry: &ReceivedEntry, cancel: &AtomicBool) -> Result<Vec<u8>, Error> {
        // One positioned read covers the header and stream; the CRC spans both.
        let raw = self.bytes(entry.offset, entry.end)?;
        let (header, mut stream) = raw.split_at(entry.start - entry.offset);
        let mut crc = crc32fast::Hasher::new();
        crc.update(header);
        let (data, consumed) = inflate_stream(
            &mut stream,
            entry.length,
            &mut crc,
            cancel,
            self.pack.path().expect("indexed packs are file backed"),
        )?;
        if consumed != entry.end - entry.start {
            return Err(Error::Corrupt("packed entry length or trailing data"));
        }
        if crc.finalize() != entry.crc {
            return Err(Error::Corrupt("entry CRC"));
        }
        Ok(data)
    }

    fn io(&self, source: std::io::Error) -> Error {
        Error::Path {
            path: self
                .pack
                .path()
                .expect("indexed packs are file backed")
                .to_owned(),
            source,
        }
    }

    fn bytes(&self, start: usize, end: usize) -> Result<Vec<u8>, Error> {
        let mut bytes = vec![0; end - start];
        crate::pack::read_exact_at(&self.file, start as u64, &mut bytes)
            .map_err(|source| self.io(source))?;
        Ok(bytes)
    }
}

fn fetch_read_error(error: Error) -> FetchError {
    match error {
        Error::Cancelled => FetchError::Cancelled,
        other => FetchError::Pack(other),
    }
}

fn read_header(
    format: crate::ObjectFormat,
    reader: &mut impl Read,
    offset: usize,
    crc: &mut crc32fast::Hasher,
) -> Result<(Base, usize, usize), FetchError> {
    let mut prefix = Vec::new();
    let mut next = || -> Result<u8, FetchError> {
        let mut b = [0];
        reader
            .read_exact(&mut b)
            .map_err(|_| Error::Corrupt("truncated pack entry"))?;
        prefix.push(b[0]);
        crc.update(&b);
        Ok(b[0])
    };
    let header = next()?;
    let kind = (header >> 4) & 7;
    let mut length_bytes = vec![header];
    let mut last = header;
    while last & 128 != 0 {
        last = next()?;
        length_bytes.push(last);
        if length_bytes.len() > 10 {
            return Err(Error::Corrupt("size overflow").into());
        }
    }
    let mut input = &length_bytes[1..];
    let length = size(&mut input, (header & 15) as usize, 4, header & 128 != 0)?;
    match kind {
        6 => {
            let mut b = next()?;
            let mut count = 1;
            while b & 128 != 0 {
                b = next()?;
                count += 1;
                if count > 10 {
                    return Err(Error::Corrupt("delta offset overflow").into());
                }
            }
        }
        7 => {
            for _ in 0..format.digest_len() {
                next()?;
            }
        }
        _ => {}
    }
    let mut input = &prefix[length_bytes.len()..];
    let base = base(format, &mut input, kind, offset)?;
    Ok((base, length, prefix.len()))
}

fn inflate_stream(
    reader: &mut impl BufRead,
    expected: usize,
    crc: &mut crc32fast::Hasher,
    cancel: &AtomicBool,
    path: &std::path::Path,
) -> Result<(Vec<u8>, usize), Error> {
    let mut inflater = Decompress::new(true);
    // Capacity grows with actual output, never past one byte beyond the declared size, so an
    // oversized stream shows itself and an inflated claim allocates nothing up front.
    let limit = expected.saturating_add(1);
    let mut data = Vec::with_capacity(limit.min(64 * 1024));
    loop {
        crate::pack::reader::check_cancelled(cancel)?;
        if data.len() == data.capacity() {
            data.reserve_exact(data.len().max(64 * 1024).min(limit - data.len()));
        }
        let input = reader.fill_buf().map_err(|source| Error::Path {
            path: path.to_owned(),
            source,
        })?;
        let before_in = inflater.total_in();
        let before_out = data.len();
        let status = inflater
            .decompress_vec(input, &mut data, FlushDecompress::None)
            .map_err(|_| Error::Corrupt("invalid packed zlib stream"))?;
        let consumed = (inflater.total_in() - before_in) as usize;
        let produced = data.len() - before_out;
        crc.update(&input[..consumed]);
        reader.consume(consumed);
        if data.len() > expected {
            return Err(Error::Corrupt("inflated entry exceeds declared size"));
        }
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

fn base(
    format: crate::ObjectFormat,
    input: &mut &[u8],
    kind: u8,
    offset: usize,
) -> Result<Base, Error> {
    Ok(match kind {
        1 => Base::Kind(ObjectKind::Commit),
        2 => Base::Kind(ObjectKind::Tree),
        3 => Base::Kind(ObjectKind::Blob),
        4 => Base::Kind(ObjectKind::Tag),
        6 => {
            let mut next = byte(input)?;
            let mut distance = (next & 127) as usize;
            while next & 128 != 0 {
                next = byte(input)?;
                distance = distance
                    .checked_add(1)
                    .and_then(|n| n.checked_mul(128))
                    .and_then(|n| n.checked_add((next & 127) as usize))
                    .ok_or(Error::Corrupt("delta offset overflow"))?;
            }
            if distance == 0 {
                return Err(Error::Corrupt("zero delta distance"));
            }
            Base::Offset(
                offset
                    .checked_sub(distance)
                    .ok_or(Error::Corrupt("delta before pack"))?,
            )
        }
        7 => {
            let raw = input
                .get(..format.digest_len())
                .ok_or(Error::Corrupt("truncated base identity"))?;
            let id = ObjectId::from_bytes(format, raw).unwrap();
            *input = &input[format.digest_len()..];
            Base::Id(id)
        }
        other => return Err(Error::ObjectType(other)),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use rstest::rstest;

    use super::*;
    use crate::ObjectFormat;

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn evicted_import_payload_keeps_its_verified_identity(#[case] format: ObjectFormat) {
        let mut imported = read(format, &forward_ref_pack(format), FetchLimits::default()).unwrap();
        imported.objects.cache = BaseCache::new();
        let id = ObjectId::for_blob(format, b"two");
        crate::object::HASHES.with(|hashes| hashes.set(0));
        let object = imported
            .objects
            .get(&id, &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        assert_eq!(object.data(), b"two");
        assert_eq!(crate::object::HASHES.with(std::cell::Cell::get), 0);
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn evicted_verified_import_still_checks_entry_crc(#[case] format: ObjectFormat) {
        let mut imported = read(format, &forward_ref_pack(format), FetchLimits::default()).unwrap();
        imported.objects.cache = BaseCache::new();
        let id = ObjectId::for_blob(format, b"two");
        let position = imported.objects.identities[&id];
        imported.objects.entries[position].crc ^= 1;
        assert!(matches!(
            imported.objects.get(&id, &AtomicBool::new(false)),
            Err(FetchError::Pack(Error::Corrupt("entry CRC")))
        ));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn evicted_delta_bases_are_read_from_the_temporary_pack(#[case] format: ObjectFormat) {
        let mut imported = read(format, &forward_ref_pack(format), FetchLimits::default()).unwrap();
        imported.objects.cache = BaseCache::new();
        let id = ObjectId::for_blob(format, b"two");
        let object = imported
            .objects
            .get(&id, &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        assert_eq!(object.data(), b"two");
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn decoding_after_eviction_uses_the_remaining_fetch_budget(#[case] format: ObjectFormat) {
        let mut imported = read(
            format,
            &forward_ref_pack(format),
            FetchLimits {
                max_decode_bytes: 18,
                ..FetchLimits::default()
            },
        )
        .unwrap();
        imported.objects.cache = BaseCache::new();
        let id = ObjectId::for_blob(format, b"two");
        assert!(matches!(
            imported.objects.get(&id, &AtomicBool::new(false)),
            Err(FetchError::Pack(Error::Limit("cumulative decode bytes")))
        ));
    }

    #[test]
    fn refused_external_base_does_not_extend_the_temporary_file() {
        let format = ObjectFormat::Sha1;
        let bytes = pack(format, &[entry(3, b"", b"one")]);
        let mut imported = read(format, &bytes, FetchLimits::default()).unwrap();
        imported.objects.limits.max_pack_bytes = bytes.len();
        let base = Object {
            format,
            kind: ObjectKind::Blob,
            data: b"external".to_vec().into(),
        };
        assert!(matches!(
            imported
                .objects
                .append_dependency(&base, &AtomicBool::new(false)),
            Err(FetchError::Index(crate::PackWriteError::Limit(
                "pack bytes"
            )))
        ));
        assert_eq!(
            std::fs::metadata(imported.objects.pack.path().unwrap())
                .unwrap()
                .len(),
            bytes.len() as u64
        );
    }

    #[test]
    fn indexed_reads_report_fetch_cancellation() {
        let format = ObjectFormat::Sha1;
        let imported = read(format, &forward_ref_pack(format), FetchLimits::default()).unwrap();
        let id = ObjectId::for_blob(format, b"two");
        assert!(matches!(
            imported.objects.get(&id, &AtomicBool::new(true)),
            Err(FetchError::Cancelled)
        ));
    }

    fn entry(kind: u8, base: &[u8], data: &[u8]) -> Vec<u8> {
        assert!(data.len() < 16);
        let mut result = vec![(kind << 4) | data.len() as u8];
        result.extend(base);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        result.extend(encoder.finish().unwrap());
        result
    }
    fn pack(format: ObjectFormat, entries: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = b"PACK\0\0\0\x02".to_vec();
        bytes.extend((entries.len() as u32).to_be_bytes());
        bytes.extend(entries.concat());
        bytes.extend_from_slice(format.checksum(&bytes).as_bytes());
        bytes
    }
    fn read(
        format: ObjectFormat,
        bytes: &[u8],
        limits: FetchLimits,
    ) -> Result<Imported, FetchError> {
        Imported::read(format, bytes, limits, &AtomicBool::new(false))
    }
    fn forward_ref_pack(format: ObjectFormat) -> Vec<u8> {
        let id = ObjectId::for_blob(format, b"one");
        pack(
            format,
            &[
                entry(7, id.as_bytes(), b"\x03\x03\x03two"),
                entry(3, b"", b"one"),
            ],
        )
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn delta_progress_completion_follows_connectivity(#[case] format: ObjectFormat) {
        let cancel = AtomicBool::new(false);
        let mut snapshots = Vec::new();
        let advertisement = super::super::Advertisement {
            refs: vec![],
            capabilities: vec![format!("object-format={format}").into_bytes()],
        };
        let received = super::super::ReceivedFetch::validate_known(
            advertisement,
            vec![ObjectId::for_blob(format, b"two")],
            forward_ref_pack(format),
            &super::super::KnownHistory::default(),
            vec![],
            super::super::FetchOptions {
                limits: FetchLimits::default(),
                depth: None,
            },
            &mut ValidationObserver::new(&cancel, &mut |state| snapshots.push(state)),
        )
        .unwrap();
        assert_eq!(received.object_count(), 2);
        assert_eq!(
            snapshots.last(),
            Some(&super::super::ValidationProgress {
                objects: (2, 2),
                deltas: Some((1, 1)),
                complete: true
            })
        );
        assert!(
            snapshots[..snapshots.len() - 1]
                .iter()
                .all(|state| !state.complete)
        );
    }

    #[rstest]
    #[case::before_resolution(0)]
    #[case::after_resolution(1)]
    fn delta_progress_cancellation_never_completes(#[case] stop_at: u64) {
        let format = ObjectFormat::Sha1;
        let cancel = AtomicBool::new(false);
        let mut snapshots = Vec::new();
        let advertisement = super::super::Advertisement {
            refs: vec![],
            capabilities: vec![],
        };
        let result = super::super::ReceivedFetch::validate_known(
            advertisement,
            vec![ObjectId::for_blob(format, b"two")],
            forward_ref_pack(format),
            &super::super::KnownHistory::default(),
            vec![],
            super::super::FetchOptions {
                limits: FetchLimits::default(),
                depth: None,
            },
            &mut ValidationObserver::new(&cancel, &mut |state| {
                snapshots.push(state);
                if state.deltas == Some((stop_at, 1)) {
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }),
        );
        assert!(matches!(result, Err(FetchError::Cancelled)));
        assert!(snapshots.iter().all(|state| !state.complete));
    }

    #[test]
    fn entry_decode_failure_counts_only_inflated_entries() {
        let bytes = pack(
            ObjectFormat::Sha1,
            &[entry(3, b"", b"one"), vec![0x33, 1, 2, 3]],
        );
        let cancel = AtomicBool::new(false);
        let mut snapshots = Vec::new();
        let result = Imported::read_observed(
            ObjectFormat::Sha1,
            &bytes,
            &IdMap::default(),
            FetchLimits::default(),
            &mut ValidationObserver::new(&cancel, &mut |state| snapshots.push(state)),
        );
        assert!(result.is_err());
        assert_eq!(
            snapshots.last(),
            Some(&super::super::ValidationProgress {
                objects: (1, 2),
                deltas: None,
                complete: false
            })
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn delta_progress_counts_actual_resolution_once(#[case] format: ObjectFormat) {
        let mut snapshots = Vec::new();
        let cancel = AtomicBool::new(false);
        let imported = Imported::read_observed(
            format,
            &forward_ref_pack(format),
            &IdMap::default(),
            FetchLimits::default(),
            &mut ValidationObserver::new(&cancel, &mut |state| snapshots.push(state)),
        )
        .unwrap();
        assert_eq!(imported.objects.len(), 2);
        assert_eq!(
            snapshots
                .iter()
                .map(|state| (state.objects, state.deltas))
                .collect::<Vec<_>>(),
            [
                ((0, 2), None),
                ((1, 2), None),
                ((2, 2), None),
                ((2, 2), Some((0, 1))),
                ((2, 2), Some((1, 1))),
            ]
        );
        assert!(snapshots.iter().all(|state| !state.complete));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn malformed_delta_never_reports_resolved_work(#[case] format: ObjectFormat) {
        let bytes = pack(
            format,
            &[
                entry(3, b"", b"one"),
                entry(
                    7,
                    ObjectId::for_blob(format, b"one").as_bytes(),
                    b"\x03\x03\0",
                ),
            ],
        );
        let mut snapshots = Vec::new();
        let cancel = AtomicBool::new(false);
        let result = Imported::read_observed(
            format,
            &bytes,
            &IdMap::default(),
            FetchLimits::default(),
            &mut ValidationObserver::new(&cancel, &mut |state| snapshots.push(state)),
        );
        assert!(matches!(result, Err(FetchError::Pack(Error::Corrupt(_)))));
        assert_eq!(snapshots.last().unwrap().deltas, Some((0, 1)));
        assert!(snapshots.iter().all(|state| !state.complete));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn resolves_forward_ref_base_and_generates_readable_index(#[case] format: ObjectFormat) {
        let bytes = forward_ref_pack(format);
        let imported = read(format, &bytes, FetchLimits::default()).unwrap();
        let reader = crate::pack::Pack::open(format, &imported.index, bytes).unwrap();
        let id = ObjectId::for_blob(format, b"two");
        assert_eq!(
            reader
                .read(reader.find(id).unwrap(), crate::ReadLimits::default())
                .unwrap()
                .data(),
            b"two"
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn resolves_ofs_base(#[case] format: ObjectFormat) {
        let ordinary = entry(3, b"", b"one");
        let delta = entry(6, &[ordinary.len() as u8], b"\x03\x03\x03two");
        let imported = read(
            format,
            &pack(format, &[ordinary, delta]),
            FetchLimits::default(),
        )
        .unwrap();
        assert_eq!(
            imported
                .objects
                .get(&ObjectId::for_blob(format, b"two"), &AtomicBool::new(false))
                .unwrap()
                .unwrap()
                .data(),
            b"two"
        );
    }

    #[rstest]
    #[case::depth_sha1(ObjectFormat::Sha1, FetchLimits { max_delta_depth: 0, ..FetchLimits::default() })]
    #[case::depth_sha256(ObjectFormat::Sha256, FetchLimits { max_delta_depth: 0, ..FetchLimits::default() })]
    #[case::program_sha1(ObjectFormat::Sha1, FetchLimits { max_delta_bytes: 5, ..FetchLimits::default() })]
    #[case::program_sha256(ObjectFormat::Sha256, FetchLimits { max_delta_bytes: 5, ..FetchLimits::default() })]
    #[case::reconstructed_bytes_sha1(ObjectFormat::Sha1, FetchLimits { max_decode_bytes: 11, ..FetchLimits::default() })]
    #[case::reconstructed_bytes_sha256(ObjectFormat::Sha256, FetchLimits { max_decode_bytes: 11, ..FetchLimits::default() })]
    #[case::resolution_work_sha1(ObjectFormat::Sha1, FetchLimits { max_resolution_steps: 2, ..FetchLimits::default() })]
    #[case::resolution_work_sha256(ObjectFormat::Sha256, FetchLimits { max_resolution_steps: 2, ..FetchLimits::default() })]
    fn delta_limits(#[case] format: ObjectFormat, #[case] limits: FetchLimits) {
        assert!(matches!(
            read(format, &forward_ref_pack(format), limits),
            Err(FetchError::Limit(_) | FetchError::Pack(Error::Limit(_)))
        ));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn exact_delta_limits_succeed(#[case] format: ObjectFormat) {
        let limits = FetchLimits {
            max_delta_depth: 1,
            max_delta_bytes: 6,
            max_decode_bytes: 18,
            max_object_bytes: 3,
            max_resolution_steps: 4,
            ..FetchLimits::default()
        };
        assert_eq!(
            read(format, &forward_ref_pack(format), limits)
                .unwrap()
                .objects
                .len(),
            2
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn rejects_external_base(#[case] format: ObjectFormat) {
        let id = ObjectId::for_blob(format, b"external");
        let bytes = pack(format, &[entry(7, id.as_bytes(), b"\x08\x03\x03two")]);
        assert!(
            matches!(read(format, &bytes, FetchLimits::default()), Err(FetchError::Pack(Error::MissingBase(actual))) if actual == id)
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn verified_external_base_becomes_self_contained_pack(#[case] format: ObjectFormat) {
        let base = Object {
            format,
            kind: ObjectKind::Blob,
            data: b"one".to_vec().into(),
        };
        let base_id = base.id();
        let thin = pack(format, &[entry(7, base_id.as_bytes(), b"\x03\x03\x03two")]);
        let known = crate::IdMap::from_iter([(base_id, base)]);
        let imported = Imported::read_with_known(
            format,
            &thin,
            &known,
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let complete = imported.objects.pack.to_vec();
        let reader = crate::pack::Pack::open(format, &imported.index, complete).unwrap();
        let id = ObjectId::for_blob(format, b"two");
        assert_eq!(
            reader
                .read(reader.find(id).unwrap(), crate::ReadLimits::default())
                .unwrap()
                .data(),
            b"two"
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn rejects_corrupt_external_base(#[case] format: ObjectFormat) {
        let id = ObjectId::for_blob(format, b"one");
        let thin = pack(format, &[entry(7, id.as_bytes(), b"\x03\x03\x03two")]);
        let corrupt = Object {
            format,
            kind: ObjectKind::Blob,
            data: b"bad".to_vec().into(),
        };
        let known = crate::IdMap::from_iter([(id, corrupt)]);
        assert!(matches!(
            Imported::read_with_known(
                format,
                &thin,
                &known,
                FetchLimits::default(),
                &AtomicBool::new(false)
            ),
            Err(FetchError::Pack(Error::Corrupt(
                "external delta base identity"
            )))
        ));
    }

    #[rstest]
    #[case::before_pack_sha1(ObjectFormat::Sha1, 127)]
    #[case::before_pack_sha256(ObjectFormat::Sha256, 127)]
    #[case::self_reference_sha1(ObjectFormat::Sha1, 0)]
    #[case::self_reference_sha256(ObjectFormat::Sha256, 0)]
    #[case::not_entry_boundary_sha1(ObjectFormat::Sha1, 1)]
    #[case::not_entry_boundary_sha256(ObjectFormat::Sha256, 1)]
    fn rejects_bad_offset_base(#[case] format: ObjectFormat, #[case] distance: u8) {
        let bytes = pack(format, &[entry(6, &[distance], b"\x03\x03\x03two")]);
        assert!(matches!(
            read(format, &bytes, FetchLimits::default()),
            Err(FetchError::Pack(Error::Corrupt(_)))
        ));
    }

    #[rstest]
    #[case::reserved_sha1(ObjectFormat::Sha1, 0)]
    #[case::reserved_sha256(ObjectFormat::Sha256, 0)]
    #[case::unused_sha1(ObjectFormat::Sha1, 5)]
    #[case::unused_sha256(ObjectFormat::Sha256, 5)]
    fn rejects_reserved_type(#[case] format: ObjectFormat, #[case] kind: u8) {
        assert!(matches!(
            read(
                format,
                &pack(format, &[entry(kind, b"", b"one")]),
                FetchLimits::default()
            ),
            Err(FetchError::Pack(Error::ObjectType(_)))
        ));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn rejects_duplicate_identities(#[case] format: ObjectFormat) {
        let bytes = pack(format, &[entry(3, b"", b"one"), entry(3, b"", b"one")]);
        assert!(matches!(
            read(format, &bytes, FetchLimits::default()),
            Err(FetchError::Pack(Error::Corrupt(
                "duplicate received identity"
            )))
        ));
    }

    #[rstest]
    #[case::reserved_instruction_sha1(ObjectFormat::Sha1, b"\x03\x03\0")]
    #[case::reserved_instruction_sha256(ObjectFormat::Sha256, b"\x03\x03\0")]
    #[case::wrong_base_length_sha1(ObjectFormat::Sha1, b"\x04\x03\x03two")]
    #[case::wrong_base_length_sha256(ObjectFormat::Sha256, b"\x04\x03\x03two")]
    #[case::short_result_sha1(ObjectFormat::Sha1, b"\x03\x04\x03two")]
    #[case::short_result_sha256(ObjectFormat::Sha256, b"\x03\x04\x03two")]
    #[case::copy_outside_sha1(ObjectFormat::Sha1, b"\x03\x03\x91\x04\x03")]
    #[case::copy_outside_sha256(ObjectFormat::Sha256, b"\x03\x03\x91\x04\x03")]
    fn rejects_invalid_delta_program(#[case] format: ObjectFormat, #[case] program: &[u8]) {
        let bytes = pack(
            format,
            &[
                entry(3, b"", b"one"),
                entry(7, ObjectId::for_blob(format, b"one").as_bytes(), program),
            ],
        );
        assert!(matches!(
            read(format, &bytes, FetchLimits::default()),
            Err(FetchError::Pack(Error::Corrupt(_)))
        ));
    }

    #[rstest]
    #[case::bad_zlib_sha1(ObjectFormat::Sha1, vec![0x33, 1, 2, 3])]
    #[case::bad_zlib_sha256(ObjectFormat::Sha256, vec![0x33, 1, 2, 3])]
    #[case::truncated_zlib_sha1(ObjectFormat::Sha1, vec![0x33, 0x78, 0x9c])]
    #[case::truncated_zlib_sha256(ObjectFormat::Sha256, vec![0x33, 0x78, 0x9c])]
    #[case::wrong_length_sha1(ObjectFormat::Sha1, {let mut e = entry(3, b"", b"one"); e[0] = 0x34; e})]
    #[case::wrong_length_sha256(ObjectFormat::Sha256, {let mut e = entry(3, b"", b"one"); e[0] = 0x34; e})]
    #[case::trailing_sha1(ObjectFormat::Sha1, {let mut e = entry(3, b"", b"one"); e.push(0); e})]
    #[case::trailing_sha256(ObjectFormat::Sha256, {let mut e = entry(3, b"", b"one"); e.push(0); e})]
    fn rejects_entry_framing_even_with_valid_checksum(
        #[case] format: ObjectFormat,
        #[case] raw: Vec<u8>,
    ) {
        assert!(matches!(
            read(format, &pack(format, &[raw]), FetchLimits::default()),
            Err(FetchError::Pack(Error::Corrupt(_)))
        ));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn cancellation_precedes_pack_validation(#[case] format: ObjectFormat) {
        assert!(matches!(
            Imported::read(format, b"", FetchLimits::default(), &AtomicBool::new(true)),
            Err(FetchError::Cancelled)
        ));
    }
}

#[cfg(test)]
mod format_tests {
    use rstest::rstest;

    use super::*;
    use crate::ObjectFormat;

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1, ObjectFormat::Sha256)]
    #[case::sha256(ObjectFormat::Sha256, ObjectFormat::Sha1)]
    fn validates_empty_and_rejects_wrong_format_and_truncation(
        #[case] format: ObjectFormat,
        #[case] other: ObjectFormat,
    ) {
        let mut pack = b"PACK\0\0\0\x02\0\0\0\0".to_vec();
        pack.extend_from_slice(format.checksum(&pack).as_bytes());
        let cancel = AtomicBool::new(false);
        let limits = FetchLimits::default();
        let imported = Imported::read(format, &pack, limits, &cancel).unwrap();
        assert!(imported.objects.is_empty());
        assert_eq!(imported.checksum.format(), format);
        assert!(crate::pack::Pack::open(format, &imported.index, pack.clone()).is_ok());
        assert!(Imported::read(other, &pack, limits, &cancel).is_err());
        assert!(
            (0..pack.len())
                .all(|end| Imported::read(format, &pack[..end], limits, &cancel).is_err())
        );
        pack[12] ^= 1;
        assert!(matches!(
            Imported::read(format, &pack, limits, &cancel),
            Err(FetchError::Pack(Error::Corrupt("received pack checksum")))
        ));
    }
}

#[cfg(test)]
mod git_format_tests {
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    use rstest::rstest;

    use super::*;
    use crate::{InitKind, ObjectFormat, Repository};

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn imports_git_thin_delta_with_verified_external_base(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
        let mut state = 123456789u64;
        let base: Vec<u8> = (0..16384)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let mut changed = base.clone();
        changed[100] ^= 1;
        let old: ObjectId = std::str::from_utf8(&git(
            repo.git_dir(),
            &["hash-object", "-w", "--stdin"],
            &base,
        ))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
        let new: ObjectId = std::str::from_utf8(&git(
            repo.git_dir(),
            &["hash-object", "-w", "--stdin"],
            &changed,
        ))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
        let old_tree = git(
            repo.git_dir(),
            &["mktree"],
            format!("100644 blob {old}\tfile\n").as_bytes(),
        );
        let new_tree = git(
            repo.git_dir(),
            &["mktree"],
            format!("100644 blob {new}\tfile\n").as_bytes(),
        );
        let old_tree = std::str::from_utf8(&old_tree).unwrap().trim();
        let new_tree = std::str::from_utf8(&new_tree).unwrap().trim();
        git(repo.git_dir(), &["config", "user.name", "Fixture"], b"");
        git(
            repo.git_dir(),
            &["config", "user.email", "f@example.test"],
            b"",
        );
        let old_commit = git(repo.git_dir(), &["commit-tree", old_tree], b"old\n");
        let old_commit = std::str::from_utf8(&old_commit).unwrap().trim();
        let new_commit = git(
            repo.git_dir(),
            &["commit-tree", new_tree, "-p", old_commit],
            b"new\n",
        );
        let new_commit = std::str::from_utf8(&new_commit).unwrap().trim();
        let input = format!("{new_commit}\n^{old_commit}\n");
        let pack = git(
            repo.git_dir(),
            &[
                "pack-objects",
                "--stdout",
                "--thin",
                "--revs",
                "--window=50",
                "--depth=50",
            ],
            input.as_bytes(),
        );
        assert!(
            matches!(Imported::read(format, &pack, FetchLimits::default(), &AtomicBool::new(false)),
            Err(FetchError::Pack(Error::MissingBase(actual))) if actual == old)
        );
        let known = crate::IdMap::from_iter([(
            old,
            Object {
                format,
                kind: ObjectKind::Blob,
                data: base.into(),
            },
        )]);
        let imported = Imported::read_with_known(
            format,
            &pack,
            &known,
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            imported
                .objects
                .get(&new, &AtomicBool::new(false))
                .unwrap()
                .unwrap()
                .data(),
            changed
        );
        let full = imported.objects.pack.to_vec();
        assert!(full.len() > pack.len());
        assert!(crate::pack::Pack::open(format, &imported.index, full).is_ok());
        let mut failed_snapshots = Vec::new();
        let cancel = AtomicBool::new(false);
        assert!(matches!(
            Imported::read_observed(
                format,
                &pack,
                &known,
                FetchLimits {
                    max_pack_bytes: pack.len(),
                    ..FetchLimits::default()
                },
                &mut ValidationObserver::new(&cancel, &mut |state| failed_snapshots.push(state))
            ),
            Err(FetchError::Index(crate::PackWriteError::Limit(_)))
        ));
        assert!(failed_snapshots.iter().all(|state| !state.complete));
        let resolved = failed_snapshots.last().unwrap().deltas.unwrap();
        // Disk-space/pack-size refusal can precede delta reconstruction.
        assert!(resolved.0 <= resolved.1);
    }

    /// Original CLI observations; no upstream implementation or test source is used.
    fn git(path: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let mut child = command
            .current_dir(path)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", path.join("absent-config"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    }

    fn received(repo: &Repository, delta_option: &str) -> (Vec<u8>, Vec<(ObjectId, Vec<u8>)>) {
        let mut names = Vec::new();
        let mut records = Vec::new();
        for variant in 0..8 {
            let mut state = 123456789u64;
            let mut bytes: Vec<_> = (0..16384)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state as u8
                })
                .collect();
            bytes[variant * 100] ^= 255;
            let name = git(repo.git_dir(), &["hash-object", "-w", "--stdin"], &bytes);
            records.push((
                std::str::from_utf8(&name).unwrap().trim().parse().unwrap(),
                bytes,
            ));
            names.extend(name);
        }
        let pack = git(
            repo.git_dir(),
            &[
                "pack-objects",
                "--stdout",
                "--no-reuse-object",
                delta_option,
            ],
            &names,
        );
        (pack, records)
    }

    #[rstest]
    #[case::sha1_ref(ObjectFormat::Sha1, "--window=8", 7)]
    #[case::sha256_ref(ObjectFormat::Sha256, "--window=8", 7)]
    #[case::sha1_ofs(ObjectFormat::Sha1, "--delta-base-offset", 6)]
    #[case::sha256_ofs(ObjectFormat::Sha256, "--delta-base-offset", 6)]
    fn imports_git_deltas_and_git_verifies_generated_index(
        #[case] format: ObjectFormat,
        #[case] option: &str,
        #[case] delta_type: u8,
    ) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
        let (pack, records) = received(&repo, option);
        let mut snapshots = Vec::new();
        let cancel = AtomicBool::new(false);
        let imported = Imported::read_observed(
            format,
            &pack,
            &IdMap::default(),
            FetchLimits::default(),
            &mut ValidationObserver::new(&cancel, &mut |state| snapshots.push(state)),
        )
        .unwrap();
        assert_eq!(imported.objects.len(), records.len());
        assert!(records.iter().all(|(id, bytes)| {
            imported
                .objects
                .get(id, &AtomicBool::new(false))
                .unwrap()
                .unwrap()
                .data()
                == bytes
        }));
        let base = repo
            .object_dir()
            .join("pack")
            .join(format!("pack-{}", imported.checksum));
        std::fs::write(base.with_extension("pack"), &pack).unwrap();
        std::fs::write(base.with_extension("idx"), &imported.index).unwrap();
        // Git for Windows does not accept Rust's verbatim absolute paths here.
        let git_base = base.strip_prefix(repo.git_dir()).unwrap();
        let report = git(
            repo.git_dir(),
            &[
                "verify-pack",
                "-v",
                git_base.with_extension("idx").to_str().unwrap(),
            ],
            b"",
        );
        let report = String::from_utf8(report).unwrap();
        let delta_offsets: Vec<usize> = report
            .lines()
            .filter_map(|line| {
                let words: Vec<_> = line.split_whitespace().collect();
                (words.len() == 7).then(|| words[4].parse().unwrap())
            })
            .collect();
        assert!(!delta_offsets.is_empty());
        let last = snapshots.last().unwrap();
        assert_eq!(last.objects, (records.len() as u64, records.len() as u64));
        assert_eq!(
            last.deltas,
            Some((delta_offsets.len() as u64, delta_offsets.len() as u64))
        );
        assert!(
            snapshots
                .windows(2)
                .all(|pair| pair[0].objects.0 <= pair[1].objects.0
                    && pair[0].deltas <= pair[1].deltas)
        );
        assert!(snapshots.iter().all(|state| !state.complete));
        assert!(
            delta_offsets
                .iter()
                .all(|offset| (pack[*offset] >> 4) & 7 == delta_type)
        );
        git(
            repo.git_dir(),
            &[
                "index-pack",
                "--index-version=2",
                "-o",
                "independent.idx",
                git_base.with_extension("pack").to_str().unwrap(),
            ],
            b"",
        );
        assert_eq!(
            std::fs::read(repo.git_dir().join("independent.idx")).unwrap(),
            imported.index
        );
    }
}
