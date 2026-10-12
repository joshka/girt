//! Pinned file pairs with bounded offset tables and explicit streaming pack verification.
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use super::index::{Entry, Index, word};
use super::reader::{EntryHeader, Source, check_cancelled, decode};
use crate::object::Hasher;
use crate::{Object, ObjectFormat, ObjectId, ObjectKind, ObjectReadError as Error, ReadLimits};

/// Serializes short seek/read operations without cloning descriptors or sharing cursor state.
#[derive(Debug)]
struct Artifact {
    file: Mutex<File>,
    path: PathBuf,
    len: usize,
    #[cfg(test)]
    bytes_read: std::sync::atomic::AtomicUsize,
}

impl Artifact {
    fn open(path: &Path) -> Result<Self, Error> {
        let file = File::open(path).map_err(|source| path_error(path, source))?;
        let len = file
            .metadata()
            .map_err(|source| path_error(path, source))?
            .len();
        let len = usize::try_from(len).map_err(|_| Error::Limit("artifact address space"))?;
        Ok(Self {
            file: Mutex::new(file),
            path: path.to_owned(),
            len,
            #[cfg(test)]
            bytes_read: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn read_at(&self, offset: usize, output: &mut [u8]) -> Result<(), Error> {
        #[cfg(test)]
        self.bytes_read
            .fetch_add(output.len(), std::sync::atomic::Ordering::Relaxed);
        let mut file = self.file.lock().unwrap_or_else(|error| error.into_inner());
        file.seek(SeekFrom::Start(offset as u64))
            .map_err(|source| path_error(&self.path, source))?;
        file.read_exact(output)
            .map_err(|source| path_error(&self.path, source))
    }

    fn bytes(&self, start: usize, end: usize) -> Result<Vec<u8>, Error> {
        let mut bytes = vec![0; end - start];
        self.read_at(start, &mut bytes)?;
        Ok(bytes)
    }

    fn check_not_truncated(&self) -> Result<(), Error> {
        let file = self.file.lock().unwrap_or_else(|error| error.into_inner());
        let current = file
            .metadata()
            .map_err(|source| path_error(&self.path, source))?
            .len();
        if current < self.len as u64 {
            return Err(path_error(&self.path, io::ErrorKind::UnexpectedEof.into()));
        }
        Ok(())
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
    index_bytes: Vec<u8>,
    pack: Artifact,
    format: ObjectFormat,
    legacy: bool,
    ranges: Vec<(usize, usize)>,
    offsets: Vec<usize>,
    base_cache: super::reader::BaseCache,
}

impl FilePack {
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
        if &header[..4] != b"PACK" {
            return Err(Error::Corrupt("pack header"));
        }
        let version = word(&header, 4)?;
        if !matches!(version, 2 | 3) {
            return Err(Error::PackVersion(version));
        }
        let end = pack.len - format.digest_len();
        let index_bytes = index_file.bytes(0, index_file.len)?;
        check_cancelled(cancelled)?;
        let index = Index::parse(format, &index_bytes, end)?;
        check_cancelled(cancelled)?;
        if word(&header, 8)? as usize != index.entries.len() {
            return Err(Error::Corrupt("pack object count"));
        }
        let legacy = !index_bytes.starts_with(b"\xfftOc");
        let trailer = pack.bytes(end, pack.len)?;
        if index.pack_hash.as_bytes() != trailer {
            return Err(Error::Corrupt("index/pack checksum disagreement"));
        }
        check_cancelled(cancelled)?;
        let ranges = index
            .entries
            .iter()
            .map(|entry| (entry.offset, entry.end))
            .collect();
        Ok(Self {
            index: index_file,
            index_bytes,
            pack,
            format,
            legacy,
            ranges,
            offsets: index.offsets,
            base_cache: super::reader::BaseCache::new(),
        })
    }

    /// Checks the entire pinned pack checksum and every v2 entry CRC without decoding objects.
    pub(crate) fn verify(&self, cancelled: &AtomicBool) -> Result<(), Error> {
        check_cancelled(cancelled)?;
        self.index.check_not_truncated()?;
        self.pack.check_not_truncated()?;
        let pack = &self.pack;
        let format = self.format;
        let end = pack.len - format.digest_len();
        let mut hash = Hasher::new(format);
        hash.update(&pack.bytes(0, 12)?);
        let mut buffer = [0; 64 * 1024];
        for &position in &self.offsets {
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
        let pack_hash = &self.index_bytes[self.index_bytes.len() - 2 * format.digest_len()
            ..self.index_bytes.len() - format.digest_len()];
        if pack_hash != trailer {
            return Err(Error::Corrupt("index/pack checksum disagreement"));
        }
        check_cancelled(cancelled)
    }

    pub(crate) fn find(&self, id: ObjectId) -> Result<Option<usize>, Error> {
        if id.format() != self.format {
            return Ok(None);
        }
        // Retained identities cannot turn a truncated pinned index into a successful lookup.
        self.index.check_not_truncated()?;
        let mut low = 0;
        let mut high = self.ranges.len();
        while low < high {
            let middle = low + (high - low) / 2;
            match self.identity(middle)?.cmp(&id) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return Ok(Some(middle)),
            }
        }
        Ok(None)
    }

    fn identity(&self, position: usize) -> Result<ObjectId, Error> {
        let width = self.format.digest_len();
        let start = if self.legacy {
            1028 + position * (width + 4)
        } else {
            1032 + position * width
        };
        Ok(ObjectId::from_bytes(self.format, &self.index_bytes[start..start + width]).unwrap())
    }

    /// Iterates identities from the validated pinned index without decoding their payloads.
    pub(crate) fn identities(&self) -> impl Iterator<Item = Result<ObjectId, Error>> + '_ {
        (0..self.ranges.len()).map(|position| self.identity(position))
    }

    /// Reads entry/delta headers only. Payload identity and CRC remain the reader's job.
    pub(crate) fn locate(
        &self,
        mut position: usize,
        limits: ReadLimits,
        cancel: &AtomicBool,
    ) -> Result<(ObjectKind, u64), Error> {
        let mut seen = std::collections::BTreeSet::new();
        let mut result_size = None;
        loop {
            check_cancelled(cancel)?;
            if !seen.insert(position) {
                return Err(Error::Corrupt("delta cycle"));
            }
            let entry = self.entry(position)?;
            let header = super::reader::entry_header(self, &entry)?;
            if let Some(base) = header.base {
                if seen.len() > limits.max_delta_depth {
                    return Err(Error::Limit("delta depth"));
                }
                if result_size.is_none() {
                    let input = Range {
                        artifact: &self.pack,
                        position: header.data_start,
                        end: entry.end,
                        crc: None,
                    };
                    let mut decoder = flate2::read::ZlibDecoder::new(input);
                    let mut prefix = Vec::new();
                    // Two bounded delta size varints; no instructions or blob bytes are inflated.
                    for _ in 0..2 {
                        for _ in 0..10 {
                            let mut byte = [0];
                            decoder
                                .read_exact(&mut byte)
                                .map_err(|_| Error::Corrupt("delta size prefix"))?;
                            prefix.push(byte[0]);
                            if byte[0] & 128 == 0 {
                                break;
                            }
                        }
                    }
                    let mut input = prefix.as_slice();
                    super::delta::size(&mut input, 0, 0, true)?;
                    result_size = Some(super::delta::size(&mut input, 0, 0, true)? as u64);
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
                let size = result_size.unwrap_or(header.length as u64);
                if size > limits.max_object_bytes as u64 {
                    return Err(Error::Limit("object bytes"));
                }
                return Ok((kind, size));
            }
        }
    }

    pub(crate) fn read(
        &self,
        position: usize,
        limits: ReadLimits,
        cancelled: &AtomicBool,
    ) -> Result<Object, Error> {
        decode(self, position, limits, cancelled)
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
    pub(crate) fn object_header(
        &self,
        position: usize,
        limits: ReadLimits,
    ) -> Result<(crate::ObjectKind, usize), Error> {
        let mut current = position;
        let mut length = None;
        let mut seen = std::collections::HashSet::new();
        loop {
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
                let range = Range {
                    artifact: &self.pack,
                    position: header.data_start,
                    end: entry.end,
                    crc: None,
                };
                let mut decoder = flate2::read::ZlibDecoder::new(BufReader::new(range));
                let mut prefix = Vec::new();
                // Two size varints, each at most ten bytes on a 64-bit host.
                for _ in 0..2 {
                    loop {
                        if prefix.len() == 20 {
                            return Err(Error::Corrupt("delta size overflow"));
                        }
                        let mut byte = [0];
                        decoder
                            .read_exact(&mut byte)
                            .map_err(|e| path_error(&self.pack.path, e))?;
                        prefix.push(byte[0]);
                        if byte[0] & 128 == 0 {
                            break;
                        }
                    }
                }
                let mut input = prefix.as_slice();
                super::delta::size(&mut input, 0, 0, true)?;
                let result = super::delta::size(&mut input, 0, 0, true)?;
                length.get_or_insert(result);
                current = base;
            } else {
                let kind = match header.kind {
                    1 => crate::ObjectKind::Commit,
                    2 => crate::ObjectKind::Tree,
                    3 => crate::ObjectKind::Blob,
                    4 => crate::ObjectKind::Tag,
                    _ => unreachable!("entry header checks kind"),
                };
                let length = length.unwrap_or(header.length);
                if length > limits.max_object_bytes {
                    return Err(Error::Limit("object bytes"));
                }
                return Ok((kind, length));
            }
        }
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
                crc.update(&header.encoded);
            }
            crc
        });
        let input = Range {
            artifact: &self.pack,
            position: start,
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
        let (offset, end) = self.ranges[position];
        Ok(Entry {
            id: self.identity(position)?,
            offset,
            end,
            crc: (!self.legacy)
                .then(|| {
                    word(
                        &self.index_bytes,
                        1032 + self.ranges.len() * self.format.digest_len() + position * 4,
                    )
                })
                .transpose()?,
        })
    }

    fn find(&self, id: ObjectId) -> Result<Option<usize>, Error> {
        self.find(id)
    }

    fn base_cache(&self) -> Option<&super::reader::BaseCache> {
        Some(&self.base_cache)
    }

    fn at_offset(&self, offset: usize) -> Result<usize, Error> {
        let position = self
            .offsets
            .binary_search_by_key(&offset, |&i| self.ranges[i].0)
            .map_err(|_| Error::Corrupt("base offset is not an indexed entry"))?;
        Ok(self.offsets[position])
    }

    fn prefix(&self, entry: &Entry) -> Result<Vec<u8>, Error> {
        self.pack
            .bytes(entry.offset, entry.end.min(entry.offset.saturating_add(64)))
    }

    fn inflate(
        &self,
        entry: &Entry,
        header: &EntryHeader,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>, Error> {
        let start = header.data_start;
        let end = entry.end;
        let expected = header.length;
        let crc = entry.crc.map(|_| {
            let mut crc = crc32fast::Hasher::new();
            crc.update(&header.encoded);
            crc
        });
        let input = Range {
            artifact: &self.pack,
            position: start,
            end,
            crc,
        };
        let mut input = BufReader::with_capacity(64 * 1024, input);
        let mut decoder = flate2::Decompress::new(true);
        let mut data = vec![0; expected];
        let mut written = 0;
        let mut overflow = [0; 1];
        loop {
            check_cancelled(cancelled)?;
            let bytes = input
                .fill_buf()
                .map_err(|source| path_error(&self.pack.path, source))?;
            let before_in = decoder.total_in();
            let before_out = decoder.total_out();
            // Keep the old 8 KiB cancellation cadence without copying each decoded chunk.
            let available = expected - written;
            let output = if available == 0 {
                &mut overflow[..]
            } else {
                &mut data[written..written + available.min(8192)]
            };
            let status = decoder
                .decompress(bytes, output, flate2::FlushDecompress::None)
                .map_err(|_| Error::Corrupt("invalid packed zlib stream"))?;
            let consumed = (decoder.total_in() - before_in) as usize;
            let count = (decoder.total_out() - before_out) as usize;
            input.consume(consumed);
            if count > available {
                return Err(Error::Corrupt("inflated entry exceeds declared size"));
            }
            written += count;
            if status == flate2::Status::StreamEnd {
                if written != expected || decoder.total_in() != (end - start) as u64 {
                    return Err(Error::Corrupt("packed entry length or trailing data"));
                }
                break;
            }
            if consumed == 0 && count == 0 {
                return Err(Error::Corrupt("truncated packed zlib stream"));
            }
        }
        input.get_ref().verify_crc(entry.crc)?;
        Ok(data)
    }
}

/// A bounded cursor whose reads never disturb another lookup's logical position.
struct Range<'a> {
    artifact: &'a Artifact,
    position: usize,
    end: usize,
    crc: Option<crc32fast::Hasher>,
}

impl Range<'_> {
    fn verify_crc(&self, expected: Option<u32>) -> Result<(), Error> {
        if let Some(expected) = expected {
            let actual = self.crc.as_ref().expect("v2 entry CRC").clone().finalize();
            if actual != expected {
                return Err(Error::Corrupt("entry CRC"));
            }
        }
        Ok(())
    }
}

impl Read for Range<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = output.len().min(self.end - self.position);
        if count == 0 {
            return Ok(0);
        }
        let mut file = self
            .artifact
            .file
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        file.seek(SeekFrom::Start(self.position as u64))?;
        file.read_exact(&mut output[..count])?;
        #[cfg(test)]
        self.artifact
            .bytes_read
            .fetch_add(count, std::sync::atomic::Ordering::Relaxed);
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
}
