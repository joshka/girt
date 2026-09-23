use std::fmt;
use std::str::FromStr;

use sha1::{Digest, Sha1};

/// The hash format used by a Git object database.
///
/// Recognizing a format does not imply storage support: [`crate::LooseObjects`] and [`ObjectId`]
/// currently support only SHA-1.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ObjectFormat {
    /// Git's SHA-1 object format, with 20-byte identities.
    Sha1,
    /// Git's SHA-256 object format, with 32-byte identities; currently unsupported by girt.
    Sha256,
}

impl fmt::Display for ObjectFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        })
    }
}

/// A 20-byte SHA-1 Git object identity, independent of object existence or type.
///
/// Parsing accepts exactly 40 ASCII hexadecimal digits, in either case; display uses lowercase.
/// SHA-256 identifiers and abbreviated identifiers are rejected. This type does not provide SHA-1
/// collision detection.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectId([u8; 20]);

impl ObjectId {
    /// Hashes the canonical Git blob header and the exact bytes, without text conversion.
    ///
    /// ```
    /// use girt::ObjectId;
    /// assert_eq!(
    ///     ObjectId::for_blob(b"").to_string(),
    ///     "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
    /// );
    /// ```
    pub fn for_blob(bytes: &[u8]) -> Self {
        let mut hash = Sha1::new();
        hash.update(blob_header(bytes.len()));
        hash.update(bytes);
        Self(hash.finalize().into())
    }

    /// Constructs an identity from raw SHA-1 bytes without checking object existence.
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// Borrows the raw SHA-1 bytes.
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for ObjectId {
    type Err = ParseObjectIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ParseObjectIdError);
        }
        let mut bytes = [0; 20];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| ParseObjectIdError)?;
        }
        Ok(Self(bytes))
    }
}

/// An identifier was not exactly 40 ASCII hexadecimal digits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseObjectIdError;

impl fmt::Display for ParseObjectIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected a 40-digit SHA-1 object identifier")
    }
}

impl std::error::Error for ParseObjectIdError {}

/// Encodes a blob as `blob <decimal byte length>\0` followed by its unchanged bytes.
///
/// A blob is an uninterpreted byte sequence; filenames and text encodings are not part of it.
/// [`ObjectId::for_blob`] hashes this representation to derive the blob's identity.
///
/// The returned buffer is uncompressed; loose storage adds zlib compression. No text encoding,
/// newline normalization, or Git attributes are applied.
///
/// ```
/// assert_eq!(girt::encode_blob(b"a\0\xff"), b"blob 3\0a\0\xff");
/// ```
pub fn encode_blob(bytes: &[u8]) -> Vec<u8> {
    let mut encoded = blob_header(bytes.len()).into_bytes();
    encoded.extend_from_slice(bytes);
    encoded
}

pub(crate) fn blob_header(length: usize) -> String {
    format!("blob {length}\0")
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1, "sha1")]
    #[case::sha256(ObjectFormat::Sha256, "sha256")]
    fn displays_git_object_format_names(#[case] format: ObjectFormat, #[case] expected: &str) {
        assert_eq!(format.to_string(), expected);
    }

    #[rstest]
    #[case::lowercase("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")]
    #[case::uppercase("E69DE29BB2D1D6434B8B29AE775AD8C2E48C5391")]
    fn parses_full_sha1_identifiers(#[case] value: &str) {
        let id = ObjectId::for_blob(b"");
        assert_eq!(value.parse(), Ok(id));
    }

    #[test]
    fn preserves_raw_identity_bytes() {
        let id = ObjectId::for_blob(b"");
        assert_eq!(ObjectId::from_bytes(*id.as_bytes()), id);
    }

    #[rstest]
    #[case::abbreviated("abc".to_owned())]
    #[case::sha256("0".repeat(64))]
    #[case::non_hexadecimal("g".repeat(40))]
    #[case::non_ascii("é".repeat(20))]
    fn rejects_invalid_sha1_identifiers(#[case] value: String) {
        assert_eq!(value.parse::<ObjectId>(), Err(ParseObjectIdError));
    }
}
