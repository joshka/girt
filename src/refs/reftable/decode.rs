use super::{Error, Limits, LogRecord, LogValue, RecordName, RefRecord, Table};
use crate::refs::{RefName, Target};
use crate::{ObjectFormat, ObjectId};

impl Table {
    /// Decodes and validates a complete immutable table within caller-selected limits.
    ///
    /// No object lookup, stack merging or files-reflog construction validation is performed.
    /// The decoder retains binary log fields exactly, including unsigned timestamps.
    ///
    /// # Errors
    ///
    /// Reports unsupported versions/hash IDs, malformed framing/checksums/records and exhausted
    /// limits. A failure returns no partial table. Work is synchronous and bounded by the input,
    /// inflated block and record limits.
    pub fn decode(bytes: &[u8], limits: Limits) -> Result<Self, Error> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(target: "girt", "reftable.decode", outcome = "incomplete", failure_class = tracing::field::Empty, bytes = bytes.len());
        let operation = || Self::decode_usage(bytes, limits).map(|(table, _, _)| table);
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = operation();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| match error {
            Error::Malformed(_) => "corrupt",
            Error::Unsupported(_) => "unsupported",
            Error::Limit(_) => "limit",
        });
        result
    }

    pub(super) fn decode_usage(
        bytes: &[u8],
        limits: Limits,
    ) -> Result<(Self, usize, usize), Error> {
        if bytes.len() > limits.bytes {
            return Err(Error::Limit("table bytes"));
        }
        let header = Header::parse(bytes)?;
        let footer_start = bytes
            .len()
            .checked_sub(header.size + 44)
            .ok_or(bad("footer"))?;
        if footer_start < header.size {
            return Err(bad("overlapping header and footer"));
        }
        let footer = &bytes[footer_start..];
        if footer[..header.size] != bytes[..header.size] {
            return Err(bad("header/footer mismatch"));
        }
        let crc_start = footer.len() - 4;
        let crc = u32::from_be_bytes(footer[crc_start..].try_into().unwrap());
        if crc32fast::hash(&footer[..crc_start]) != crc {
            return Err(bad("footer checksum"));
        }
        let mut fields = Cursor::new(&footer[header.size..crc_start], limits);
        let ref_index = fields.u64()?;
        let object = fields.u64()?;
        let object_index = fields.u64()?;
        let log = fields.u64()?;
        let log_index = fields.u64()?;
        let positions = [ref_index, object >> 5, object_index, log, log_index];
        let mut previous = 0;
        for position in positions.into_iter().filter(|p| *p != 0) {
            if position < header.size as u64
                || position >= footer_start as u64
                || position < previous
            {
                return Err(bad("section positions"));
            }
            previous = position;
        }
        let mut table = Self {
            format: header.format,
            min_update_index: header.min,
            max_update_index: header.max,
            references: Vec::new(),
            logs: Vec::new(),
        };
        // Git 2.55 log-only tables may use offset zero and include the file header in
        // the first inflated block's offsets, just as reference blocks do.
        let first_log = log == 0 && bytes.get(header.size) == Some(&b'g');
        let mut position = header.size;
        let mut records = 0;
        let mut decoded_bytes = 0_usize;
        let mut blocks = std::collections::BTreeMap::<u64, (u8, Vec<u8>)>::new();
        let mut index_sections = std::collections::BTreeMap::new();
        let mut index_links = std::collections::BTreeMap::new();
        let mut last_ref = Vec::new();
        let mut last_log = Vec::new();
        while position < footer_start {
            if bytes[position] == 0 {
                let next = position
                    .checked_add(header.alignment - position % header.alignment.max(1))
                    .ok_or(bad("padding overflow"))?;
                if header.alignment == 0
                    || next > footer_start
                    || bytes[position..next].iter().any(|b| *b != 0)
                {
                    return Err(bad("block padding"));
                }
                position = next;
                continue;
            }
            let kind = bytes[position];
            let expected = if log_index != 0 && position as u64 >= log_index {
                b'i'
            } else if (log != 0 && position as u64 >= log) || first_log {
                b'g'
            } else if object_index != 0 && position as u64 >= object_index {
                b'i'
            } else if object >> 5 != 0 && position as u64 >= object >> 5 {
                b'o'
            } else if ref_index != 0 && position as u64 >= ref_index {
                b'i'
            } else {
                b'r'
            };
            let leaf_kind = if (log != 0 && position as u64 >= log) || first_log {
                b'g'
            } else if object >> 5 != 0 && position as u64 >= object >> 5 {
                b'o'
            } else {
                b'r'
            };
            let root_index = match leaf_kind {
                b'g' => log_index,
                b'o' => object_index,
                _ => ref_index,
            };
            let child_index = kind == b'i' && root_index != 0 && position as u64 <= root_index;
            if kind != expected && !child_index {
                return Err(bad("block section type"));
            }
            let prefix = if position == header.size && (kind != b'g' || first_log) {
                header.size
            } else {
                0
            };
            if matches!(kind, b'r' | b'o') && header.alignment != 0 {
                let length = bytes
                    .get(position + 1..position + 4)
                    .ok_or(bad("block header"))?;
                if uint24(length) > header.alignment {
                    return Err(bad("block exceeds alignment"));
                }
            }
            let block = Block::read(&bytes[position..footer_start], prefix, limits)?;
            let mut cursor = Cursor::new(&block.data, limits);
            let mut key = Vec::new();
            let mut restart = 0;
            let mut links = Vec::new();
            while cursor.position < block.records_end {
                records += 1;
                if records > limits.records {
                    return Err(Error::Limit("record count"));
                }
                let offset = cursor.position + prefix + 4;
                let prefix_len = cursor.length()?;
                if block.restarts.get(restart).copied() == Some(offset) {
                    if prefix_len != 0 {
                        return Err(bad("compressed restart"));
                    }
                    restart += 1;
                }
                let tagged = cursor.varint()?;
                let suffix_len = usize::try_from(tagged >> 3).map_err(|_| bad("suffix length"))?;
                if prefix_len > key.len()
                    || prefix_len
                        .checked_add(suffix_len)
                        .is_none_or(|n| n > limits.string_bytes)
                {
                    return Err(bad("key prefix or length"));
                }
                decoded_bytes = decoded_bytes
                    .checked_add(prefix_len + suffix_len)
                    .ok_or(Error::Limit("decoded bytes"))?;
                if decoded_bytes > limits.decoded_bytes {
                    return Err(Error::Limit("decoded bytes"));
                }
                let record_start = cursor.position;
                cursor.end_limit = cursor
                    .position
                    .saturating_add(limits.decoded_bytes - decoded_bytes);
                let mut next = key[..prefix_len].to_vec();
                next.extend_from_slice(cursor.take(suffix_len)?);
                if !key.is_empty() && next <= key {
                    return Err(bad("record key order"));
                }
                key = next;
                let value_type = (tagged & 7) as u8;
                match kind {
                    b'r' => {
                        if !last_ref.is_empty() && key <= last_ref {
                            return Err(bad("reference order"));
                        }
                        let record = decode_ref(&mut cursor, &key, value_type, &header)?;
                        table.references.push(record);
                        last_ref.clone_from(&key);
                    }
                    b'g' => {
                        if !last_log.is_empty() && key <= last_log {
                            return Err(bad("log order"));
                        }
                        table
                            .logs
                            .push(decode_log(&mut cursor, &key, value_type, &header)?);
                        last_log.clone_from(&key);
                    }
                    b'i' => {
                        let target = cursor.varint()?;
                        links.push(target);
                        let Some((target_kind, target_key)) = blocks.get(&target) else {
                            return Err(bad("index pointer"));
                        };
                        if value_type != 0
                            || (*target_kind != leaf_kind
                                && (*target_kind != b'i'
                                    || index_sections.get(&target) != Some(&leaf_kind)))
                            || *target_key != key
                        {
                            return Err(bad("index key or block type"));
                        }
                    }
                    b'o' => {
                        if key.len() != (object & 31) as usize || key.len() < 2 {
                            return Err(bad("object abbreviation"));
                        }
                        let count = if value_type == 0 {
                            cursor.varint()?
                        } else {
                            u64::from(value_type)
                        };
                        if count > block.data.len() as u64 {
                            return Err(bad("object position count"));
                        }
                        let mut offset = 0_u64;
                        for index in 0..count {
                            let delta = cursor.varint()?;
                            if index != 0 && delta == 0 {
                                return Err(bad("object position order"));
                            }
                            offset = offset
                                .checked_add(delta)
                                .ok_or(bad("object offset overflow"))?;
                            if !blocks.get(&offset).is_some_and(|(kind, _)| *kind == b'r') {
                                return Err(bad("object position"));
                            }
                        }
                    }
                    _ => return Err(bad("block type")),
                }
                decoded_bytes = decoded_bytes
                    .checked_add(cursor.position - record_start)
                    .ok_or(Error::Limit("decoded bytes"))?;
                if decoded_bytes > limits.decoded_bytes {
                    return Err(Error::Limit("decoded bytes"));
                }
            }
            if cursor.position != block.records_end || restart != block.restarts.len() {
                return Err(bad("record/restart boundary"));
            }
            if kind == b'i' {
                index_sections.insert(position as u64, leaf_kind);
                index_links.insert(position as u64, links);
            }
            blocks.insert(if prefix == 0 { position as u64 } else { 0 }, (kind, key));
            position += block.consumed;
        }
        for section in positions.into_iter().filter(|p| *p != 0) {
            if !blocks.contains_key(&section) {
                return Err(bad("section block boundary"));
            }
        }
        validate_index(
            ref_index,
            b'r',
            header.alignment == 0,
            &blocks,
            &index_sections,
            &index_links,
        )?;
        validate_index(
            log_index,
            b'g',
            false,
            &blocks,
            &index_sections,
            &index_links,
        )?;
        validate_index(
            object_index,
            b'o',
            false,
            &blocks,
            &index_sections,
            &index_links,
        )?;
        Ok((table, records, decoded_bytes))
    }
}

