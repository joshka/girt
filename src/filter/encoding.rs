//! The `working-tree-encoding` attribute for the UTF-16 and UTF-32 families.

/// Byte order of one code unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Order {
    Little,
    Big,
}

/// How a working-tree encoding treats a byte order mark.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Bom {
    /// `UTF-16`, `UTF-32`: required when reading, written in the platform's order.
    Required,
    /// `UTF-16LE`, `UTF-32BE` and so on: a leading mark is refused.
    Prohibited(Order),
    /// `UTF-16LE-BOM`: always written. Reading treats it as the unsuffixed encoding with an
    /// optional mark: either mark sets the order, and unmarked content uses the platform order.
    Written(Order),
}

/// A working-tree encoding girt converts natively.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Encoding {
    /// Bytes per code unit: 2 or 4.
    width: usize,
    bom: Bom,
}

/// Why converting through `working-tree-encoding` failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Failure {
    /// `UTF-16` or `UTF-32` content without a byte order mark.
    BomRequired,
    /// Content starts with a byte order mark the encoding's name already fixes.
    BomProhibited,
    /// The bytes are not valid in the source encoding.
    Invalid,
}

/// The byte order `iconv` writes for `UTF-16` and `UTF-32` without a suffix: big-endian on macOS,
/// the machine's order with glibc.
const PLATFORM: Order = if cfg!(target_os = "macos") || cfg!(target_endian = "big") {
    Order::Big
} else {
    Order::Little
};

impl Encoding {
    /// Parses an encoding name, ignoring ASCII case. `Ok(None)` is UTF-8, which needs no
    /// conversion; `Err(())` is an encoding girt does not convert.
    pub(super) fn parse(name: &[u8]) -> Result<Option<Self>, ()> {
        let name = name.to_ascii_uppercase();
        let (width, rest) = match name.as_slice() {
            b"UTF-8" | b"UTF8" => return Ok(None),
            _ if name.starts_with(b"UTF-16") => (2, &name[6..]),
            _ if name.starts_with(b"UTF16") => (2, &name[5..]),
            _ if name.starts_with(b"UTF-32") => (4, &name[6..]),
            _ if name.starts_with(b"UTF32") => (4, &name[5..]),
            _ => return Err(()),
        };
        let bom = match rest {
            b"" => Bom::Required,
            b"LE" => Bom::Prohibited(Order::Little),
            b"BE" => Bom::Prohibited(Order::Big),
            b"LE-BOM" => Bom::Written(Order::Little),
            b"BE-BOM" => Bom::Written(Order::Big),
            _ => return Err(()),
        };
        Ok(Some(Self { width, bom }))
    }

    fn mark(&self, order: Order) -> Vec<u8> {
        unit(0xfeff, self.width, order)
    }

    fn starting_order(&self, bytes: &[u8]) -> Option<Order> {
        [Order::Little, Order::Big]
            .into_iter()
            .find(|order| bytes.starts_with(&self.mark(*order)))
    }

    /// Decodes working-tree bytes to UTF-8.
    pub(super) fn decode(&self, bytes: &[u8]) -> Result<Vec<u8>, Failure> {
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let found = self.starting_order(bytes);
        let (order, body) = match (self.bom, found) {
            (Bom::Required, Some(order)) => (order, &bytes[self.width..]),
            (Bom::Required, None) => return Err(Failure::BomRequired),
            (Bom::Prohibited(_), Some(_)) => return Err(Failure::BomProhibited),
            (Bom::Prohibited(order), None) => (order, bytes),
            (Bom::Written(_), Some(found)) => (found, &bytes[self.width..]),
            (Bom::Written(_), None) => (PLATFORM, bytes),
        };
        if body.len() % self.width != 0 {
            return Err(Failure::Invalid);
        }
        let mut units = body
            .chunks_exact(self.width)
            .map(|chunk| read(chunk, order));
        let mut text = String::with_capacity(body.len() / self.width);
        while let Some(unit) = units.next() {
            let scalar = match unit {
                0xd800..=0xdbff if self.width == 2 => {
                    let low = units.next().ok_or(Failure::Invalid)?;
                    if !(0xdc00..=0xdfff).contains(&low) {
                        return Err(Failure::Invalid);
                    }
                    0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00)
                }
                _ => unit,
            };
            text.push(char::from_u32(scalar).ok_or(Failure::Invalid)?);
        }
        Ok(text.into_bytes())
    }

    /// Encodes UTF-8 stored bytes for the working tree.
    pub(super) fn encode(&self, bytes: &[u8]) -> Result<Vec<u8>, Failure> {
        let text = std::str::from_utf8(bytes).map_err(|_| Failure::Invalid)?;
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let (order, mut out) = match self.bom {
            Bom::Required => (PLATFORM, self.mark(PLATFORM)),
            Bom::Written(order) => (order, self.mark(order)),
            Bom::Prohibited(order) => (order, Vec::new()),
        };
        out.reserve(text.len() * self.width);
        if self.width == 2 {
            for unit in text.encode_utf16() {
                out.extend(unit_bytes(u32::from(unit), 2, order));
            }
        } else {
            for scalar in text.chars() {
                out.extend(unit_bytes(u32::from(scalar), 4, order));
            }
        }
        Ok(out)
    }
}

