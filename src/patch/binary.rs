//! Forward/reverse Git binary blocks from admitted blob contents.
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use flate2::Compression;
use flate2::write::ZlibEncoder;

use super::Error;

const ALPHABET: &[u8; 85] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";

pub(super) fn write(
    out: &mut impl Write,
    old: &[u8],
    new: &[u8],
    max_work: usize,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    check(cancel)?;
    writeln!(out, "GIT binary patch")?;
    let mut remaining = max_work as u64;
    block(out, old, new, &mut remaining, cancel)?;
    block(out, new, old, &mut remaining, cancel)
}

fn block(
    out: &mut impl Write,
    base: &[u8],
    target: &[u8],
    remaining: &mut u64,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    let literal = compress(target, cancel)?;
    // Delta offsets have four bytes. Larger admitted blobs remain representable as literals.
    let delta = if base.len() <= u32::MAX as usize && target.len() <= u32::MAX as usize {
        crate::pack::binary_delta(base, target, remaining, &mut || check(cancel))?
    } else {
        None
    };
    let compressed_delta = delta
        .as_deref()
        .map(|program| compress(program, cancel))
        .transpose()?;
    let (kind, size, bytes) = match (delta.as_deref(), compressed_delta.as_deref()) {
        (Some(program), Some(bytes)) if bytes.len() < literal.len() => {
            ("delta", program.len(), bytes)
        }
        _ => ("literal", target.len(), literal.as_slice()),
    };

    writeln!(out, "{kind} {size}")?;
    for chunk in bytes.chunks(52) {
        check(cancel)?;
        line(out, chunk)?;
    }
    writeln!(out)?;
    Ok(())
}

fn compress(bytes: &[u8], cancel: &AtomicBool) -> Result<Vec<u8>, Error> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    for chunk in bytes.chunks(64 * 1024) {
        check(cancel)?;
        encoder.write_all(chunk)?;
    }
    check(cancel)?;
    Ok(encoder.finish()?)
}

fn line(out: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let length = bytes.len();
    let prefix = if length <= 26 {
        b'A' + length as u8 - 1
    } else {
        b'a' + length as u8 - 27
    };
    out.write_all(&[prefix])?;
    for chunk in bytes.chunks(4) {
        let mut padded = [0; 4];
        padded[..chunk.len()].copy_from_slice(chunk);
        let mut word = u32::from_be_bytes(padded);
        let mut encoded = [0; 5];
        for digit in encoded.iter_mut().rev() {
            *digit = ALPHABET[(word % 85) as usize];
            word /= 85;
        }
        out.write_all(&encoded)?;
    }
    out.write_all(b"\n")
}

fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::zero_word(&[0, 0, 0, 0], b"D00000\n")]
    #[case::max_word(&[255, 255, 255, 255], b"D|NsC0\n")]
    #[case::ordered_word(&[1, 2, 3, 4], b"D0RjUA\n")]
    #[case::partial_word(&[0], b"A00000\n")]
    fn base85_vectors(#[case] bytes: &[u8], #[case] expected: &[u8]) {
        let mut encoded = Vec::new();
        line(&mut encoded, bytes).unwrap();
        assert_eq!(encoded, expected);
    }

    #[test]
    fn cancellation_precedes_binary_header() {
        let mut output = Vec::new();
        let result = write(&mut output, b"old", b"new", 10, &AtomicBool::new(true));
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(output.is_empty());
    }

    struct CancellingWriter<'a> {
        bytes: Vec<u8>,
        cancel: &'a AtomicBool,
    }

    impl Write for CancellingWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            self.cancel.store(true, Ordering::Relaxed);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn cancellation_during_output_retains_only_partial_artifact() {
        let cancel = AtomicBool::new(false);
        let mut out = CancellingWriter {
            bytes: Vec::new(),
            cancel: &cancel,
        };
        let result = write(&mut out, b"old", b"new", 10, &cancel);
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(out.bytes, b"GIT binary patch\n");
    }
}
