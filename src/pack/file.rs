//! Pinned file pairs with bounded offset tables and streaming pack validation.
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use super::index::{Entry, Index, word};
use super::reader::{Source, check_cancelled, decode};
use crate::object::Hasher;
use crate::{Object, ObjectFormat, ObjectId, ObjectReadError as Error, ReadLimits};

/// Serializes short seek/read operations without cloning descriptors or sharing cursor state.
#[derive(Debug)]
struct Artifact {
    file: Mutex<File>,
    path: PathBuf,
    len: usize,
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
        })
    }

    fn read_at(&self, offset: usize, output: &mut [u8]) -> Result<(), Error> {
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
        validate_pack(&pack, &index, cancelled)?;
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

    #[cfg(unix)]
    pub(crate) fn checksum(&self) -> ObjectId {
        let width = self.format.digest_len();
        let end = self.index_bytes.len() - width;
        ObjectId::from_bytes(self.format, &self.index_bytes[end - width..end]).unwrap()
    }

    pub(crate) fn identities(&self) -> Result<Vec<ObjectId>, Error> {
        self.index.check_not_truncated()?;
        (0..self.ranges.len())
            .map(|position| self.identity(position))
            .collect()
    }

    pub(crate) fn path(&self) -> &Path {
        &self.index.path
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

    /// Returns framing and a pinned compressed entry without inflating its payload.
    pub(crate) fn encoded(&self, position: usize) -> Result<EncodedEntry, Error> {
        let entry = self.entry(position)?;
        let prefix = self.prefix(&entry)?;
        let mut input = prefix.as_slice();
        let first = super::delta::byte(&mut input)?;
        let code = (first >> 4) & 7;
        let _length = super::delta::size(&mut input, (first & 15) as usize, 4, first & 128 != 0)?;
        let header_len = prefix.len() - input.len();
        let base = match code {
            1..=4 => None,
            6 => {
                let mut next = super::delta::byte(&mut input)?;
                let mut distance = (next & 127) as usize;
                while next & 128 != 0 {
                    next = super::delta::byte(&mut input)?;
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
                Some(self.identity(self.at_offset(offset)?)?)
            }
            7 => {
                let width = self.format.digest_len();
                let raw = input
                    .get(..width)
                    .ok_or(Error::Corrupt("truncated base identity"))?;
                input = &input[width..];
                let id = ObjectId::from_bytes(self.format, raw).unwrap();
                self.find(id)?.ok_or(Error::MissingBase(id))?;
                Some(id)
            }
            other => return Err(Error::ObjectType(other)),
        };
        let payload = entry.offset + prefix.len() - input.len();
        let mut header = prefix[..header_len].to_vec();
        if code == 6 {
            // REF_DELTA survives arbitrary movement, retaining the exact zlib program bytes.
            header[0] = (header[0] & !0x70) | 0x70;
            header.extend_from_slice(base.unwrap().as_bytes());
        } else if code == 7 {
            header.extend_from_slice(base.unwrap().as_bytes());
        }
        Ok(EncodedEntry {
            base,
            ofs: code == 6,
            offset: entry.offset,
            header,
            payload,
            end: entry.end,
        })
    }

    pub(crate) fn copy_encoded(
        &self,
        entry: &EncodedEntry,
        output: &mut (impl io::Write + ?Sized),
        cancel: &AtomicBool,
    ) -> Result<(), super::write::PackWriteError> {
        let mut offset = entry.payload;
        let mut buffer = [0; 64 * 1024];
        while offset < entry.end {
            check_cancelled(cancel).map_err(io::Error::other)?;
            let n = buffer.len().min(entry.end - offset);
            self.pack
                .read_at(offset, &mut buffer[..n])
                .map_err(io::Error::other)?;
            output
                .write_all(&buffer[..n])
                .map_err(super::write::output_error)?;
            offset += n;
        }
        Ok(())
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

pub(crate) struct EncodedEntry {
    pub base: Option<ObjectId>,
    pub ofs: bool,
    pub offset: usize,
    pub header: Vec<u8>,
    payload: usize,
    end: usize,
}

impl EncodedEntry {
    pub(crate) fn relocated_header(&self, offset: u64, base_offset: Option<u64>) -> Vec<u8> {
        let mut header = self.header.clone();
        if self.ofs
            && let Some(base_offset) = base_offset
        {
            header[0] = (header[0] & !0x70) | 0x60;
            header.truncate(header.len() - self.base.unwrap().format().digest_len());
            let mut distance = offset - base_offset;
            let mut encoded = vec![(distance & 127) as u8];
            while distance >= 128 {
                distance = (distance >> 7) - 1;
                encoded.push((distance as u8 & 127) | 128);
            }
            header.extend(encoded.into_iter().rev());
        }
        header
    }
}

fn validate_pack(pack: &Artifact, index: &Index, cancelled: &AtomicBool) -> Result<(), Error> {
    let format = index.pack_hash.format();
    let end = pack.len - format.digest_len();
    let mut hash = Hasher::new(format);
    hash.update(&pack.bytes(0, 12)?);
    let mut buffer = [0; 64 * 1024];
    for &position in &index.offsets {
        let entry = &index.entries[position];
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
    if index.pack_hash.as_bytes() != trailer {
        return Err(Error::Corrupt("index/pack checksum disagreement"));
    }
    check_cancelled(cancelled)
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
            crc: None,
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
        start: usize,
        end: usize,
        expected: usize,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>, Error> {
        let input = Range {
            artifact: &self.pack,
            position: start,
            end,
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
        Ok(data)
    }
}

/// A bounded cursor whose reads never disturb another lookup's logical position.
struct Range<'a> {
    artifact: &'a Artifact,
    position: usize,
    end: usize,
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
    #[case::reserved(&[0x50, 0])]
    #[case::truncated_size(&[0xb0])]
    #[case::zero_distance(&[0x60, 0])]
    #[case::before_pack(&[0x60, 127])]
    #[case::truncated_reference(&[0x70, 1])]
    #[case::missing_reference(&[0x70, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])]
    fn encoded_entry_rejects_malformed_framing(#[case] framing: &[u8]) {
        let root = tempfile::tempdir().unwrap();
        let path = malformed_pair(root.path(), framing);
        let pack = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(pack.encoded(0).is_err());
    }

    #[test]
    fn ofs_delta_reframing_preserves_compressed_program_verbatim() {
        use std::io::Write;

        use flate2::write::ZlibEncoder;
        let root = tempfile::tempdir().unwrap();
        let format = ObjectFormat::Sha1;
        let base_id = ObjectId::for_blob(format, b"abcd");
        let target_id = ObjectId::for_blob(format, b"abcX");
        let mut encoder = ZlibEncoder::new(vec![0x34], flate2::Compression::new(6));
        encoder.write_all(b"abcd").unwrap();
        let base = encoder.finish().unwrap();
        let mut encoder = ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        encoder
            .write_all(&[4, 4, 3, b'a', b'b', b'c', 1, b'X'])
            .unwrap();
        let compressed = encoder.finish().unwrap();
        let mut delta = vec![0x68, base.len() as u8];
        delta.extend_from_slice(&compressed);
        let mut bytes = b"PACK\0\0\0\x02\0\0\0\x02".to_vec();
        bytes.extend_from_slice(&base);
        bytes.extend_from_slice(&delta);
        let checksum = format.checksum(&bytes);
        bytes.extend_from_slice(checksum.as_bytes());
        let mut entries = vec![
            crate::pack::write::Entry {
                id: base_id,
                offset: 12,
                crc: crc32fast::hash(&base),
            },
            crate::pack::write::Entry {
                id: target_id,
                offset: 12 + base.len() as u64,
                crc: crc32fast::hash(&delta),
            },
        ];
        entries.sort_unstable_by_key(|entry| entry.id);
        let path = root.path().join("ofs.idx");
        fs::write(
            &path,
            crate::pack::write::encode_index(&entries, checksum).unwrap(),
        )
        .unwrap();
        fs::write(path.with_extension("pack"), bytes).unwrap();
        let pack = FilePack::open(
            format,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let position = pack.find(target_id).unwrap().unwrap();
        assert_eq!(
            pack.read(position, ReadLimits::default(), &AtomicBool::new(false))
                .unwrap()
                .data(),
            b"abcX"
        );
        let entry = pack.encoded(position).unwrap();
        assert_eq!(entry.base, Some(base_id));
        assert_eq!(entry.header[0], 0x78);
        assert_eq!(&entry.header[1..], base_id.as_bytes());
        let mut copied = Vec::new();
        pack.copy_encoded(&entry, &mut copied, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(copied, compressed);
        assert_eq!(
            entry.relocated_header(12 + base.len() as u64, Some(12)),
            delta[..2]
        );
    }

    #[rstest]
    #[case::one_byte(127, &[0x7f])]
    #[case::two_bytes(128, &[0x80, 0])]
    #[case::carry(256, &[0x81, 0])]
    #[case::three_bytes(16512, &[0x80, 0x80, 0])]
    fn relocated_ofs_distance_vectors(#[case] distance: u64, #[case] expected: &[u8]) {
        let base = ObjectId::for_blob(ObjectFormat::Sha1, b"base");
        let mut header = vec![0x78];
        header.extend_from_slice(base.as_bytes());
        let entry = EncodedEntry {
            base: Some(base),
            ofs: true,
            offset: 12,
            header,
            payload: 13,
            end: 14,
        };
        let header = entry.relocated_header(12 + distance, Some(12));
        assert_eq!(header[0], 0x68);
        assert_eq!(&header[1..], expected);
    }

    #[test]
    fn ordinary_compressed_entry_is_copied_verbatim() {
        let root = tempfile::tempdir().unwrap();
        let path = pair(
            root.path(),
            "verbatim",
            ObjectFormat::Sha1,
            b"original payload",
        );
        let pack = FilePack::open(
            ObjectFormat::Sha1,
            &path,
            &mut PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let entry = pack.encoded(0).unwrap();
        let mut copied = entry.header.clone();
        pack.copy_encoded(&entry, &mut copied, &AtomicBool::new(false))
            .unwrap();
        let bytes = fs::read(path.with_extension("pack")).unwrap();
        assert_eq!(copied, bytes[12..bytes.len() - 20]);
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
