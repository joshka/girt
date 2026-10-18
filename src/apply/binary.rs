//! Git binary payload decoding, checked against original Git CLI fixtures.
use flate2::{Decompress, FlushDecompress, Status};

use super::{Applied, Error, FilePatch, Limits};
use crate::ObjectId;

/// Encoding of one Git binary-patch block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BinaryKind {
    /// Compressed complete blob bytes.
    Literal,
    /// Compressed Git pack-delta instructions against the preimage.
    Delta,
}

#[derive(Clone, Debug)]
pub(super) struct Block {
    pub kind: BinaryKind,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(super) struct BinaryPatch {
    pub old: ObjectId,
    pub new: ObjectId,
    pub forward: Block,
    pub reverse: Option<Block>,
}

pub(super) fn decode_line(line: &[u8], at: usize, output: &mut Vec<u8>) -> Result<(), Error> {
    let length = match line.first() {
        Some(b'A'..=b'Z') => (line[0] - b'A' + 1) as usize,
        Some(b'a'..=b'z') => (line[0] - b'a' + 27) as usize,
        _ => return Err(bad(at, "invalid binary line length")),
    };
    let encoded = line.get(1..).unwrap_or_default();
    if encoded.len() != length.div_ceil(4) * 5 {
        return Err(bad(at, "binary line length mismatch"));
    }
    let mut remaining = length;
    for group in encoded.as_chunks::<5>().0 {
        let mut word = 0u32;
        for byte in group {
            let digit = ALPHABET
                .iter()
                .position(|candidate| candidate == byte)
                .ok_or_else(|| bad(at, "invalid binary base85 digit"))?
                as u32;
            word = word
                .checked_mul(85)
                .and_then(|value| value.checked_add(digit))
                .ok_or_else(|| bad(at, "binary base85 overflow"))?;
        }
        let take = remaining.min(4);
        output.extend_from_slice(&word.to_be_bytes()[..take]);
        remaining -= take;
    }
    Ok(())
}

const ALPHABET: &[u8; 85] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";

pub(super) fn inflate(input: &[u8], expected: usize, at: usize) -> Result<Vec<u8>, Error> {
    let mut decoder = Decompress::new(true);
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let before_in = decoder.total_in() as usize;
        let before_out = decoder.total_out() as usize;
        let status = decoder
            .decompress(&input[before_in..], &mut buffer, FlushDecompress::None)
            .map_err(|_| bad(at, "invalid binary zlib stream"))?;
        let produced = decoder.total_out() as usize - before_out;
        if produced > expected.saturating_sub(output.len()) {
            return Err(bad(at, "binary inflated size mismatch"));
        }
        output.extend_from_slice(&buffer[..produced]);
        if status == Status::StreamEnd {
            if output.len() != expected || decoder.total_in() as usize != input.len() {
                return Err(bad(at, "binary stream length mismatch"));
            }
            return Ok(output);
        }
        if decoder.total_in() as usize == before_in && produced == 0 {
            return Err(bad(at, "truncated binary zlib stream"));
        }
    }
}

pub(super) fn apply(
    file: &FilePatch,
    binary: &BinaryPatch,
    preimage: &[u8],
    limits: Limits,
) -> Result<Applied, Error> {
    if preimage.len() > limits.max_input_bytes {
        return Err(Error::Limit("input bytes"));
    }
    let mismatch = || Error::Mismatch { line: file.line };
    if binary.old.is_null() {
        if !preimage.is_empty() {
            return Err(mismatch());
        }
    } else if ObjectId::for_blob(binary.old.format(), preimage) != binary.old {
        return Err(mismatch());
    }
    let mut remaining = limits.max_output_bytes;
    let content = match binary.forward.kind {
        BinaryKind::Literal => {
            if binary.forward.bytes.len() > limits.max_output_bytes {
                return Err(Error::Limit(super::text::OUTPUT_BYTES));
            }
            binary.forward.bytes.clone()
        }
        BinaryKind::Delta => crate::pack::delta::apply(
            preimage,
            &binary.forward.bytes,
            limits.max_output_bytes,
            &mut remaining,
        )
        .map_err(|error| match error {
            crate::ObjectReadError::Limit(_) => Error::Limit(super::text::OUTPUT_BYTES),
            crate::ObjectReadError::Corrupt(reason) => bad(file.line, reason),
            _ => bad(file.line, "invalid binary delta"),
        })?,
    };
    if binary.new.is_null() {
        if !content.is_empty() {
            return Err(mismatch());
        }
    } else if ObjectId::for_blob(binary.new.format(), &content) != binary.new {
        return Err(mismatch());
    }
    Ok(Applied {
        content,
        hunks: Vec::new(),
    })
}

fn bad(line: usize, reason: &'static str) -> Error {
    Error::Malformed { line, reason }
}