// Every data block must be reachable from its advertised index root. Merely validating individual
// pointers would let a corrupt index hide records from Git while a linear scan still finds them.
fn validate_index(
    root: u64,
    kind: u8,
    required_for_multiple: bool,
    blocks: &std::collections::BTreeMap<u64, (u8, Vec<u8>)>,
    sections: &std::collections::BTreeMap<u64, u8>,
    links: &std::collections::BTreeMap<u64, Vec<u64>>,
) -> Result<(), Error> {
    let expected: std::collections::BTreeSet<_> = blocks
        .iter()
        .filter(|(position, (block_kind, _))| {
            *block_kind == kind || sections.get(position) == Some(&kind)
        })
        .map(|(position, _)| *position)
        .collect();
    if root == 0 {
        if (required_for_multiple && expected.len() > 1)
            || sections.values().any(|section| *section == kind)
        {
            return Err(bad("missing index root"));
        }
        return Ok(());
    }
    if sections.get(&root) != Some(&kind) {
        return Err(bad("index root type"));
    }
    // Git can emit several root-level index blocks beginning at the advertised position.
    let mut pending: Vec<_> = sections
        .range(root..)
        .filter(|(_, section)| **section == kind)
        .map(|(position, _)| *position)
        .collect();
    let mut visited = std::collections::BTreeSet::new();
    while let Some(position) = pending.pop() {
        if !visited.insert(position) {
            return Err(bad("duplicate index child"));
        }
        if let Some(children) = links.get(&position) {
            pending.extend(children);
        }
    }
    if visited != expected {
        return Err(bad("incomplete index coverage"));
    }
    Ok(())
}

