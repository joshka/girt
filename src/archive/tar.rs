use std::io::{self, Write};

use crate::ObjectId;

/// Tar files are written in records of this many bytes, as Git writes them.
const RECORD: u64 = 10240;
const BLOCK: usize = 512;
/// Field widths of the POSIX ustar header.
const NAME: usize = 100;
const PREFIX: usize = 155;

/// What a tar header describes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    File,
    Directory,
    Symlink,
    GlobalHeader,
    ExtendedHeader,
}

impl Kind {
    fn flag(self) -> u8 {
        match self {
            Self::File => b'0',
            Self::Directory => b'5',
            Self::Symlink => b'2',
            Self::GlobalHeader => b'g',
            Self::ExtendedHeader => b'x',
        }
    }
}

/// Writes ustar entries with pax headers where fields overflow, counting bytes for padding.
pub(super) struct TarWriter<W> {
    out: W,
    written: u64,
    mtime: u64,
}

impl<W: Write> TarWriter<W> {
    pub(super) fn new(out: W, mtime: u64) -> Self {
        Self {
            out,
            written: 0,
            mtime,
        }
    }

    /// Writes the pax global header Git uses to record the archived commit.
    pub(super) fn commit_header(&mut self, commit: ObjectId) -> io::Result<()> {
        let record = pax_record(b"comment", commit.to_string().as_bytes());
        self.header(
            b"pax_global_header",
            0o666,
            record.len() as u64,
            Kind::GlobalHeader,
            b"",
        )?;
        self.data(&record)
    }

    /// Writes one entry's header; `id` names the pax header Git writes for overlong fields.
    pub(super) fn entry(
        &mut self,
        path: &[u8],
        mode: u32,
        size: u64,
        kind: Kind,
        link: &[u8],
        id: ObjectId,
    ) -> io::Result<()> {
        let split = split_path(path);
        let mut records = Vec::new();
        if split.is_none() {
            records.extend(pax_record(b"path", path));
        }
        if link.len() > NAME {
            records.extend(pax_record(b"linkpath", link));
        }
        if !records.is_empty() {
            let hex = id.to_string();
            self.header(
                format!("{hex}.paxheader").as_bytes(),
                0o666,
                records.len() as u64,
                Kind::ExtendedHeader,
                b"",
            )?;
            self.data(&records)?;
        }
        let data_name = format!("{id}.data");
        let (prefix, name) = split.unwrap_or((b"", data_name.as_bytes()));
        // Git points readers without pax support at the header holding the real target.
        let see = format!("see {id}.paxheader");
        let link = if link.len() > NAME {
            see.as_bytes()
        } else {
            link
        };
        let mut block = header_block(name, prefix, mode, size, self.mtime, kind, link);
        checksum(&mut block);
        self.write(&block)
    }

    /// Writes entry content and pads it to a whole block.
    pub(super) fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write(bytes)?;
        let partial = bytes.len() % BLOCK;
        if partial != 0 {
            self.write(&[0; BLOCK][partial..])?;
        }
        Ok(())
    }

    /// Writes two zero blocks and pads the archive to a whole record.
    pub(super) fn finish(mut self) -> io::Result<W> {
        let end = (self.written + 2 * BLOCK as u64).div_ceil(RECORD) * RECORD;
        let zeros = [0; BLOCK];
        while self.written < end {
            self.write(&zeros)?;
        }
        Ok(self.out)
    }

    fn header(
        &mut self,
        name: &[u8],
        mode: u32,
        size: u64,
        kind: Kind,
        link: &[u8],
    ) -> io::Result<()> {
        let mut block = header_block(name, b"", mode, size, self.mtime, kind, link);
        checksum(&mut block);
        self.write(&block)
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }
}

/// Splits `path` into ustar prefix and name fields at the last `/` that makes both fit, or
/// returns `None` when it needs a pax `path` record.
fn split_path(path: &[u8]) -> Option<(&[u8], &[u8])> {
    if path.len() <= NAME {
        return Some((b"", path));
    }
    // The prefix field is NUL-terminated as Git writes it, so it holds at most 154 bytes.
    let limit = path.len().min(PREFIX);
    let slash = path.get(..limit)?.iter().rposition(|&byte| byte == b'/')?;
    let (prefix, name) = (path.get(..slash)?, path.get(slash + 1..)?);
    (!name.is_empty() && name.len() <= NAME).then_some((prefix, name))
}

/// One pax record: `<length> <key>=<value>\n`, where the length counts the whole record.
fn pax_record(key: &[u8], value: &[u8]) -> Vec<u8> {
    let body = key.len() + value.len() + 3;
    let mut length = body + 1;
    while length != body + length.to_string().len() {
        length = body + length.to_string().len();
    }
    let mut record = length.to_string().into_bytes();
    record.push(b' ');
    record.extend_from_slice(key);
    record.push(b'=');
    record.extend_from_slice(value);
    record.push(b'\n');
    record
}

fn header_block(
    name: &[u8],
    prefix: &[u8],
    mode: u32,
    size: u64,
    mtime: u64,
    kind: Kind,
    link: &[u8],
) -> [u8; BLOCK] {
    let mut block = [0u8; BLOCK];
    let mut put = |at: usize, bytes: &[u8]| {
        if let Some(field) = block.get_mut(at..at + bytes.len()) {
            field.copy_from_slice(bytes);
        }
    };
    put(0, name);
    put(100, format!("{mode:07o}\0").as_bytes());
    put(108, b"0000000\0");
    put(116, b"0000000\0");
    put(124, format!("{size:011o}\0").as_bytes());
    put(136, format!("{mtime:011o}\0").as_bytes());
    put(156, &[kind.flag()]);
    put(157, link);
    put(257, b"ustar\x0000");
    put(265, b"root");
    put(297, b"root");
    put(329, b"0000000\0");
    put(337, b"0000000\0");
    put(345, prefix);
    block
}

fn checksum(block: &mut [u8; BLOCK]) {
    if let Some(field) = block.get_mut(148..156) {
        field.fill(b' ');
    }
    let sum: u32 = block.iter().map(|&byte| u32::from(byte)).sum();
    if let Some(field) = block.get_mut(148..156) {
        field.copy_from_slice(format!("{sum:07o}\0").as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::{pax_record, split_path};

    #[test]
    fn pax_record_length_counts_itself() {
        assert_eq!(pax_record(b"comment", b"x"), b"13 comment=x\n");
        let long = vec![b'x'; 120];
        let record = pax_record(b"path", &long);
        assert!(record.starts_with(b"130 path="));
        assert_eq!(record.len(), 130);
    }

    #[test]
    fn splits_at_the_last_fitting_slash() {
        let path = format!("{}/{}/{}", "a".repeat(40), "b".repeat(40), "c".repeat(50));
        let (prefix, name) = split_path(path.as_bytes()).unwrap();
        assert_eq!(name, "c".repeat(50).as_bytes());
        assert_eq!(prefix.len(), 81);
        assert!(split_path("x".repeat(120).as_bytes()).is_none());
    }
}
