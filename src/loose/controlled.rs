//! Capped compressed input for maintenance's explicitly controlled reads.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Error, LooseObjects, decompress, split_header};
use crate::source_files::{self, FileError};
use crate::{Object, ObjectId, ObjectReadError, ReadLimits};

impl LooseObjects {
    /// Checks compressed input before inflation; the public loose readers keep their contract.
    pub(crate) fn read_capped(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancelled: &AtomicBool,
    ) -> Result<Object, ObjectReadError> {
        check(cancelled)?;
        id.require_format(self.format).map_err(Error::from)?;
        let path = self.object_path(id);
        let input = source_files::read_optional(&path, limits.max_input_bytes)
            .map_err(|error| match error {
                FileError::TooLarge { .. } => ObjectReadError::Limit("loose compressed bytes"),
                FileError::NotFile => ObjectReadError::Unsupported("non-regular loose object"),
                FileError::Io { path, source } => ObjectReadError::Path { path, source },
            })?
            .ok_or_else(|| Error::Io(io::Error::from(io::ErrorKind::NotFound)))?;
        check(cancelled)?;
        let maximum = limits.max_object_bytes.min(limits.max_decode_bytes);
        let reader = CancelRead {
            input: input.as_slice(),
            cancelled,
        };
        let result = decompress(reader, maximum.saturating_add(32));
        check(cancelled)?;
        let mut encoded = result.map_err(|error| match error {
            Error::TooLarge => ObjectReadError::Limit("object bytes"),
            error => error.into(),
        })?;
        let (kind, content) = split_header(&encoded, None)?;
        if content.len() > maximum {
            return Err(ObjectReadError::Limit("object bytes"));
        }
        if self.format.hash_object(kind, content) != id {
            return Err(Error::Corrupt("object identity mismatch").into());
        }
        let header = encoded.len() - content.len();
        encoded.drain(..header);
        check(cancelled)?;
        Ok(Object {
            kind,
            format: self.format,
            data: encoded.into(),
        })
    }
}

/// The inflater requests input in bounded chunks, including streams with no decoded output.
struct CancelRead<'a> {
    input: &'a [u8],
    cancelled: &'a AtomicBool,
}

impl Read for CancelRead<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::other("loose read cancelled"));
        }
        self.input.read(output)
    }
}

fn check(cancelled: &AtomicBool) -> Result<(), ObjectReadError> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(ObjectReadError::Cancelled);
    }
    Ok(())
}
