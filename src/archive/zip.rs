use std::io::{self, Write};

use flate2::Compression;
use flate2::write::DeflateEncoder;

/// Git reads this many leading bytes to decide whether content is binary: it is when they
/// contain NUL. Zip marks other entries as text.
const BINARY_PROBE: usize = 8000;

/// Unix permission bits for an entry, as Git records them in the zip external attributes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    File,
    Executable,
    Symlink,
    Directory,
}

/// The fields of one entry the central directory repeats.
struct Central {
    name: Vec<u8>,
    made_by: u16,
    flags: u16,
    method: u16,
    crc: u32,
    compressed: u32,
    size: u32,
    internal: u16,
    external: u32,
    offset: u32,
}

/// Writes zip entries as Git does, then the central directory with the commit as comment.
pub(super) struct ZipWriter<W> {
    out: W,
    written: u64,
    level: u32,
    mtime: u32,
    dos_time: u16,
    dos_date: u16,
    entries: Vec<Central>,
}

/// The archive does not fit the classic zip format, which girt writes without zip64 records.
#[derive(Debug)]
pub(super) struct TooLarge;

impl<W: Write> ZipWriter<W> {
    /// `local` is the modification time in the local zone, as `(year, month, day, hour, minute,
    /// second)`, for the DOS date and time fields.
    pub(super) fn new(out: W, level: u32, mtime: i64, local: (i16, i8, i8, i8, i8, i8)) -> Self {
        let (year, month, day, hour, minute, second) = local;
        let dos_date = (u16::try_from(year.saturating_sub(1980)).unwrap_or(0) << 9)
            | (u16::try_from(month).unwrap_or(0) << 5)
            | u16::try_from(day).unwrap_or(0);
        let dos_time = (u16::try_from(hour).unwrap_or(0) << 11)
            | (u16::try_from(minute).unwrap_or(0) << 5)
            | (u16::try_from(second).unwrap_or(0) / 2);
        Self {
            out,
            written: 0,
            level,
            mtime: u32::try_from(mtime).unwrap_or(0),
            dos_time,
            dos_date,
            entries: Vec::new(),
        }
    }

