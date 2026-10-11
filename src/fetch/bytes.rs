//! Owned transfer artifacts. Disk-backed values remove files on drop and open independent cursors.
use std::io::{Read, Seek, SeekFrom, Write};

use super::{FetchError, check_cancelled};

#[derive(Debug)]
pub(crate) enum Bytes {
    Memory(Vec<u8>),
    File(tempfile::NamedTempFile, usize),
}

impl Bytes {
    pub(crate) fn temporary() -> Result<Self, FetchError> {
        Ok(Self::File(tempfile::NamedTempFile::new()?, 0))
    }

    pub(crate) fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::File(file, _) => Some(file.path()),
            Self::Memory(_) => None,
        }
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Memory(bytes) => bytes.len(),
            Self::File(_, len) => *len,
        }
    }

    pub(crate) fn reader(&self) -> Result<Box<dyn Read + Send + '_>, FetchError> {
        match self {
            Self::Memory(bytes) => Ok(Box::new(bytes.as_slice())),
            Self::File(file, _) => Ok(Box::new(std::io::BufReader::new(file.reopen()?))),
        }
    }

    pub(crate) fn copy_to(
        &self,
        output: &mut impl Write,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), FetchError> {
        let mut reader = self.reader()?;
        let mut buffer = [0; 64 * 1024];
        loop {
            check_cancelled(cancel)?;
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                return Ok(());
            }
            output.write_all(&buffer[..count])?;
        }
    }

    pub(crate) fn append(&mut self, bytes: &[u8]) -> Result<(), FetchError> {
        match self {
            Self::Memory(data) => data.extend_from_slice(bytes),
            Self::File(file, len) => {
                file.as_file_mut().seek(SeekFrom::Start(*len as u64))?;
                file.write_all(bytes)?;
                *len += bytes.len();
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn to_vec(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.reader().unwrap().read_to_end(&mut bytes).unwrap();
        bytes
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Memory(bytes)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    #[test]
    fn readers_have_independent_cursors_and_drop_removes_the_file() {
        let mut bytes = Bytes::temporary().unwrap();
        bytes.append(b"payload").unwrap();
        let path = bytes.path().unwrap().to_owned();
        let mut first = bytes.reader().unwrap();
        let mut prefix = [0; 3];
        first.read_exact(&mut prefix).unwrap();
        assert_eq!(&prefix, b"pay");
        assert_eq!(bytes.to_vec(), b"payload");
        drop(first);
        drop(bytes);
        assert!(!path.exists());
    }

    #[test]
    fn cancelled_installation_copy_writes_nothing() {
        let mut bytes = Bytes::temporary().unwrap();
        bytes.append(b"payload").unwrap();
        let mut output = Vec::new();
        assert!(matches!(
            bytes.copy_to(&mut output, &AtomicBool::new(true)),
            Err(FetchError::Cancelled)
        ));
        assert!(output.is_empty());
    }
}
