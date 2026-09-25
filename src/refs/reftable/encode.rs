use std::io::Write;

use super::{Error, Limits, Table};
use crate::ObjectFormat;
use crate::refs::Target;

impl Table {
    /// Encodes this table in canonical key order without changing its records.
    ///
    /// Uses version 1 for SHA-1 and version 2 for SHA-256. Object lookup accelerators are omitted;
    /// reference and log indexes locate independently compressed blocks. Inputs must already be
    /// sorted and unique. The decoder validates the complete result before it is returned.
    ///
    /// # Errors
    ///
    /// Reports invalid ordering, update ranges, mixed identities, invalid peeled values, and
    /// exhausted limits. No filesystem mutation is performed.
    pub fn encode(&self, limits: Limits) -> Result<Vec<u8>, Error> {
        self.check_limits(limits)?;
        let header = self.header();
        let mut writer = Writer {
            bytes: header.clone(),
            limits,
        };
        let mut records = Vec::new();
        for record in &self.references {
            let mut bytes = Vec::new();
            let kind = match (&record.target, record.peeled) {
                (None, None) => 0,
                (Some(Target::Direct(_)), None) => 1,
                (Some(Target::Direct(_)), Some(_)) => 2,
                (Some(Target::Symbolic(_)), None) => 3,
                _ => return Err(Error::Malformed("peeled value without direct target")),
            };
            key(&mut bytes, record.name.as_bytes(), kind);
            let delta = record
                .update_index
                .checked_sub(self.min_update_index)
                .ok_or(Error::Malformed("reference update range"))?;
            varint(&mut bytes, delta);
            match &record.target {
                Some(Target::Direct(id)) => {
                    require_format(self.format, *id)?;
                    bytes.extend_from_slice(id.as_bytes());
                }
                Some(Target::Symbolic(name)) => string(&mut bytes, name.as_bytes()),
                None => {}
            }
            if let Some(id) = record.peeled {
                require_format(self.format, id)?;
                bytes.extend_from_slice(id.as_bytes());
            }
            records.push((record.name.as_bytes().to_vec(), bytes));
        }
        let ref_index = writer.section(b'r', records, header.len())?;
        let log_position = if self.logs.is_empty() {
            0
        } else {
            writer.bytes.len() as u64
        };
        let mut records = Vec::new();
        for record in &self.logs {
            let mut name = record.name.as_bytes().to_vec();
            name.push(0);
            name.extend_from_slice(&(!record.update_index).to_be_bytes());
            let mut bytes = Vec::new();
            key(&mut bytes, &name, u8::from(record.value.is_some()));
            if let Some(value) = &record.value {
                require_format(self.format, value.old)?;
                require_format(self.format, value.new)?;
                bytes.extend_from_slice(value.old.as_bytes());
                bytes.extend_from_slice(value.new.as_bytes());
                string(&mut bytes, &value.name);
                string(&mut bytes, &value.email);
                varint(&mut bytes, value.seconds);
                bytes.extend_from_slice(&value.offset_minutes.to_be_bytes());
                string(&mut bytes, &value.message);
            }
            records.push((name, bytes));
        }
        let log_index = writer.section(b'g', records, 0)?;
        let mut footer = header;
        for position in [ref_index, 0, 0, log_position, log_index] {
            footer.extend_from_slice(&position.to_be_bytes());
        }
        footer.extend_from_slice(&crc32fast::hash(&footer).to_be_bytes());
        writer.append(&footer)?;
        Self::decode(&writer.bytes, limits)?;
        Ok(writer.bytes)
    }

    fn check_limits(&self, limits: Limits) -> Result<(), Error> {
        if self.references.len().saturating_add(self.logs.len()) > limits.records {
            return Err(Error::Limit("record count"));
        }
        let mut total = 0_usize;
        let mut account = |length: usize| {
            if length > limits.string_bytes {
                return Err(Error::Limit("string bytes"));
            }
            total = total
                .checked_add(length)
                .ok_or(Error::Limit("decoded bytes"))?;
            if total > limits.decoded_bytes {
                return Err(Error::Limit("decoded bytes"));
            }
            Ok(())
        };
        for record in &self.references {
            account(record.name.as_bytes().len())?;
            if let Some(Target::Symbolic(name)) = &record.target {
                account(name.as_bytes().len())?;
            }
        }
        for record in &self.logs {
            account(record.name.as_bytes().len().saturating_add(9))?;
            if let Some(value) = &record.value {
                account(value.name.len())?;
                account(value.email.len())?;
                account(value.message.len())?;
            }
        }
        Ok(())
    }

