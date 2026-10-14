//! Shared pkt-line framing; service negotiation belongs to fetch and push.

#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub(crate) enum Error {
    Io(std::io::Error),
    Protocol(&'static str),
    Limit(&'static str),
    Cancelled,
    Remote(Vec<u8>),
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
pub(crate) fn check_cancelled(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
/// One decoded pkt-line, including the protocol v2 special packets.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    /// `0000`: ends a message or section list.
    Flush,
    /// `0001`: separates sections of one v2 message.
    Delim,
    /// `0002`: ends a v2 response on stateless connections.
    ResponseEnd,
    /// A packet carrying its payload, which may be empty.
    Data(Vec<u8>),
}

pub(crate) struct Wire<'a, R> {
    pub(crate) reader: &'a mut R,
    pub(crate) remaining: usize,
    pub(crate) cancel: &'a AtomicBool,
}
impl<R: Read> Wire<'_, R> {
    /// Reads one v0/v1 packet, where only flush is special.
    pub(crate) fn packet(&mut self) -> Result<Option<Vec<u8>>, Error> {
        match self.frame()? {
            Frame::Flush => Ok(None),
            Frame::Data(bytes) => Ok(Some(bytes)),
            Frame::Delim | Frame::ResponseEnd => Err(Error::Protocol("pkt-line length")),
        }
    }

    /// Reads one packet, distinguishing the v2 delimiter and response-end packets from flush.
    pub(crate) fn frame(&mut self) -> Result<Frame, Error> {
        self.charge(4)?;
        let mut header = [0; 4];
        self.exact(&mut header)?;
        let length = match frame_length(&header)? {
            0 => return Ok(Frame::Flush),
            1 => return Ok(Frame::Delim),
            2 => return Ok(Frame::ResponseEnd),
            length => length,
        };
        self.charge(length - 4)?;
        let mut bytes = vec![0; length - 4];
        self.exact(&mut bytes)?;
        if let Some(message) = bytes.strip_prefix(b"ERR ") {
            return Err(Error::Remote(message.to_vec()));
        }
        Ok(Frame::Data(bytes))
    }
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or(Error::Limit("wire bytes"))?;
        Ok(())
    }
    fn exact(&mut self, mut bytes: &mut [u8]) -> Result<(), Error> {
        while !bytes.is_empty() {
            check_cancelled(self.cancel)?;
            let read = self.reader.read(bytes)?;
            if read == 0 {
                return Err(Error::Protocol("truncated pkt-line"));
            }
            bytes = bytes
                .get_mut(read..)
                .ok_or(Error::Protocol("reader overran its buffer"))?;
        }
        Ok(())
    }
    pub(crate) fn end(&mut self) -> Result<(), Error> {
        check_cancelled(self.cancel)?;
        if self.reader.read(&mut [0])? != 0 {
            return Err(Error::Protocol("trailing response bytes"));
        }
        Ok(())
    }
}

/// Decodes a complete header, including flush, using the shared pkt-line length bounds.
///
/// Only the live sideband scanners need a bare header decoder; readers use [`Wire`].
#[cfg(any(
    test,
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
pub(crate) fn packet_length(header: &[u8; 4]) -> Result<usize, Error> {
    match frame_length(header)? {
        1 | 2 => Err(Error::Protocol("pkt-line length")),
        length => Ok(length),
    }
}

/// Decodes a header, returning 0, 1 and 2 for flush, delimiter and response-end packets.
fn frame_length(header: &[u8; 4]) -> Result<usize, Error> {
    let length = std::str::from_utf8(header)
        .ok()
        .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .and_then(|digits| usize::from_str_radix(digits, 16).ok())
        .ok_or(Error::Protocol("pkt-line header"))?;
    if length > 2 && !(4..=65520).contains(&length) {
        return Err(Error::Protocol("pkt-line length"));
    }
    Ok(length)
}

