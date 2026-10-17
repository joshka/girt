//! Validated, format-bearing hexadecimal prefixes.
use std::fmt;

use super::{ObjectFormat, ObjectId};

/// Four through the full hash width hexadecimal digits in one object format.
///
/// Parsing normalizes uppercase digits and preserves odd widths. It does not establish existence
/// or uniqueness; use [`crate::Objects::resolve_prefix`] for that.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ObjectIdPrefix {
    lower: ObjectId,
    digits: usize,
}

/// An invalid prefix width or non-hexadecimal spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("object prefix must contain 4 through the format's full width hexadecimal digits")]
#[non_exhaustive]
pub struct ParseObjectIdPrefixError;

impl ObjectIdPrefix {
    /// Parses ASCII hexadecimal digits for the selected repository format.
    ///
    /// # Errors
    ///
    /// Rejects fewer than four digits, more than the full width, or non-hexadecimal input.
    ///
    /// ```
    /// use girt::{ObjectFormat, ObjectIdPrefix};
    /// let prefix = ObjectIdPrefix::from_hex(ObjectFormat::Sha1, "B6FC4")?;
    /// assert_eq!(prefix.to_string(), "b6fc4");
    /// # Ok::<(), girt::ParseObjectIdPrefixError>(())
    /// ```
    pub fn from_hex(format: ObjectFormat, text: &str) -> Result<Self, ParseObjectIdPrefixError> {
        let width = format.digest_len() * 2;
        if !(4..=width).contains(&text.len()) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ParseObjectIdPrefixError);
        }
        let padded = format!("{text:0<width$}");
        let lower = ObjectId::from_hex(format, &padded).map_err(|_| ParseObjectIdPrefixError)?;
        Ok(Self {
            lower,
            digits: text.len(),
        })
    }

    /// The selected hash format.
    pub fn object_format(self) -> ObjectFormat {
        self.lower.format()
    }

    /// Number of hexadecimal digits, including an optional final half-byte.
    pub fn hex_len(self) -> usize {
        self.digits
    }

    /// Tests the prefix without reading storage.
    pub fn matches(self, id: ObjectId) -> bool {
        if id.format() != self.object_format() {
            return false;
        }
        let bytes = self.digits / 2;
        let (left, right) = (self.lower.as_bytes(), id.as_bytes());
        left.get(..bytes) == right.get(..bytes)
            && (self.digits.is_multiple_of(2)
                || left
                    .get(bytes)
                    .zip(right.get(bytes))
                    .is_some_and(|(a, b)| a >> 4 == b >> 4))
    }

    pub(crate) fn lower(self) -> ObjectId {
        self.lower
    }
}

impl fmt::Display for ObjectIdPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.lower.to_string();
        f.write_str(hex.get(..self.digits).ok_or(fmt::Error)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_width_hex_and_half_bytes() {
        for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
            for text in ["", "abc", "abcdg", "éabcd"] {
                assert!(ObjectIdPrefix::from_hex(format, text).is_err());
            }
            assert!(
                ObjectIdPrefix::from_hex(format, &"a".repeat(format.digest_len() * 2 + 1)).is_err()
            );
            let prefix = ObjectIdPrefix::from_hex(format, "ABCD1").unwrap();
            assert_eq!(prefix.to_string(), "abcd1");
            let id = ObjectId::from_hex(
                format,
                &format!("abcd1{:0<width$}", "", width = format.digest_len() * 2 - 5),
            )
            .unwrap();
            assert!(prefix.matches(id));
            assert!(
                !ObjectIdPrefix::from_hex(format, "abcd2")
                    .unwrap()
                    .matches(id)
            );
        }
    }
}