    fn header(&self) -> Vec<u8> {
        let mut bytes = b"REFT".to_vec();
        bytes.push(if self.format == ObjectFormat::Sha1 {
            1
        } else {
            2
        });
        bytes.extend_from_slice(&[0; 3]);
        bytes.extend_from_slice(&self.min_update_index.to_be_bytes());
        bytes.extend_from_slice(&self.max_update_index.to_be_bytes());
        if self.format == ObjectFormat::Sha256 {
            bytes.extend_from_slice(b"s256");
        }
        bytes
    }
}

struct Writer {
    bytes: Vec<u8>,
    limits: Limits,
}

impl Writer {
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() > self.limits.bytes.saturating_sub(self.bytes.len()) {
            return Err(Error::Limit("table bytes"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn section(
        &mut self,
        kind: u8,
        records: Vec<(Vec<u8>, Vec<u8>)>,
        first_prefix: usize,
    ) -> Result<u64, Error> {
        let mut index = Vec::new();
        let mut block = Vec::new();
        let mut restarts = Vec::new();
        let mut last = Vec::new();
        let mut prefix = first_prefix;
        for (key, record) in records {
            if !block.is_empty()
                && block.len() + record.len() + restarts.len() * 3 + 9 + prefix > 4096
            {
                let position = self.block(kind, &block, &restarts, prefix)?;
                index.push(index_record(&last, position));
                block.clear();
                restarts.clear();
                prefix = 0;
            }
            if record.len() + block.len() + restarts.len() * 3 + 9 + prefix
                > self.limits.block_bytes
            {
                return Err(Error::Limit("block bytes"));
            }
            restarts.push(block.len() + prefix + 4);
            block.extend_from_slice(&record);
            last = key;
        }
        if !block.is_empty() {
            let position = self.block(kind, &block, &restarts, prefix)?;
            index.push(index_record(&last, position));
        }
        if index.len() < 2 {
            return Ok(0);
        }
        // A single-level index is allowed to exceed the nominal data block size.
        let position = self.bytes.len() as u64;
        let mut block = Vec::new();
        let mut restarts = Vec::new();
        for record in index {
            restarts.push(block.len() + 4);
            block.extend_from_slice(&record);
        }
        self.block(b'i', &block, &restarts, 0)?;
        Ok(position)
    }

    fn block(
        &mut self,
        kind: u8,
        records: &[u8],
        restarts: &[usize],
        prefix: usize,
    ) -> Result<u64, Error> {
        let length = prefix + 4 + records.len() + restarts.len() * 3 + 2;
        if length > self.limits.block_bytes || length > 0xff_ffff || restarts.len() > 65535 {
            return Err(Error::Limit("block bytes or restarts"));
        }
        let position = if prefix != 0 {
            0
        } else {
            self.bytes.len() as u64
        };
        let mut body = records.to_vec();
        for restart in restarts {
            uint24(&mut body, *restart);
        }
        body.extend_from_slice(&(restarts.len() as u16).to_be_bytes());
        let mut head = vec![kind];
        uint24(&mut head, length);
        self.append(&head)?;
        if kind == b'g' {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(&body)
                .map_err(|_| Error::Malformed("compression failed"))?;
            let compressed = encoder
                .finish()
                .map_err(|_| Error::Malformed("compression failed"))?;
            self.append(&compressed)?;
        } else {
            self.append(&body)?;
        }
        Ok(position)
    }
}

fn require_format(format: ObjectFormat, id: crate::ObjectId) -> Result<(), Error> {
    id.require_format(format)
        .map_err(|_| Error::Malformed("mixed object formats"))
}

fn index_record(name: &[u8], position: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    key(&mut bytes, name, 0);
    varint(&mut bytes, position);
    bytes
}

fn key(bytes: &mut Vec<u8>, name: &[u8], kind: u8) {
    bytes.push(0);
    varint(bytes, ((name.len() as u64) << 3) | u64::from(kind));
    bytes.extend_from_slice(name);
}

fn string(bytes: &mut Vec<u8>, value: &[u8]) {
    varint(bytes, value.len() as u64);
    bytes.extend_from_slice(value);
}

fn varint(bytes: &mut Vec<u8>, mut value: u64) {
    let mut encoded = [0; 10];
    let mut position = 9;
    encoded[position] = (value & 127) as u8;
    while value > 127 {
        value = (value >> 7) - 1;
        position -= 1;
        encoded[position] = (value as u8 & 127) | 128;
    }
    bytes.extend_from_slice(&encoded[position..]);
}

fn uint24(bytes: &mut Vec<u8>, value: usize) {
    bytes.extend_from_slice(&(value as u32).to_be_bytes()[1..]);
}
