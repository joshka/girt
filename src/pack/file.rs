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
}

fn path_error(path: &Path, source: io::Error) -> Error {
    Error::Path {
        path: path.to_owned(),
        source,
    }
}

/// Identity and CRC tables stay on disk; only entry ranges and offset order remain in memory.
#[derive(Debug)]
pub(crate) struct FilePack {
    index: Artifact,
    pack: Artifact,
    format: ObjectFormat,
    legacy: bool,
    ranges: Vec<(usize, usize)>,
    offsets: Vec<usize>,
}

impl FilePack {
    pub(crate) fn open(
        format: ObjectFormat,
        path: &Path,
        budget: &mut crate::PackLimits,
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        check_cancelled(cancelled)?;
        let index_file = Artifact::open(path)?;
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
        drop(index_bytes);
        validate_pack(&pack, &index, cancelled)?;
        let ranges = index
            .entries
            .iter()
            .map(|entry| (entry.offset, entry.end))
            .collect();
        Ok(Self {
            index: index_file,
            pack,
            format,
            legacy,
            ranges,
            offsets: index.offsets,
        })
    }

    pub(crate) fn find(&self, id: ObjectId) -> Result<Option<usize>, Error> {
        if id.format() != self.format {
            return Ok(None);
        }
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
        let mut bytes = [0; 32];
        self.index.read_at(start, &mut bytes[..width])?;
        Ok(ObjectId::from_bytes(self.format, &bytes[..width]).unwrap())
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
        let mut output = [0; 8192];
        let mut data = Vec::new();
        loop {
            check_cancelled(cancelled)?;
            let bytes = input
                .fill_buf()
                .map_err(|source| path_error(&self.pack.path, source))?;
            let before_in = decoder.total_in();
            let before_out = decoder.total_out();
            let status = decoder
                .decompress(bytes, &mut output, flate2::FlushDecompress::None)
                .map_err(|_| Error::Corrupt("invalid packed zlib stream"))?;
            let consumed = (decoder.total_in() - before_in) as usize;
            let count = (decoder.total_out() - before_out) as usize;
            input.consume(consumed);
            if count > expected.saturating_sub(data.len()) {
                return Err(Error::Corrupt("inflated entry exceeds declared size"));
            }
            data.extend_from_slice(&output[..count]);
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
