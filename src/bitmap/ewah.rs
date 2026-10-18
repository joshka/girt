//! EWAH framing is checked without expanding runs; decoding has a separate shared work budget.
use std::sync::atomic::AtomicBool;

use super::{Error, bad, charge, check, tail_mask};

#[derive(Clone, Debug)]
pub(super) struct Ewah {
    start: usize,
    words: usize,
}

impl Ewah {
    pub(super) fn parse(
        bytes: &[u8],
        cursor: &mut usize,
        end: usize,
        objects: usize,
    ) -> Result<Self, Error> {
        let bits = word(bytes, *cursor)? as usize;
        let words = word(bytes, cursor.checked_add(4).ok_or(bad("EWAH offset"))?)? as usize;
        if bits.div_ceil(64) > objects.div_ceil(64) {
            return Err(bad("EWAH bit length"));
        }
        let start = cursor.checked_add(8).ok_or(bad("EWAH offset"))?;
        let data_end = words
            .checked_mul(8)
            .and_then(|length| start.checked_add(length))
            .ok_or(bad("EWAH words"))?;
        let next = data_end.checked_add(4).ok_or(bad("EWAH trailer"))?;
        if words == 0 || next > end {
            return Err(bad("EWAH range"));
        }
        let current = word(bytes, data_end)? as usize;
        let mut position = 0;
        let mut expanded = 0_usize;
        let mut last_rlw = 0;
        let mut last_word = 0;
        while position < words {
            last_rlw = position;
            let header = long(bytes, start + position * 8)?;
            position += 1;
            let run = ((header >> 1) & 0xffff_ffff) as usize;
            let literals = (header >> 33) as usize;
            expanded = expanded
                .checked_add(run)
                .and_then(|count| count.checked_add(literals))
                .ok_or(bad("EWAH expansion"))?;
            if expanded > bits.div_ceil(64) {
                return Err(bad("EWAH expansion"));
            }
            if run != 0 {
                last_word = if header & 1 != 0 { u64::MAX } else { 0 };
            }
            position = position
                .checked_add(literals)
                .filter(|&next| next <= words)
                .ok_or(bad("EWAH literal range"))?;
            if literals != 0 {
                last_word = long(bytes, start + (position - 1) * 8)?;
            }
        }
        if expanded != bits.div_ceil(64) || current != last_rlw {
            return Err(bad("EWAH length or current run"));
        }
        if expanded != 0 && last_word & !tail_mask(bits) != 0 {
            return Err(bad("EWAH trailing bits"));
        }
        if expanded != 0 && expanded == objects.div_ceil(64) && last_word & !tail_mask(objects) != 0
        {
            return Err(bad("bitmap outside object universe"));
        }
        *cursor = next;
        Ok(Self { start, words })
    }

    pub(super) fn xor_into(
        &self,
        bytes: &[u8],
        output: &mut [u64],
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        let mut position = 0;
        let mut out = 0;
        while position < self.words {
            check(cancel)?;
            let header = long(bytes, self.start + position * 8)?;
            position += 1;
            let run = ((header >> 1) & 0xffff_ffff) as usize;
            let literals = (header >> 33) as usize;
            charge(budget, run + literals)?;
            let run_end = out + run;
            if header & 1 != 0 {
                for word in output
                    .get_mut(out..run_end)
                    .ok_or(bad("decoded run range"))?
                {
                    check(cancel)?;
                    *word ^= u64::MAX;
                }
            }
            out = run_end;
            for _ in 0..literals {
                check(cancel)?;
                let value = long(bytes, self.start + position * 8)?;
                *output.get_mut(out).ok_or(bad("decoded literal range"))? ^= value;
                out += 1;
                position += 1;
            }
        }
        Ok(())
    }
}

pub(super) fn short(bytes: &[u8], start: usize) -> Result<u16, Error> {
    let end = start.checked_add(2).ok_or(bad("integer offset"))?;
    let value = bytes.get(start..end).ok_or(bad("integer range"))?;
    Ok(u16::from_be_bytes(
        value.try_into().map_err(|_| bad("integer width"))?,
    ))
}

pub(super) fn word(bytes: &[u8], start: usize) -> Result<u32, Error> {
    let end = start.checked_add(4).ok_or(bad("integer offset"))?;
    let value = bytes.get(start..end).ok_or(bad("integer range"))?;
    Ok(u32::from_be_bytes(
        value.try_into().map_err(|_| bad("integer width"))?,
    ))
}

pub(super) fn long(bytes: &[u8], start: usize) -> Result<u64, Error> {
    let end = start.checked_add(8).ok_or(bad("integer offset"))?;
    let value = bytes.get(start..end).ok_or(bad("integer range"))?;
    Ok(u64::from_be_bytes(
        value.try_into().map_err(|_| bad("integer width"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(bits: u32, words: &[u64], current: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&bits.to_be_bytes());
        bytes.extend_from_slice(&(words.len() as u32).to_be_bytes());
        for word in words {
            bytes.extend_from_slice(&word.to_be_bytes());
        }
        bytes.extend_from_slice(&current.to_be_bytes());
        bytes
    }

    #[test]
    fn runs_literals_and_xor_decode_with_shared_budget() {
        // Two one-words followed by a literal, then one zero-word.
        let bytes = encoded(256, &[1 | (2 << 1) | (1 << 33), 5, 1 << 1], 2);
        let ewah = Ewah::parse(&bytes, &mut 0, bytes.len(), 256).unwrap();
        let mut output = [0; 4];
        ewah.xor_into(&bytes, &mut output, &mut 4, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(output, [u64::MAX, u64::MAX, 5, 0]);
        ewah.xor_into(&bytes, &mut output, &mut 4, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(output, [0; 4]);
        assert!(matches!(
            ewah.xor_into(&bytes, &mut output, &mut 3, &AtomicBool::new(false)),
            Err(Error::Limit(_))
        ));
    }

    #[test]
    fn rejects_invalid_framing_before_expansion() {
        for (bits, words, current, objects) in [
            (64, vec![u64::MAX], 0, 64),  // Huge run and literal lengths.
            (64, vec![1 << 33], 0, 64),   // Missing literal.
            (64, vec![1 << 1], 1, 64),    // Current RLW points outside the buffer.
            (1, vec![1 << 33, 2], 0, 1),  // Bit outside declared size.
            (64, vec![1 << 33, 2], 0, 1), // Bit outside actual inventory.
            (128, vec![1 << 1], 0, 128),  // Short expansion.
            (0, vec![], 0, 0),            // No RLW.
        ] {
            let bytes = encoded(bits, &words, current);
            assert!(Ewah::parse(&bytes, &mut 0, bytes.len(), objects).is_err());
        }
    }
}
