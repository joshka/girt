use std::sync::atomic::AtomicBool;

use super::graph::Graph;
use super::reachable::Closures;
use super::{WriteError, WriteLimits, WriteOptions, Written, check};
use crate::{ObjectId, ObjectKind};

pub(super) fn artifacts(
    anchor: ObjectId,
    order: &[usize],
    graph: &Graph<'_>,
    options: WriteOptions,
    limits: WriteLimits,
    midx: bool,
    cancel: &AtomicBool,
) -> Result<Written, WriteError> {
    let ids = graph.ids;
    let selected = graph.select(&options, limits, cancel)?;
    let mut inverse = vec![0; ids.len()];
    for (bit, &position) in order.iter().enumerate() {
        check(cancel)?;
        inverse[position] = bit;
    }
    let mut output = Output::new(limits.max_bytes);
    output.bytes(b"BITM")?;
    output.bytes(&1_u16.to_be_bytes())?;
    output.bytes(&(if midx { 0x15_u16 } else { 0x11 }).to_be_bytes())?;
    output.word(selected.len() as u32)?;
    output.bytes(anchor.as_bytes())?;
    for kind in [
        ObjectKind::Commit,
        ObjectKind::Tree,
        ObjectKind::Blob,
        ObjectKind::Tag,
    ] {
        let mut words = vec![0_u64; ids.len().div_ceil(64)];
        for (index, &actual) in graph.kinds.iter().enumerate() {
            check(cancel)?;
            if actual == kind {
                let bit = inverse[index];
                words[bit / 64] |= 1 << (bit % 64);
            }
        }
        ewah(&mut output, &words, cancel)?;
    }
    let mut offsets = Vec::with_capacity(selected.len());
    let mut closures = Closures::new(graph, &inverse, limits);
    let ordered = closures.ancestors_first(&selected, cancel)?;
    for position in ordered {
        check(cancel)?;
        offsets.push((position, output.data.len()));
        output.word(position as u32)?;
        output.bytes(&[0, 0])?; // Independent record, no reuse flag or XOR dependency.
        let words = closures.reachable(position, cancel)?;
        ewah(&mut output, &words, cancel)?;
    }
    offsets.sort_unstable_by_key(|&(position, _)| position);
    for (position, offset) in offsets {
        check(cancel)?;
        output.word(position as u32)?;
        output.long(offset as u64)?;
        output.word(u32::MAX)?;
    }
    if midx {
        // MIDX consumers expect a name cache. Unknown pathname hints use zero; they affect delta
        // heuristics only, never object identity or reachability.
        for _ in ids {
            check(cancel)?;
            output.word(0)?;
        }
    }
    output.checksum(anchor.format())?;
    let mut reverse = Output::new(limits.max_bytes - output.data.len());
    reverse.bytes(b"RIDX")?;
    reverse.word(1)?;
    reverse.word(if anchor.format() == crate::ObjectFormat::Sha1 {
        1
    } else {
        2
    })?;
    for &position in order {
        check(cancel)?;
        reverse.word(position as u32)?;
    }
    reverse.bytes(anchor.as_bytes())?;
    reverse.checksum(anchor.format())?;
    // Parse the returned formats with caller-derived limits as an independent structural check.
    let bounds = super::super::Limits {
        max_bytes: limits.max_bytes,
        max_objects: limits.max_objects,
        max_entries: options.max_commits,
        max_decoded_words: ids.len().div_ceil(64).saturating_mul(8),
    };
    super::super::BitmapIndex::parse_with(&output.data, anchor, ids.len(), bounds)?;
    super::super::ReverseIndex::parse_with(&reverse.data, anchor, ids.len(), bounds)?;
    check(cancel)?;
    Ok(Written {
        anchor,
        objects: ids.len(),
        commits: selected.len(),
        bitmap: output.data,
        reverse_index: reverse.data,
    })
}

