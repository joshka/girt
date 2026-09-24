use std::collections::HashSet;

use flate2::{Decompress, FlushDecompress, Status};

use super::delta::{apply, byte, charge, size};
use super::index::{Index, verify_hash, word};
use crate::{Object, ObjectId, ObjectKind, ObjectReadError as Error, ReadLimits};

#[derive(Debug)]
pub(crate) struct Pack {
    index: Index,
    data: Vec<u8>,
}

impl Pack {
    pub fn open(index_bytes: &[u8], data: Vec<u8>) -> Result<Self, Error> {
        if data.len() < 32 || &data[..4] != b"PACK" {
            return Err(Error::Corrupt("pack header"));
        }
        let version = word(&data, 4)?;
        if version != 2 {
            return Err(Error::PackVersion(version));
        }
        let end = data.len() - 20;
        let index = Index::parse(index_bytes, end)?;
        if word(&data, 8)? as usize != index.entries.len() {
            return Err(Error::Corrupt("pack object count"));
        }
        verify_hash(&data, "pack checksum")?;
        if index.pack_hash != data[end..] {
            return Err(Error::Corrupt("index/pack checksum disagreement"));
        }
        for entry in &index.entries {
            if crc32fast::hash(&data[entry.offset..entry.end]) != entry.crc {
                return Err(Error::Corrupt("entry CRC"));
            }
        }
        Ok(Self { index, data })
    }

    pub fn find(&self, id: ObjectId) -> Option<usize> {
        self.index.find(id)
    }

    pub fn read(&self, mut position: usize, limits: ReadLimits) -> Result<Object, Error> {
        let mut remaining = limits.max_decode_bytes;
        let mut pending = Vec::new();
        let mut seen = HashSet::new();
        let mut object = loop {
            if !seen.insert(position) {
                return Err(Error::DeltaCycle);
            }
            let entry = &self.index.entries[position];
            let mut input = &self.data[entry.offset..entry.end];
            let header = byte(&mut input)?;
            let kind = (header >> 4) & 7;
            let length = size(&mut input, (header & 15) as usize, 4, header & 128 != 0)?;
            let base = match kind {
                1..=4 => None,
                6 => {
                    let mut next = byte(&mut input)?;
                    let mut distance = (next & 127) as usize;
                    while next & 128 != 0 {
                        next = byte(&mut input)?;
                        distance = distance
                            .checked_add(1)
                            .and_then(|n| n.checked_mul(128))
                            .and_then(|n| n.checked_add((next & 127) as usize))
                            .ok_or(Error::Corrupt("delta offset overflow"))?;
                    }
                    if distance == 0 {
                        return Err(Error::Corrupt("zero delta distance"));
                    }
                    let offset = entry
                        .offset
                        .checked_sub(distance)
                        .ok_or(Error::Corrupt("delta offset before pack"))?;
                    Some(self.index.at_offset(offset)?)
                }
                7 => {
                    let raw = input
                        .get(..20)
                        .ok_or(Error::Corrupt("truncated base identity"))?;
                    let id = ObjectId::from_bytes(raw.try_into().unwrap());
                    input = &input[20..];
                    Some(self.index.find(id).ok_or(Error::MissingBase(id))?)
                }
                other => return Err(Error::ObjectType(other)),
            };
            let limit = if base.is_some() {
                limits.max_delta_bytes
            } else {
                limits.max_object_bytes
            };
            if length > limit {
                return Err(Error::Limit(if base.is_some() {
                    "delta program bytes"
                } else {
                    "object bytes"
                }));
            }
            if base.is_some() && pending.len() >= limits.max_delta_depth {
                return Err(Error::Limit("delta depth"));
            }
            charge(&mut remaining, length)?;
            let data = inflate(input, length)?;
            if let Some(base) = base {
                pending.push((position, data));
                position = base;
            } else {
                let kind = match kind {
                    1 => ObjectKind::Commit,
                    2 => ObjectKind::Tree,
                    3 => ObjectKind::Blob,
                    4 => ObjectKind::Tag,
                    _ => unreachable!("non-delta kinds validated above"),
                };
                let object = Object { kind, data };
                verify_identity(&object, entry.id)?;
                break object;
            }
        };
        while let Some((position, program)) = pending.pop() {
            object.data = apply(
                &object.data,
                &program,
                limits.max_object_bytes,
                &mut remaining,
            )?;
            verify_identity(&object, self.index.entries[position].id)?;
        }
        Ok(object)
    }
}

fn verify_identity(object: &Object, expected: ObjectId) -> Result<(), Error> {
    if object.id() != expected {
        return Err(Error::Corrupt("object identity"));
    }
    Ok(())
}

fn inflate(input: &[u8], expected: usize) -> Result<Vec<u8>, Error> {
    let (data, consumed) = inflate_prefix(input, expected)?;
    if consumed != input.len() {
        return Err(Error::Corrupt("packed entry length or trailing data"));
    }
    Ok(data)
}

pub(super) fn inflate_prefix(mut input: &[u8], expected: usize) -> Result<(Vec<u8>, usize), Error> {
    let mut inflater = Decompress::new(true);
    let mut output = [0; 8192];
    let mut data = Vec::new();
    loop {
        let before_in = inflater.total_in();
        let before_out = inflater.total_out();
        let status = inflater
            .decompress(input, &mut output, FlushDecompress::None)
            .map_err(|_| Error::Corrupt("invalid packed zlib stream"))?;
        let consumed = (inflater.total_in() - before_in) as usize;
        let produced = (inflater.total_out() - before_out) as usize;
        input = &input[consumed..];
        if produced > expected.saturating_sub(data.len()) {
            return Err(Error::Corrupt("inflated entry exceeds declared size"));
        }
        data.extend_from_slice(&output[..produced]);
        if status == Status::StreamEnd {
            if data.len() != expected {
                return Err(Error::Corrupt("packed entry length or trailing data"));
            }
            return Ok((data, inflater.total_in() as usize));
        }
        if produced == 0 && consumed == 0 {
            return Err(Error::Corrupt("truncated packed zlib stream"));
        }
    }
}