struct Header {
    size: usize,
    alignment: usize,
    format: ObjectFormat,
    min: u64,
    max: u64,
}

impl Header {
    fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut cursor = Cursor::new(bytes, Limits::default());
        if cursor.take(4)? != b"REFT" {
            return Err(bad("magic"));
        }
        let version = cursor.take(1)?[0];
        let alignment = uint24(cursor.take(3)?);
        let min = cursor.u64()?;
        let max = cursor.u64()?;
        if min > max {
            return Err(bad("update range"));
        }
        let (size, format) = match version {
            1 => (24, ObjectFormat::Sha1),
            2 => (
                28,
                match cursor.take(4)? {
                    b"sha1" => ObjectFormat::Sha1,
                    b"s256" => ObjectFormat::Sha256,
                    _ => return Err(Error::Unsupported("hash identifier")),
                },
            ),
            _ => return Err(Error::Unsupported("version")),
        };
        Ok(Self {
            size,
            alignment,
            format,
            min,
            max,
        })
    }
}

struct Block {
    data: Vec<u8>,
    records_end: usize,
    restarts: Vec<usize>,
    consumed: usize,
}

impl Block {
    fn read(bytes: &[u8], prefix: usize, limits: Limits) -> Result<Self, Error> {
        let head = bytes.get(..4).ok_or(bad("block header"))?;
        let length = uint24(&head[1..]);
        if length > limits.block_bytes {
            return Err(Error::Limit("block bytes"));
        }
        let data_len = length.checked_sub(prefix + 4).ok_or(bad("block length"))?;
        let (data, consumed) = if head[0] == b'g' {
            let mut inflater = flate2::Decompress::new(true);
            // One spare byte distinguishes an exact-sized stream from an oversized expansion.
            let mut output = vec![0; data_len + 1];
            let status = inflater
                .decompress(&bytes[4..], &mut output, flate2::FlushDecompress::Finish)
                .map_err(|_| bad("log compression"))?;
            if status != flate2::Status::StreamEnd || inflater.total_out() != data_len as u64 {
                return Err(bad("inflated log length"));
            }
            output.truncate(data_len);
            (output, 4 + inflater.total_in() as usize)
        } else {
            (
                bytes
                    .get(4..4 + data_len)
                    .ok_or(bad("truncated block"))?
                    .to_vec(),
                4 + data_len,
            )
        };
        let tail = data.len().checked_sub(2).ok_or(bad("restart count"))?;
        let count = u16::from_be_bytes(data[tail..].try_into().unwrap()) as usize;
        if count == 0 {
            return Err(bad("empty restart table"));
        }
        let records_end = tail.checked_sub(count * 3).ok_or(bad("restart table"))?;
        let restarts: Vec<_> = data[records_end..tail]
            .as_chunks::<3>()
            .0
            .iter()
            .map(|bytes| uint24(bytes))
            .collect();
        if restarts[0] != prefix + 4
            || restarts.windows(2).any(|pair| pair[0] >= pair[1])
            || restarts.last().unwrap() >= &(records_end + prefix + 4)
        {
            return Err(bad("restart offsets"));
        }
        Ok(Self {
            data,
            records_end,
            restarts,
            consumed,
        })
    }
}

