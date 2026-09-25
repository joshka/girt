use super::write::PackWriteError;

/// Outgoing pack compression policy. All entries use zlib level 6.
#[derive(Debug, Clone, Copy, Default)]
pub enum PackCompression {
    /// Stream ordinary entries with fixed compression scratch space (the default).
    #[default]
    Ordinary,
    /// Try bounded byte-oriented matching against earlier internal bases.
    Delta(DeltaOptions),
}

/// Bounds and selection policy for internal REF_DELTA entries.
///
/// Pack and index order remain ascending object ID. Search visits the preceding `window` entries
/// newest first, trying at most `max_candidates` of the same kind, with lengths within a factor of
/// two and within `max_object_bytes`. Bases always precede dependents; no external bases or
/// OFS_DELTA offsets are emitted. REF_DELTA needs no delta-specific receive-pack capability; the
/// transport must still negotiate the repository object format independently.
///
/// A fixed 4096-slot table indexes 8-byte anchors every 16 base bytes; the earliest anchor wins a
/// hash collision. Target scanning and match extension consume `max_work` units per object across
/// all candidates. Exhaustion discards the unfinished candidate and keeps the best completed entry.
/// One unit means one indexed anchor, scanned target position, or compared byte. Hashing an anchor
/// reads eight bytes. This is a deterministic algorithmic budget, not a CPU or wall-clock limit.
/// Each completed candidate additionally compresses at most roughly `2 * max_object_bytes + 20`
/// instruction bytes. Identity hashing and ordinary compression retain the pack input bounds.
///
/// Selection compares complete entry headers, the format-sized base ID, and zlib bytes. A delta
/// must save at least `min_savings` bytes against the ordinary entry and strictly beat the current
/// best; ties retain the first candidate. Search is skipped when mandatory header/base-ID overhead
/// already rules out sufficient savings. The default saves at least 16 bytes. Output is
/// reproducible for fixed inputs, options, and compression backend/version, independent of caller
/// order.
///
/// Payloads are borrowed. Additional live scratch is bounded by a 32 KiB anchor table and roughly
/// eight times `max_object_bytes`, plus zlib scratch and allocator overhead. Entry metadata is
/// proportional to input count. These are not total-process heap bounds. Objects larger than the
/// configured size (or `u32::MAX`) use the streaming ordinary path. Zero bounds disable search;
/// no option value is invalid. Small objects under eight bytes also use ordinary entries.
#[derive(Debug, Clone, Copy)]
pub struct DeltaOptions {
    /// Preceding pack entries inspected per object (default 16), including unsuitable entries.
    pub window: usize,
    /// Suitable bases attempted per object (default 4).
    pub max_candidates: usize,
    /// Maximum eligible base and target payload size (default 1 MiB).
    pub max_object_bytes: usize,
    /// Maximum search units per target across all candidates (default 8 million).
    pub max_work: u64,
    /// Maximum emitted chain depth; ordinary entries have depth zero (default 4).
    pub max_depth: usize,
    /// Minimum complete-entry savings against ordinary encoding (default 16 bytes).
    pub min_savings: usize,
}

impl Default for DeltaOptions {
    fn default() -> Self {
        Self {
            window: 16,
            max_candidates: 4,
            max_object_bytes: 1024 * 1024,
            max_work: 8_000_000,
            max_depth: 4,
            min_savings: 16,
        }
    }
}

/// Search accounting for a completed pack; excludes ordinary hashing and zlib work.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeltaStats {
    /// Number of emitted REF_DELTA entries.
    pub entries: u32,
    /// Suitable candidate attempts, including work-exhausted attempts.
    pub candidates: u64,
    /// Consumed anchor, target-position, and byte-comparison units, saturating at `u64::MAX`.
    pub work: u64,
    /// Deepest emitted delta dependency chain.
    pub max_depth: usize,
}

// Original encoder based on Git's documented pack format, not Git implementation source.
pub(super) fn instructions(
    base: &[u8],
    target: &[u8],
    remaining: &mut u64,
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
) -> Result<Option<Vec<u8>>, PackWriteError> {
    let mut anchors = [usize::MAX; 4096];
    for offset in (0..base.len().saturating_sub(7)).step_by(16) {
        if !charge(remaining, check)? {
            return Ok(None);
        }
        let slot = slot(&base[offset..]);
        if anchors[slot] == usize::MAX {
            anchors[slot] = offset;
        }
    }
    let mut out = Vec::new();
    size(&mut out, base.len());
    size(&mut out, target.len());
    let (mut position, mut literal) = (0, 0);
    while target.len() - position >= 8 {
        if !charge(remaining, check)? {
            return Ok(None);
        }
        let offset = anchors[slot(&target[position..])];
        let mut length = 0;
        if offset != usize::MAX {
            while length < base.len() - offset && length < target.len() - position {
                if !charge(remaining, check)? {
                    return Ok(None);
                }
                if base[offset + length] != target[position + length] {
                    break;
                }
                length += 1;
            }
        }
        if length >= 8 {
            insert(&mut out, &target[literal..position]);
            copy(&mut out, offset as u32, length);
            position += length;
            literal = position;
        } else {
            position += 1;
        }
    }
    insert(&mut out, &target[literal..]);
    Ok(Some(out))
}

