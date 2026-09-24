//! Shared pkt-line framing; service negotiation belongs to fetch and push.
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
pub(crate) struct Wire<'a, R> {
    pub(crate) reader: &'a mut R,
    pub(crate) remaining: usize,
    pub(crate) cancel: &'a AtomicBool,
}
impl<R: Read> Wire<'_, R> {
    pub(crate) fn packet(&mut self) -> Result<Option<Vec<u8>>, Error> {
        self.charge(4)?;
        let mut header = [0; 4];
        self.exact(&mut header)?;
        if !header.iter().all(u8::is_ascii_hexdigit) {
            return Err(Error::Protocol("pkt-line header"));
        }
        let length = usize::from_str_radix(std::str::from_utf8(&header).unwrap(), 16).unwrap();
        if length == 0 {
            return Ok(None);
        }
        if !(4..=65520).contains(&length) {
            return Err(Error::Protocol("pkt-line length"));
        }
        self.charge(length - 4)?;
        let mut bytes = vec![0; length - 4];
        self.exact(&mut bytes)?;
        if let Some(message) = bytes.strip_prefix(b"ERR ") {
            return Err(Error::Remote(message.to_vec()));
        }
        Ok(Some(bytes))
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
            bytes = &mut bytes[read..];
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
        bytes = &bytes[written..];
    }
    Ok(())
}
