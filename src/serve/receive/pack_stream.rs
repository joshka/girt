//! Reads exactly one pack from a stream that the client keeps open after sending it.
//!
//! The pack's end can only be found by parsing it: each entry's compressed data runs until its
//! zlib stream ends. The raw bytes are spooled unchanged for validation and installation.

use std::io::Read;
use std::sync::atomic::AtomicBool;

use flate2::{Decompress, FlushDecompress, Status};

use crate::ObjectFormat;
use crate::fetch::Bytes;
use crate::packet::check_cancelled;
use crate::serve::ServeError;

/// Bounds on one received pack.
pub(super) struct Bounds {
    pub max_pack_bytes: u64,
    pub max_objects: u32,
    pub max_object_bytes: u64,
    /// Inflated bytes across all entries.
    pub max_input_bytes: u64,
}

struct Spool<'a, R> {
    input: &'a mut R,
    buffer: Vec<u8>,
    start: usize,
    end: usize,
    out: Bytes,
    total: u64,
    inflated: u64,
    bounds: &'a Bounds,
    cancel: &'a AtomicBool,
}

impl<R: Read> Spool<'_, R> {
    /// Makes at least one unread byte available.
    fn fill(&mut self) -> Result<(), ServeError> {
        if self.start < self.end {
            return Ok(());
        }
        check_cancelled(self.cancel)?;
        let read = loop {
            match self.input.read(&mut self.buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                result => break result?,
            }
        };
        if read == 0 {
            return Err(ServeError::Protocol("truncated pack"));
        }
        self.start = 0;
        self.end = read;
        Ok(())
    }

    /// Moves `count` buffered bytes into the spool.
    fn consume(&mut self, count: usize) -> Result<(), ServeError> {
        self.total += count as u64;
        if self.total > self.bounds.max_pack_bytes {
            return Err(ServeError::Limit("pack bytes"));
        }
        let bytes = self
            .buffer
            .get(self.start..self.start + count)
            .ok_or(ServeError::Protocol("pack"))?;
        self.out
            .append(bytes)
            .map_err(|_| ServeError::Io(std::io::Error::other("spooling the pack")))?;
        self.start += count;
        Ok(())
    }

    fn byte(&mut self) -> Result<u8, ServeError> {
        self.fill()?;
        let byte = *self
            .buffer
            .get(self.start)
            .ok_or(ServeError::Protocol("pack"))?;
        self.consume(1)?;
        Ok(byte)
    }

    fn skip(&mut self, mut count: usize) -> Result<Vec<u8>, ServeError> {
        let mut bytes = Vec::with_capacity(count);
        while count > 0 {
            self.fill()?;
            let take = count.min(self.end - self.start);
            bytes.extend_from_slice(
                self.buffer
                    .get(self.start..self.start + take)
                    .ok_or(ServeError::Protocol("pack"))?,
            );
            self.consume(take)?;
            count -= take;
        }
        Ok(bytes)
    }

    /// Consumes one zlib stream, checking that it inflates to `size` bytes.
    fn inflate(&mut self, size: u64) -> Result<(), ServeError> {
        if size > self.bounds.max_object_bytes {
            return Err(ServeError::Limit("object bytes"));
        }
        // Entries inflate to their declared size or fail, so the declared sizes bound the work.
        self.inflated = self
            .inflated
            .checked_add(size)
            .filter(|&total| total <= self.bounds.max_input_bytes)
            .ok_or(ServeError::Limit("inflated pack bytes"))?;
        let mut inflater = Decompress::new(true);
        let mut scratch = vec![0; 64 * 1024];
        loop {
            self.fill()?;
            let before_in = inflater.total_in();
            let before_out = inflater.total_out();
            let input = self
                .buffer
                .get(self.start..self.end)
                .ok_or(ServeError::Protocol("pack"))?;
            let status = inflater
                .decompress(input, &mut scratch, FlushDecompress::None)
                .map_err(|_| ServeError::Protocol("corrupt pack entry"))?;
            let consumed = usize::try_from(inflater.total_in() - before_in)
                .map_err(|_| ServeError::Protocol("pack"))?;
            self.consume(consumed)?;
            if inflater.total_out() > size {
                return Err(ServeError::Protocol("pack entry larger than declared"));
            }
            match status {
                Status::StreamEnd => break,
                Status::Ok | Status::BufError => {
                    if consumed == 0 && inflater.total_out() == before_out && self.start < self.end
                    {
                        return Err(ServeError::Protocol("corrupt pack entry"));
                    }
                }
            }
        }
        if inflater.total_out() != size {
            return Err(ServeError::Protocol("pack entry size mismatch"));
        }
        Ok(())
    }
}