pub(crate) fn packet(
    writer: &mut impl Write,
    bytes: &[u8],
    cancel: &AtomicBool,
) -> Result<(), Error> {
    if bytes.len() > 65516 {
        return Err(Error::Protocol("pkt-line output length"));
    }
    put(
        writer,
        format!("{:04x}", bytes.len() + 4).as_bytes(),
        cancel,
    )?;
    put(writer, bytes, cancel)
}
pub(crate) fn put(
    writer: &mut impl Write,
    mut bytes: &[u8],
    cancel: &AtomicBool,
) -> Result<(), Error> {
    while !bytes.is_empty() {
        check_cancelled(cancel)?;
        let written = writer.write(bytes)?;
        if written == 0 {
            return Err(Error::Io(std::io::ErrorKind::WriteZero.into()));
        }
        bytes = bytes
            .get(written..)
            .ok_or(Error::Io(std::io::ErrorKind::InvalidData.into()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::sign(b"+fff", "pkt-line header")]
    #[case::non_hex(b"zzzz", "pkt-line header")]
    #[case::non_ascii(b"\xff000", "pkt-line header")]
    #[case::too_short(b"0003", "pkt-line length")]
    #[case::too_long(b"fff1", "pkt-line length")]
    fn rejects_malformed_header(#[case] header: &[u8; 4], #[case] reason: &str) {
        assert!(matches!(packet_length(header), Err(Error::Protocol(actual)) if actual == reason));
    }

    struct CountingReader<'a> {
        cursor: std::io::Cursor<&'a [u8]>,
        reads: usize,
    }

    impl Read for CountingReader<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            self.cursor.read(bytes)
        }
    }

    #[rstest]
    #[case::non_hex(b"zzzznext", "pkt-line header", 4, 1)]
    #[case::too_short(b"0003next", "pkt-line length", 4, 1)]
    #[case::too_long(b"fff1next", "pkt-line length", 4, 1)]
    #[case::delimiter(b"0001next", "pkt-line length", 4, 1)]
    #[case::response_end(b"0002next", "pkt-line length", 4, 1)]
    #[case::truncated_payload(b"0008ab", "truncated pkt-line", 6, 3)]
    fn rejects_malformed_packet_without_reading_further(
        #[case] input: &[u8],
        #[case] reason: &str,
        #[case] consumed: u64,
        #[case] reads: usize,
    ) {
        let cancel = AtomicBool::new(false);
        let mut reader = CountingReader {
            cursor: std::io::Cursor::new(input),
            reads: 0,
        };
        let mut wire = Wire {
            reader: &mut reader,
            remaining: 65520,
            cancel: &cancel,
        };
        assert!(matches!(wire.packet(), Err(Error::Protocol(actual)) if actual == reason));
        assert_eq!(reader.cursor.position(), consumed);
        assert_eq!(reader.reads, reads);
    }

    #[rstest]
    #[case::flush(b"0000", 0)]
    #[case::empty(b"0004", 4)]
    #[case::upper(b"FFF0", 0xfff0)]
    fn decodes_valid_header(#[case] header: &[u8; 4], #[case] length: usize) {
        assert!(matches!(packet_length(header), Ok(actual) if actual == length));
    }

    #[rstest]
    #[case::delim(b"0001")]
    #[case::response_end(b"0002")]
    fn v0_packets_reject_v2_special_packets(#[case] header: &[u8; 4]) {
        assert!(matches!(
            packet_length(header),
            Err(Error::Protocol("pkt-line length"))
        ));
        let cancel = AtomicBool::new(false);
        let mut reader = header.as_slice();
        let mut wire = Wire {
            reader: &mut reader,
            remaining: 4,
            cancel: &cancel,
        };
        assert!(matches!(
            wire.packet(),
            Err(Error::Protocol("pkt-line length"))
        ));
    }

    #[test]
    fn frames_distinguish_v2_special_packets() {
        let cancel = AtomicBool::new(false);
        let mut reader = b"0000000100020004000ahello\n0003".as_slice();
        let mut wire = Wire {
            reader: &mut reader,
            remaining: 64,
            cancel: &cancel,
        };
        assert_eq!(wire.frame().unwrap(), Frame::Flush);
        assert_eq!(wire.frame().unwrap(), Frame::Delim);
        assert_eq!(wire.frame().unwrap(), Frame::ResponseEnd);
        assert_eq!(wire.frame().unwrap(), Frame::Data(Vec::new()));
        assert_eq!(wire.frame().unwrap(), Frame::Data(b"hello\n".to_vec()));
        assert!(matches!(
            wire.frame(),
            Err(Error::Protocol("pkt-line length"))
        ));
    }
}