/// Emits EWAH RLWs and literal runs, trimming zero tail words while preserving exact set bits.
fn ewah(output: &mut Output, input: &[u64], cancel: &AtomicBool) -> Result<(), WriteError> {
    let length = input
        .iter()
        .rposition(|&word| word != 0)
        .map_or(0, |position| position + 1);
    let words = &input[..length];
    let bits = words.last().map_or(0, |word| {
        (length - 1) * 64 + (64 - word.leading_zeros() as usize)
    });
    output.word(u32::try_from(bits).map_err(|_| WriteError::Limit("EWAH bits"))?)?;
    let count_offset = output.data.len();
    output.word(0)?;
    let mut compressed = 0_u32;
    let mut last = 0_u32;
    let mut position = 0;
    if words.is_empty() {
        output.long(0)?;
        compressed = 1;
    }
    while position < words.len() {
        check(cancel)?;
        let bit = words[position] == u64::MAX;
        let fill = if bit { u64::MAX } else { 0 };
        let mut run = 0_u32;
        while position < words.len() && words[position] == fill {
            check(cancel)?;
            run += 1;
            position += 1;
        }
        let first = position;
        while position < words.len() && words[position] != 0 && words[position] != u64::MAX {
            check(cancel)?;
            position += 1;
        }
        let literals = u32::try_from(position - first)
            .ok()
            .filter(|&count| count <= 0x7fff_ffff)
            .ok_or(WriteError::Limit("EWAH literals"))?;
        last = compressed;
        output.long(u64::from(bit) | (u64::from(run) << 1) | (u64::from(literals) << 33))?;
        compressed = compressed
            .checked_add(1 + literals)
            .ok_or(WriteError::Limit("EWAH words"))?;
        for &word in &words[first..position] {
            check(cancel)?;
            output.long(word)?;
        }
    }
    output
        .data
        .get_mut(count_offset..count_offset + 4)
        .ok_or(WriteError::Limit("EWAH count"))?
        .copy_from_slice(&compressed.to_be_bytes());
    output.word(last)
}

pub(super) struct Output {
    pub data: Vec<u8>,
    limit: usize,
}
impl Output {
    pub fn new(limit: usize) -> Self {
        Self {
            data: Vec::new(),
            limit,
        }
    }
    pub fn bytes(&mut self, bytes: &[u8]) -> Result<(), WriteError> {
        if self
            .data
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > self.limit)
        {
            return Err(WriteError::Limit("artifact bytes"));
        }
        self.data
            .try_reserve(bytes.len())
            .map_err(|_| WriteError::Limit("output storage"))?;
        self.data.extend_from_slice(bytes);
        Ok(())
    }
    pub fn word(&mut self, value: u32) -> Result<(), WriteError> {
        self.bytes(&value.to_be_bytes())
    }
    pub fn long(&mut self, value: u64) -> Result<(), WriteError> {
        self.bytes(&value.to_be_bytes())
    }
    pub fn checksum(&mut self, format: crate::ObjectFormat) -> Result<(), WriteError> {
        let checksum = format.checksum(&self.data);
        self.bytes(checksum.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewah_round_trips_mixed_runs_literals_and_empty_tail() {
        for words in [
            vec![],
            vec![0; 12],
            vec![u64::MAX; 12],
            vec![0, 0, 5, u64::MAX, u64::MAX, 9, 0, 0],
            vec![3, 9, 0, u64::MAX, 1],
        ] {
            let mut output = Output::new(1024);
            ewah(&mut output, &words, &AtomicBool::new(false)).unwrap();
            let parsed = super::super::super::ewah::Ewah::parse(
                &output.data,
                &mut 0,
                output.data.len(),
                words.len() * 64,
            )
            .unwrap();
            let mut decoded = vec![0; words.len()];
            parsed
                .xor_into(
                    &output.data,
                    &mut decoded,
                    &mut 1024,
                    &AtomicBool::new(false),
                )
                .unwrap();
            assert_eq!(decoded, words);
        }
    }
}
