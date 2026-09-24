use crate::ObjectReadError as Error;

/// Advances a borrowed byte cursor; all pack/delta integers use checked arithmetic.
pub(super) fn byte(input: &mut &[u8]) -> Result<u8, Error> {
    let (&value, tail) = input
        .split_first()
        .ok_or(Error::Corrupt("truncated delta or entry"))?;
    *input = tail;
    Ok(value)
}

pub(super) fn size(
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

pub(super) fn apply(
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
    let mut result = Vec::new();
    while !program.is_empty() {
        let instruction = byte(&mut program)?;
        let source = if instruction & 0x80 == 0 {
            if instruction == 0 {
                return Err(Error::Corrupt("reserved delta instruction"));
            }
            let length = instruction as usize;
            let data = program
                .get(..length)
                .ok_or(Error::Corrupt("truncated delta insertion"))?;
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
            let end = offset
                .checked_add(length)
                .ok_or(Error::Corrupt("delta copy overflow"))?;
            base.get(offset..end)
                .ok_or(Error::Corrupt("delta copy outside base"))?
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

pub(super) fn charge(remaining: &mut usize, size: usize) -> Result<(), Error> {
    *remaining = remaining
        .checked_sub(size)
        .ok_or(Error::Limit("cumulative decode bytes"))?;
    Ok(())
}