fn decode_ref(
    cursor: &mut Cursor<'_>,
    key: &[u8],
    kind: u8,
    header: &Header,
) -> Result<RefRecord, Error> {
    let name = RecordName::new(key)?;
    let update_index = header
        .min
        .checked_add(cursor.varint()?)
        .ok_or(bad("update overflow"))?;
    if update_index > header.max {
        return Err(bad("reference update range"));
    }
    let (target, peeled) = match kind {
        0 => (None, None),
        1 => (Some(Target::Direct(cursor.id(header.format)?)), None),
        2 => (
            Some(Target::Direct(cursor.id(header.format)?)),
            Some(cursor.id(header.format)?),
        ),
        3 => (
            Some(Target::Symbolic(
                RefName::new(cursor.string()?).map_err(|_| bad("symbolic target"))?,
            )),
            None,
        ),
        _ => return Err(Error::Unsupported("reference value type")),
    };
    Ok(RefRecord {
        name,
        update_index,
        target,
        peeled,
    })
}

fn decode_log(
    cursor: &mut Cursor<'_>,
    key: &[u8],
    kind: u8,
    header: &Header,
) -> Result<LogRecord, Error> {
    let split = key.len().checked_sub(9).ok_or(bad("log key"))?;
    if key[split] != 0 {
        return Err(bad("log key separator"));
    }
    let name = RecordName::new(&key[..split])?;
    let update_index = !u64::from_be_bytes(key[split + 1..].try_into().unwrap());
    // Reflog rewrites observed in Git retain older keys in a newer transaction table.
    if update_index > header.max {
        return Err(bad("log update range"));
    }
    let value = match kind {
        0 => None,
        1 => Some(LogValue {
            old: cursor.id(header.format)?,
            new: cursor.id(header.format)?,
            name: cursor.string()?.to_vec(),
            email: cursor.string()?.to_vec(),
            seconds: cursor.varint()?,
            offset_minutes: i16::from_be_bytes(cursor.take(2)?.try_into().unwrap()),
            message: cursor.string()?.to_vec(),
        }),
        _ => return Err(Error::Unsupported("log value type")),
    };
    Ok(LogRecord {
        name,
        update_index,
        value,
    })
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
    end_limit: usize,
    limits: Limits,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8], limits: Limits) -> Self {
        Self {
            bytes,
            position: 0,
            end_limit: bytes.len(),
            limits,
        }
    }
    fn take(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(bad("length overflow"))?;
        if end > self.end_limit {
            return Err(Error::Limit("decoded bytes"));
        }
        let result = self
            .bytes
            .get(self.position..end)
            .ok_or(bad("truncated record"))?;
        self.position = end;
        Ok(result)
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn varint(&mut self) -> Result<u64, Error> {
        let mut byte = self.take(1)?[0];
        let mut value = u64::from(byte & 127);
        while byte & 128 != 0 {
            byte = self.take(1)?[0];
            value = value
                .checked_add(1)
                .and_then(|v| v.checked_mul(128))
                .and_then(|v| v.checked_add(u64::from(byte & 127)))
                .ok_or(bad("varint overflow"))?;
        }
        Ok(value)
    }
    fn length(&mut self) -> Result<usize, Error> {
        usize::try_from(self.varint()?).map_err(|_| bad("length overflow"))
    }
    fn string(&mut self) -> Result<&'a [u8], Error> {
        let length = self.length()?;
        if length > self.limits.string_bytes {
            return Err(Error::Limit("string bytes"));
        }
        self.take(length)
    }
    fn id(&mut self, format: ObjectFormat) -> Result<ObjectId, Error> {
        ObjectId::from_bytes(format, self.take(format.digest_len())?).map_err(|_| bad("object ID"))
    }
}

fn uint24(bytes: &[u8]) -> usize {
    (usize::from(bytes[0]) << 16) | (usize::from(bytes[1]) << 8) | usize::from(bytes[2])
}
fn bad(reason: &'static str) -> Error {
    Error::Malformed(reason)
}
