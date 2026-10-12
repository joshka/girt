use crate::ObjectReadError as Error;

/// Advances a borrowed byte cursor; all pack/delta integers use checked arithmetic.
pub(crate) fn byte(input: &mut &[u8]) -> Result<u8, Error> {
    // Hot in delta decoding: build the error only on failure.
    let Some((&value, tail)) = input.split_first() else {
        return Err(Error::Corrupt("truncated delta or entry"));
    };
    *input = tail;
    Ok(value)
}

pub(crate) fn size(
    input: &mut &[u8],
    mut value: usize,
    mut shift: u32,
    mut more: bool,
) -> Result<usize, Error> {
    while more {
        let next = byte(input)?;
        let part = (next & 0x7f) as usize;
        if shift >= usize::BITS || part > (usize::MAX >> shift) {
            return Err(Error::Corrupt("size overflow"));
        }
        value |= part << shift;
        shift += 7;
        more = next & 0x80 != 0;
    }
    Ok(value)
}

pub(crate) fn apply(
    base: &[u8],
    mut program: &[u8],
    max_size: usize,
    remaining: &mut usize,
) -> Result<Vec<u8>, Error> {
    let base_size = size(&mut program, 0, 0, true)?;
    if base_size != base.len() {
        return Err(Error::Corrupt("delta base size"));
    }
    let result_size = size(&mut program, 0, 0, true)?;
    if result_size > max_size {
        return Err(Error::Limit("object bytes"));
    }
    charge(remaining, result_size)?;
    // The declared size is unproven until the program produces it; reserve at most 1 MiB up
    // front and let larger results grow with the bytes actually copied.
    let mut result = Vec::with_capacity(result_size.min(INITIAL_RESERVATION));
    while !program.is_empty() {
        let instruction = byte(&mut program)?;
        let source = if instruction & 0x80 == 0 {
            if instruction == 0 {
                return Err(Error::Corrupt("reserved delta instruction"));
            }
            let length = instruction as usize;
            let Some(data) = program.get(..length) else {
                return Err(Error::Corrupt("truncated delta insertion"));
            };
            program = &program[length..];
            data
        } else {
            let mut offset = 0usize;
            let mut length = 0usize;
            for bit in 0..4 {
                if instruction & (1 << bit) != 0 {
                    offset |= (byte(&mut program)? as usize) << (bit * 8);
                }
            }
            for bit in 0..3 {
                if instruction & (0x10 << bit) != 0 {
                    length |= (byte(&mut program)? as usize) << (bit * 8);
                }
            }
            if length == 0 {
                length = 65536;
            }
            let Some(end) = offset.checked_add(length) else {
                return Err(Error::Corrupt("delta copy overflow"));
            };
            let Some(copied) = base.get(offset..end) else {
                return Err(Error::Corrupt("delta copy outside base"));
            };
            copied
        };
        if source.len() > result_size.saturating_sub(result.len()) {
            return Err(Error::Corrupt("delta exceeds result size"));
        }
        result.extend_from_slice(source);
    }
    if result.len() != result_size {
        return Err(Error::Corrupt("delta result size"));
    }
    Ok(result)
}

/// Largest buffer reserved from a declared, not yet produced, size.
pub(crate) const INITIAL_RESERVATION: usize = 1024 * 1024;

pub(crate) fn charge(remaining: &mut usize, size: usize) -> Result<(), Error> {
    *remaining = remaining
        .checked_sub(size)
        .ok_or(Error::Limit("cumulative decode bytes"))?;
    Ok(())
}
