use super::ewah::word;
use super::{Error, Limits, bad, bounds};
use crate::{ObjectFormat, ObjectId};

/// A checked `.rev` permutation from bitmap order to ordinary pack/MIDX index positions.
///
/// Parsing proves permutation and checksum consistency, not that the order matches pack offsets.
/// Consumers must bind these positions to the associated inventory before interpreting bitmaps.
#[derive(Clone, Debug)]
pub struct ReverseIndex {
    anchor: ObjectId,
    positions: Vec<u32>,
}

impl ReverseIndex {
    /// Parses reverse-index v1 under default bounds, requiring the expected inventory checksum.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for damaged, mismatched, unsupported or over-budget data.
    pub fn parse(bytes: &[u8], expected: ObjectId, objects: usize) -> Result<Self, Error> {
        Self::parse_with(bytes, expected, objects, Limits::default())
    }

    /// Checks header, hash selection, exact table size, permutation and both trailer checksums.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] before allocating a permutation beyond the supplied object bound.
    pub fn parse_with(
        bytes: &[u8],
        expected: ObjectId,
        objects: usize,
        limits: Limits,
    ) -> Result<Self, Error> {
        bounds(bytes, objects, limits)?;
        let width = expected.format().digest_len();
        let size = objects
            .checked_mul(4)
            .and_then(|length| length.checked_add(12 + 2 * width))
            .ok_or(bad("reverse index size"))?;
        if bytes.len() != size || bytes.get(..4) != Some(b"RIDX") {
            return Err(bad("reverse index header or size"));
        }
        if word(bytes, 4)? != 1 {
            return Err(Error::Unsupported("reverse index version"));
        }
        let hash = match expected.format() {
            ObjectFormat::Sha1 => 1,
            ObjectFormat::Sha256 => 2,
        };
        if word(bytes, 8)? != hash {
            return Err(Error::Binding);
        }
        let checksum_start = bytes.len() - width;
        if expected
            .format()
            .checksum(&bytes[..checksum_start])
            .as_bytes()
            != &bytes[checksum_start..]
        {
            return Err(Error::Checksum);
        }
        let end = checksum_start - width;
        if bytes.get(end..checksum_start) != Some(expected.as_bytes()) {
            return Err(Error::Binding);
        }
        let mut seen = vec![false; objects];
        let mut positions = Vec::with_capacity(objects);
        for row in bytes
            .get(12..end)
            .ok_or(bad("reverse table"))?
            .as_chunks::<4>()
            .0
        {
            let position = word(row, 0)?;
            let slot = seen
                .get_mut(position as usize)
                .ok_or(bad("reverse position"))?;
            if *slot {
                return Err(bad("duplicate reverse position"));
            }
            *slot = true;
            positions.push(position);
        }
        Ok(Self {
            anchor: expected,
            positions,
        })
    }

    /// Associated pack or MIDX checksum.
    pub fn anchor(&self) -> ObjectId {
        self.anchor
    }
    /// Bitmap-order positions mapped to ordinary index positions.
    pub fn positions(&self) -> impl ExactSizeIterator<Item = u32> + '_ {
        self.positions.iter().copied()
    }
    /// Ordinary-index position at one bitmap position, or `None` beyond the table.
    pub fn index_position(&self, bitmap_position: usize) -> Option<u32> {
        self.positions.get(bitmap_position).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(format: ObjectFormat, positions: &[u32]) -> (ObjectId, Vec<u8>) {
        let anchor = format.checksum(b"inventory");
        let mut bytes = b"RIDX".to_vec();
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(
            &(if format == ObjectFormat::Sha1 {
                1_u32
            } else {
                2
            })
            .to_be_bytes(),
        );
        for position in positions {
            bytes.extend_from_slice(&position.to_be_bytes());
        }
        bytes.extend_from_slice(anchor.as_bytes());
        let checksum = format.checksum(&bytes);
        bytes.extend_from_slice(checksum.as_bytes());
        (anchor, bytes)
    }

    #[test]
    fn checksum_valid_invalid_permutations_are_rejected() {
        for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
            for positions in [[0, 0], [0, 2]] {
                let (anchor, bytes) = file(format, &positions);
                assert!(matches!(
                    ReverseIndex::parse(&bytes, anchor, 2),
                    Err(Error::Malformed(_))
                ));
            }
            let (anchor, bytes) = file(format, &[1, 0]);
            assert_eq!(
                ReverseIndex::parse(&bytes, anchor, 2)
                    .unwrap()
                    .positions()
                    .collect::<Vec<_>>(),
                [1, 0]
            );
            for end in 0..bytes.len() {
                assert!(ReverseIndex::parse(&bytes[..end], anchor, 2).is_err());
            }
        }
    }
}