    /// Writes one entry with its local header. File content is deflated when that makes it
    /// smaller; symbolic links are stored. A `streamed` file is always deflated and, as Git
    /// streams it, its CRC and compressed size follow the data in a data descriptor.
    pub(super) fn entry(
        &mut self,
        path: &[u8],
        kind: Kind,
        content: &[u8],
        streamed: bool,
    ) -> io::Result<Result<(), TooLarge>> {
        let crc = crc32fast::hash(content);
        let compressible = matches!(kind, Kind::File | Kind::Executable) && !content.is_empty();
        let deflated = if self.level > 0 && compressible {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(self.level));
            encoder.write_all(content)?;
            Some(encoder.finish()?).filter(|deflated| streamed || deflated.len() < content.len())
        } else {
            None
        };
        let streamed = streamed && deflated.is_some();
        let (method, stored): (u16, &[u8]) = match &deflated {
            Some(deflated) => (8, deflated.as_slice()),
            None => (0, content),
        };
        let (Ok(size), Ok(compressed), Ok(offset), Ok(name_len)) = (
            u32::try_from(content.len()),
            u32::try_from(stored.len()),
            u32::try_from(self.written),
            u16::try_from(path.len()),
        ) else {
            return Ok(Err(TooLarge));
        };
        if self.entries.len() >= usize::from(u16::MAX) {
            return Ok(Err(TooLarge));
        }
        let mut flags: u16 = if !path.is_ascii() && std::str::from_utf8(path).is_ok() {
            1 << 11
        } else {
            0
        };
        if streamed {
            flags |= 1 << 3;
        }
        let (made_by, external) = match kind {
            Kind::File => (0, 0),
            Kind::Directory => (0, 0x10),
            Kind::Executable => (0x0317, 0o100_755 << 16),
            Kind::Symlink => (0x0317, 0o120_777 << 16),
        };
        let binary = content.get(..BINARY_PROBE).unwrap_or(content).contains(&0);
        let internal = u16::from(kind != Kind::Directory && !binary);
        let mut header = Vec::with_capacity(30 + path.len() + 9);
        header.extend_from_slice(b"PK\x03\x04");
        header.extend_from_slice(&10u16.to_le_bytes());
        header.extend_from_slice(&flags.to_le_bytes());
        header.extend_from_slice(&method.to_le_bytes());
        header.extend_from_slice(&self.dos_time.to_le_bytes());
        header.extend_from_slice(&self.dos_date.to_le_bytes());
        let (local_crc, local_compressed) = if streamed { (0, 0) } else { (crc, compressed) };
        header.extend_from_slice(&local_crc.to_le_bytes());
        header.extend_from_slice(&local_compressed.to_le_bytes());
        header.extend_from_slice(&size.to_le_bytes());
        header.extend_from_slice(&name_len.to_le_bytes());
        header.extend_from_slice(&9u16.to_le_bytes());
        header.extend_from_slice(path);
        header.extend_from_slice(&self.extra());
        self.write(&header)?;
        self.write(stored)?;
        if streamed {
            let mut descriptor = Vec::with_capacity(16);
            descriptor.extend_from_slice(b"PK\x07\x08");
            descriptor.extend_from_slice(&crc.to_le_bytes());
            descriptor.extend_from_slice(&compressed.to_le_bytes());
            descriptor.extend_from_slice(&size.to_le_bytes());
            self.write(&descriptor)?;
        }
        self.entries.push(Central {
            name: path.to_vec(),
            made_by,
            flags,
            method,
            crc,
            compressed,
            size,
            internal,
            external,
            offset,
        });
        Ok(Ok(()))
    }

    /// Writes the central directory and its end record, with `comment` (the commit ID or empty).
    pub(super) fn finish(mut self, comment: &[u8]) -> io::Result<Result<W, TooLarge>> {
        let Ok(start) = u32::try_from(self.written) else {
            return Ok(Err(TooLarge));
        };
        let entries = std::mem::take(&mut self.entries);
        for entry in &entries {
            let mut record = Vec::with_capacity(46 + entry.name.len() + 9);
            record.extend_from_slice(b"PK\x01\x02");
            record.extend_from_slice(&entry.made_by.to_le_bytes());
            record.extend_from_slice(&10u16.to_le_bytes());
            record.extend_from_slice(&entry.flags.to_le_bytes());
            record.extend_from_slice(&entry.method.to_le_bytes());
            record.extend_from_slice(&self.dos_time.to_le_bytes());
            record.extend_from_slice(&self.dos_date.to_le_bytes());
            record.extend_from_slice(&entry.crc.to_le_bytes());
            record.extend_from_slice(&entry.compressed.to_le_bytes());
            record.extend_from_slice(&entry.size.to_le_bytes());
            record.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
            record.extend_from_slice(&9u16.to_le_bytes());
            record.extend_from_slice(&0u16.to_le_bytes());
            record.extend_from_slice(&0u16.to_le_bytes());
            record.extend_from_slice(&entry.internal.to_le_bytes());
            record.extend_from_slice(&entry.external.to_le_bytes());
            record.extend_from_slice(&entry.offset.to_le_bytes());
            record.extend_from_slice(&entry.name);
            record.extend_from_slice(&self.extra());
            self.write(&record)?;
        }
        let (Ok(size), Ok(count), Ok(comment_len)) = (
            u32::try_from(self.written - u64::from(start)),
            u16::try_from(entries.len()),
            u16::try_from(comment.len()),
        ) else {
            return Ok(Err(TooLarge));
        };
        let mut end = Vec::with_capacity(22 + comment.len());
        end.extend_from_slice(b"PK\x05\x06");
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&count.to_le_bytes());
        end.extend_from_slice(&count.to_le_bytes());
        end.extend_from_slice(&size.to_le_bytes());
        end.extend_from_slice(&start.to_le_bytes());
        end.extend_from_slice(&comment_len.to_le_bytes());
        end.extend_from_slice(comment);
        self.write(&end)?;
        Ok(Ok(self.out))
    }

    /// The extended-timestamp extra field: the modification time in UTC seconds.
    fn extra(&self) -> [u8; 9] {
        let mut extra = [0u8; 9];
        extra[..2].copy_from_slice(&0x5455u16.to_le_bytes());
        extra[2..4].copy_from_slice(&5u16.to_le_bytes());
        extra[4] = 1;
        extra[5..].copy_from_slice(&self.mtime.to_le_bytes());
        extra
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }
}