fn charge(
    remaining: &mut u64,
    check: &mut impl FnMut() -> Result<(), PackWriteError>,
) -> Result<bool, PackWriteError> {
    if *remaining == 0 {
        return Ok(false);
    }
    *remaining -= 1;
    if (*remaining).is_multiple_of(4096) {
        check()?;
    }
    Ok(true)
}

fn slot(bytes: &[u8]) -> usize {
    let word = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    (word.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 52) as usize
}

fn size(out: &mut Vec<u8>, mut value: usize) {
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
}

fn insert(out: &mut Vec<u8>, bytes: &[u8]) {
    for chunk in bytes.chunks(127) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
}

fn copy(out: &mut Vec<u8>, mut offset: u32, mut length: usize) {
    while length != 0 {
        let count = length.min(0xff_ffff);
        let encoded_size = if count == 65536 { 0 } else { count as u32 };
        let start = out.len();
        out.push(0x80);
        for (bit, byte) in offset.to_le_bytes().into_iter().enumerate() {
            if byte != 0 {
                out[start] |= 1 << bit;
                out.push(byte);
            }
        }
        for bit in 0..3 {
            let byte = (encoded_size >> (bit * 8)) as u8;
            if byte != 0 {
                out[start] |= 0x10 << bit;
                out.push(byte);
            }
        }
        length -= count;
        if length != 0 {
            offset += count as u32;
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::zero(0, &[0])]
    #[case::seven_bits(127, &[127])]
    #[case::eight_bits(128, &[128, 1])]
    #[case::sixteen_bits(65536, &[128, 128, 4])]
    fn size_vectors(#[case] value: usize, #[case] expected: &[u8]) {
        let mut out = vec![];
        size(&mut out, value);
        assert_eq!(out, expected);
    }

    #[rstest]
    #[case::one(0, 1, &[0x90, 1])]
    #[case::implicit_64k(0, 65536, &[0x80])]
    #[case::all_offset_bytes(0x12345678, 256, &[0xaf, 0x78, 0x56, 0x34, 0x12, 1])]
    #[case::sparse_offset(0x010001, 255, &[0x95, 1, 1, 255])]
    #[case::max_copy(0, 0xffffff, &[0xf0, 255, 255, 255])]
    #[case::split_copy(0, 0x1000000, &[0xf0, 255, 255, 255, 0x97, 255, 255, 255, 1])]
    fn copy_vectors(#[case] offset: u32, #[case] length: usize, #[case] expected: &[u8]) {
        let mut out = vec![];
        copy(&mut out, offset, length);
        assert_eq!(out, expected);
    }

    #[rstest]
    #[case::empty(0, &[])]
    #[case::one(1, &[1, 42])]
    #[case::maximum(127, &[127])]
    #[case::split(128, &[127])]
    fn insert_boundaries(#[case] length: usize, #[case] prefix: &[u8]) {
        let data = vec![42; length];
        let mut out = vec![];
        size(&mut out, 0);
        size(&mut out, length);
        let start = out.len();
        insert(&mut out, &data);
        assert!(out[start..].starts_with(prefix));
        assert_eq!(out.len() - start, length + length.div_ceil(127));
        let mut remaining = usize::MAX;
        let restored = crate::pack::delta::apply(&[], &out, length, &mut remaining).unwrap();
        assert_eq!(restored, data);
    }

    #[rstest]
    #[case::empty(vec![], vec![])]
    #[case::tiny(vec![1], vec![2])]
    #[case::repeated(vec![42; 65536], vec![42; 65536])]
    #[case::dissimilar(vec![0; 300], vec![255; 300])]
    #[case::binary((0..=255).cycle().take(100000).collect(), (0..=255).rev().cycle().take(100000).collect())]
    fn instructions_restore_exact_bytes(#[case] base: Vec<u8>, #[case] target: Vec<u8>) {
        let program = instructions(&base, &target, &mut 8_000_000, &mut || Ok(()))
            .unwrap()
            .unwrap();
        let mut remaining = usize::MAX;
        assert_eq!(
            crate::pack::delta::apply(&base, &program, target.len(), &mut remaining).unwrap(),
            target
        );
    }

    #[test]
    fn work_exhaustion_discards_partial_program() {
        let mut budget = 2;
        let result = instructions(&[1; 100], &[1; 100], &mut budget, &mut || Ok(())).unwrap();
        assert!(result.is_none());
        assert_eq!(budget, 0);
    }

    #[test]
    fn cancellation_interrupts_search() {
        let result = instructions(&[1; 65536], &[1; 65536], &mut 8192, &mut || {
            Err(PackWriteError::Limit("cancelled test"))
        });
        assert!(matches!(
            result,
            Err(PackWriteError::Limit("cancelled test"))
        ));
    }
}