fn read(chunk: &[u8], order: Order) -> u32 {
    let fold = |value: u32, byte: &u8| (value << 8) | u32::from(*byte);
    match order {
        Order::Big => chunk.iter().fold(0, fold),
        Order::Little => chunk.iter().rev().fold(0, fold),
    }
}

fn unit_bytes(value: u32, width: usize, order: Order) -> impl Iterator<Item = u8> {
    let bytes = value.to_be_bytes();
    let bytes = bytes.into_iter().skip(4 - width);
    let mut bytes: Vec<u8> = bytes.collect();
    if order == Order::Little {
        bytes.reverse();
    }
    bytes.into_iter()
}

fn unit(value: u32, width: usize, order: Order) -> Vec<u8> {
    unit_bytes(value, width, order).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoding(name: &str) -> Encoding {
        Encoding::parse(name.as_bytes()).unwrap().unwrap()
    }

    #[test]
    fn names_follow_git() {
        assert_eq!(Encoding::parse(b"utf-8"), Ok(None));
        assert!(Encoding::parse(b"latin1").is_err());
        assert!(Encoding::parse(b"UTF-16XE").is_err());
        assert_eq!(encoding("utf-16le"), encoding("UTF16LE"));
    }

    #[test]
    fn decoding_matches_git_add() {
        // Observed: `hé\n` in each encoding is stored as UTF-8.
        let stored = "hé\n".as_bytes();
        let cases: &[(&str, &[u8])] = &[
            ("UTF-16", b"\xfe\xff\x00h\x00\xe9\x00\n"),
            ("UTF-16", b"\xff\xfeh\x00\xe9\x00\n\x00"),
            ("UTF-16LE", b"h\x00\xe9\x00\n\x00"),
            ("UTF-16BE", b"\x00h\x00\xe9\x00\n"),
            ("UTF-16LE-BOM", b"\xff\xfeh\x00\xe9\x00\n\x00"),
            // Observed: a big-endian mark is accepted too.
            ("UTF-16LE-BOM", b"\xfe\xff\x00h\x00\xe9\x00\n"),
            ("UTF-32LE", b"h\x00\x00\x00\xe9\x00\x00\x00\n\x00\x00\x00"),
        ];
        for (name, bytes) in cases {
            assert_eq!(encoding(name).decode(bytes).unwrap(), stored, "{name}");
        }
        assert_eq!(
            encoding("UTF-16").decode(b"h\x00"),
            Err(Failure::BomRequired)
        );
        assert_eq!(
            encoding("UTF-16LE").decode(b"\xff\xfeh\x00"),
            Err(Failure::BomProhibited)
        );
        assert_eq!(
            encoding("UTF-16LE").decode(b"h\x00\x00"),
            Err(Failure::Invalid)
        );
        assert_eq!(
            encoding("UTF-16LE").decode(b"\x00\xd8a\x00"),
            Err(Failure::Invalid)
        );
        assert_eq!(encoding("UTF-16").decode(b""), Ok(Vec::new()));
    }

    #[test]
    fn unmarked_bom_variant_uses_the_platform_order() {
        // Observed on macOS: unmarked `00 68` is stored as `h`, and `68 00` as U+6800.
        let decoded = encoding("UTF-16LE-BOM").decode(b"\x00h").unwrap();
        let expected = if PLATFORM == Order::Big {
            "h"
        } else {
            "\u{6800}"
        };
        assert_eq!(decoded, expected.as_bytes());
    }

    #[test]
    fn encoding_round_trips_and_marks() {
        let text = "h\u{1F600}é\n".as_bytes();
        for name in ["UTF-16", "UTF-16LE", "UTF-16BE-BOM", "UTF-32", "UTF-32BE"] {
            let encoded = encoding(name).encode(text).unwrap();
            assert_eq!(encoding(name).decode(&encoded).unwrap(), text, "{name}");
        }
        assert_eq!(
            encoding("UTF-16LE-BOM").encode(b"h").unwrap(),
            b"\xff\xfeh\x00"
        );
        assert_eq!(encoding("UTF-16").encode(b"").unwrap(), b"");
        assert_eq!(encoding("UTF-16LE").encode(b"\xff"), Err(Failure::Invalid));
    }
}