/// Spools one pack from `input`, stopping right after its trailing checksum.
///
/// Bytes already read past the checksum are a protocol error, since a client sends nothing more
/// until the status report.
///
/// The structure is checked only as far as finding the end; the receiver validates the content.
pub(super) fn read_pack(
    input: &mut impl Read,
    format: ObjectFormat,
    bounds: &Bounds,
    cancel: &AtomicBool,
) -> Result<Bytes, ServeError> {
    let out = Bytes::temporary()
        .map_err(|_| ServeError::Io(std::io::Error::other("creating the pack spool")))?;
    let mut spool = Spool {
        input,
        buffer: vec![0; 64 * 1024],
        start: 0,
        end: 0,
        out,
        total: 0,
        inflated: 0,
        bounds,
        cancel,
    };
    let header = spool.skip(12)?;
    let (magic, rest) = header.split_at(4);
    if magic != b"PACK" {
        return Err(ServeError::Protocol("pack signature"));
    }
    let (version, count) = rest.split_at(4);
    if !matches!(version, [0, 0, 0, 2 | 3]) {
        return Err(ServeError::Protocol("pack version"));
    }
    let count = u32::from_be_bytes(
        count
            .try_into()
            .map_err(|_| ServeError::Protocol("pack header"))?,
    );
    if count > bounds.max_objects {
        return Err(ServeError::Limit("pack objects"));
    }
    let id_bytes = match format {
        ObjectFormat::Sha1 => 20,
        ObjectFormat::Sha256 => 32,
    };
    for _ in 0..count {
        let mut byte = spool.byte()?;
        let kind = (byte >> 4) & 7;
        let mut size = u64::from(byte & 0x0f);
        let mut shift = 4;
        while byte & 0x80 != 0 {
            byte = spool.byte()?;
            if shift > 56 {
                return Err(ServeError::Protocol("pack entry size"));
            }
            size |= u64::from(byte & 0x7f) << shift;
            shift += 7;
        }
        match kind {
            1..=4 => {}
            6 => {
                // An offset to the base: a big-endian varint with continuation bits.
                let mut length = 0;
                loop {
                    length += 1;
                    if length > 10 {
                        return Err(ServeError::Protocol("pack delta offset"));
                    }
                    if spool.byte()? & 0x80 == 0 {
                        break;
                    }
                }
            }
            7 => {
                spool.skip(id_bytes)?;
            }
            _ => return Err(ServeError::Protocol("pack entry type")),
        }
        spool.inflate(size)?;
    }
    spool.skip(id_bytes)?;
    // Nothing follows a pack: the client waits for the status report.
    if spool.start < spool.end {
        return Err(ServeError::Protocol("data after the pack"));
    }
    Ok(spool.out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ObjectId, ObjectKind, PackObject, PackWriteLimits, write_pack};

    fn pack(format: ObjectFormat, payloads: &[&[u8]]) -> Vec<u8> {
        let objects: Vec<_> = payloads
            .iter()
            .map(|data| PackObject {
                id: ObjectId::for_blob(format, data),
                kind: ObjectKind::Blob,
                data,
            })
            .collect();
        let (mut pack, mut index) = (Vec::new(), Vec::new());
        write_pack(
            format,
            &objects,
            &mut pack,
            &mut index,
            PackWriteLimits::default(),
        )
        .unwrap();
        pack
    }

    fn bounds() -> Bounds {
        Bounds {
            max_pack_bytes: 1 << 20,
            max_objects: 100,
            max_object_bytes: 1 << 20,
            max_input_bytes: 1 << 20,
        }
    }

    fn read(input: &[u8], format: ObjectFormat, bounds: &Bounds) -> Result<Vec<u8>, ServeError> {
        let mut input = input;
        read_pack(&mut input, format, bounds, &AtomicBool::new(false)).map(|bytes| bytes.to_vec())
    }

    #[test]
    fn spools_the_whole_pack() {
        for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
            let pack = pack(format, &[b"one", b"two", &[7; 100_000]]);
            assert_eq!(read(&pack, format, &bounds()).unwrap(), pack);
            let mut trailing = pack.clone();
            trailing.extend_from_slice(b"0000");
            assert!(matches!(
                read(&trailing, format, &bounds()),
                Err(ServeError::Protocol("data after the pack"))
            ));
        }
    }

    #[test]
    fn refuses_truncated_and_oversized_packs() {
        let pack = pack(ObjectFormat::Sha1, &[b"one", b"two"]);
        let truncated = read(&pack[..pack.len() - 1], ObjectFormat::Sha1, &bounds());
        assert!(matches!(
            truncated,
            Err(ServeError::Protocol("truncated pack"))
        ));
        let small = Bounds {
            max_pack_bytes: 20,
            ..bounds()
        };
        assert!(matches!(
            read(&pack, ObjectFormat::Sha1, &small),
            Err(ServeError::Limit(_))
        ));
        let few = Bounds {
            max_objects: 1,
            ..bounds()
        };
        let inflated = Bounds {
            max_input_bytes: 4,
            ..bounds()
        };
        assert!(matches!(
            read(&pack, ObjectFormat::Sha1, &inflated),
            Err(ServeError::Limit("inflated pack bytes"))
        ));
        assert!(matches!(
            read(&pack, ObjectFormat::Sha1, &few),
            Err(ServeError::Limit(_))
        ));
    }

    #[test]
    fn refuses_a_bad_header() {
        let mut pack = pack(ObjectFormat::Sha1, &[b"one"]);
        pack[0] = b'X';
        assert!(matches!(
            read(&pack, ObjectFormat::Sha1, &bounds()),
            Err(ServeError::Protocol("pack signature"))
        ));
    }
}
