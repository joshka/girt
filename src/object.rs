use std::fmt;
use std::str::FromStr;

use sha1::{Digest, Sha1};

/// The hash format used by a Git object database.
///
/// Object codecs, loose/packed storage, references and index v2 support both formats.
/// Transport negotiation currently supports SHA-1 only.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ObjectFormat {
    /// Git's SHA-1 object format, with 20-byte identities.
    Sha1,
    /// Git's SHA-256 object format, with 32-byte identities.
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

/// A format-bearing Git identity, independent of object existence or kind.
///
/// The variants contain exactly the meaningful digest bytes. Ordering places SHA-1 before SHA-256,
/// then compares bytes lexicographically. Parsing infers the format from exactly 40 or 64 ASCII
/// hexadecimal digits; display is lowercase. Abbreviations are rejected. Hashing does not provide
/// collision detection or translate identities between formats.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ObjectId {
    /// A SHA-1 digest.
    Sha1([u8; 20]),
    /// A SHA-256 digest.
    Sha256([u8; 32]),
}

impl ObjectId {
    /// Hashes the canonical blob header and exact bytes in the selected format.
    pub fn for_blob(format: ObjectFormat, bytes: &[u8]) -> Self {
        format.hash_object(crate::ObjectKind::Blob, bytes)
    }

    #[cfg(test)]
    pub(crate) fn for_object(kind: &str, bytes: &[u8]) -> Self {
        hash_sha1(kind, bytes)
    }

    /// Constructs an identity from exactly the selected format's raw digest bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the byte length differs from the selected format.
    pub fn from_bytes(format: ObjectFormat, bytes: &[u8]) -> Result<Self, ParseObjectIdError> {
        match format {
            ObjectFormat::Sha1 => bytes.try_into().map(Self::Sha1),
            ObjectFormat::Sha256 => bytes.try_into().map(Self::Sha256),
        }
        .map_err(|_| ParseObjectIdError)
    }

    /// Parses full hexadecimal digits and requires the selected format.
    ///
    /// # Errors
    ///
    /// Rejects wrong widths, non-ASCII/non-hexadecimal bytes, and the other format.
    pub fn from_hex(format: ObjectFormat, value: &str) -> Result<Self, ParseObjectIdError> {
        let id: Self = value.parse()?;
        if id.format() != format {
            return Err(ParseObjectIdError);
        }
        Ok(id)
    }

    /// Returns the digest format, without inspecting storage.
    pub const fn format(self) -> ObjectFormat {
        match self {
            Self::Sha1(_) => ObjectFormat::Sha1,
            Self::Sha256(_) => ObjectFormat::Sha256,
        }
    }

    /// Borrows only the meaningful digest bytes (20 or 32 bytes).
    pub const fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Sha1(bytes) => bytes,
            Self::Sha256(bytes) => bytes,
        }
    }

    /// Returns the all-zero sentinel for a selected format; no existence check occurs.
    pub const fn null(format: ObjectFormat) -> Self {
        match format {
            ObjectFormat::Sha1 => Self::Sha1([0; 20]),
            ObjectFormat::Sha256 => Self::Sha256([0; 32]),
        }
    }

    /// Reports whether every meaningful digest byte is zero.
    pub fn is_null(self) -> bool {
        self.as_bytes().iter().all(|&b| b == 0)
    }

    pub(crate) fn require_sha1(self) -> Result<(), ObjectFormatError> {
        self.require_format(ObjectFormat::Sha1)
    }

    pub(crate) fn require_format(self, expected: ObjectFormat) -> Result<(), ObjectFormatError> {
        if self.format() == expected {
            Ok(())
        } else {
            Err(ObjectFormatError {
                expected,
                actual: self.format(),
            })
        }
    }
}

impl ObjectFormat {
    /// Number of raw bytes in a digest of this format.
    pub const fn digest_len(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }

    /// Hashes canonical Git framing and an uninterpreted object payload.
    ///
    /// Does not validate the payload or translate embedded object references. Storage and codecs
    /// support both formats. The caller chooses the format appropriate to the payload.
    ///
    /// ```
    /// use girt::{ObjectFormat, ObjectId, ObjectKind};
    /// let id = ObjectFormat::Sha256.hash_object(ObjectKind::Blob, b"hello\n");
    /// assert_eq!(id.format(), ObjectFormat::Sha256);
    /// assert_eq!(id.to_string().parse::<ObjectId>()?, id);
    /// assert_eq!(id.as_bytes().len(), 32);
    /// # Ok::<(), girt::ParseObjectIdError>(())
    /// ```
    pub fn hash_object(self, kind: crate::ObjectKind, bytes: &[u8]) -> ObjectId {
        match self {
            Self::Sha1 => hash_sha1(kind.as_str(), bytes),
            Self::Sha256 => {
                let mut hash = sha2::Sha256::new();
                hash.update(object_header(kind.as_str(), bytes.len()));
                hash.update(bytes);
                ObjectId::Sha256(hash.finalize().into())
            }
        }
    }
}

/// Incremental checksum in the repository's selected storage format.
#[derive(Clone)]
pub(crate) enum Hasher {
    Sha1(Sha1),
    Sha256(sha2::Sha256),
}

impl Hasher {
    pub(crate) fn new(format: ObjectFormat) -> Self {
        match format {
            ObjectFormat::Sha1 => Self::Sha1(Sha1::new()),
            ObjectFormat::Sha256 => Self::Sha256(sha2::Sha256::new()),
        }
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha1(hash) => hash.update(bytes),
            Self::Sha256(hash) => hash.update(bytes),
        }
    }

    pub(crate) fn finalize(self) -> ObjectId {
        match self {
            Self::Sha1(hash) => ObjectId::Sha1(hash.finalize().into()),
            Self::Sha256(hash) => ObjectId::Sha256(hash.finalize().into()),
        }
    }
}

impl ObjectFormat {
    pub(crate) fn checksum(self, bytes: &[u8]) -> ObjectId {
        let mut hash = Hasher::new(self);
        hash.update(bytes);
        hash.finalize()
    }
}

fn hash_sha1(kind: &str, bytes: &[u8]) -> ObjectId {
    let mut hash = Sha1::new();
    hash.update(object_header(kind, bytes.len()));
    hash.update(bytes);
    ObjectId::Sha1(hash.finalize().into())
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.as_bytes() {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for ObjectId {
    type Err = ParseObjectIdError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let format = match value.len() {
            40 => ObjectFormat::Sha1,
            64 => ObjectFormat::Sha256,
            _ => return Err(ParseObjectIdError),
        };
        if !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ParseObjectIdError);
        }
        let mut id = Self::null(format);
        let bytes: &mut [u8] = match &mut id {
            Self::Sha1(bytes) => bytes,
            Self::Sha256(bytes) => bytes,
        };
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| ParseObjectIdError)?;
        }
        Ok(id)
    }
}

/// Invalid raw digest length or a string other than 40/64 ASCII hexadecimal digits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("expected a format-sized digest or 40/64 hexadecimal digits")]
pub struct ParseObjectIdError;

/// An identity uses a format unsupported by the selected operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("object format mismatch: expected {expected}, got {actual}")]
pub struct ObjectFormatError {
    /// Format required by the operation.
    pub expected: ObjectFormat,
    /// Format carried by the supplied identity.
    pub actual: ObjectFormat,
}

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
    let mut encoded = object_header("blob", bytes.len()).into_bytes();
    encoded.extend_from_slice(bytes);
    encoded
}

pub(crate) fn object_header(kind: &str, length: usize) -> String {
    format!("{kind} {length}\0")
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// Checks exact headers at decimal-length transitions and preservation of all byte values.
    /// Literal identities were independently verified with Python and Git; provenance is in
    /// docs/compatibility.md.
    #[rstest]
    #[case::nine_bytes(vec![b'x'; 9], b"blob 9\0", "aab1169962b9fd16c9465117c12cad0ffd8bffac")]
    #[case::ten_bytes(vec![b'x'; 10], b"blob 10\0", "72035e10b5524757f990eb198acfce358b268c12")]
    #[case::ninety_nine_bytes(vec![b'x'; 99], b"blob 99\0", "9e84ea89f5f5200b6ef0350a87885d5d3f39e453")]
    #[case::one_hundred_bytes(vec![b'x'; 100], b"blob 100\0", "f6be7cae2045aac11912ea642bf7f9d5d261f63b")]
    #[case::all_byte_values((0..=255).collect(), b"blob 256\0", "c86626638e0bc8cf47ca49bb1525b40e9737ee64")]
    fn encodes_and_identifies_blob_bytes(
        #[case] bytes: Vec<u8>,
        #[case] header: &[u8],
        #[case] expected_id: &str,
    ) {
        let expected_encoding = [header, bytes.as_slice()].concat();

        assert_eq!(encode_blob(&bytes), expected_encoding);
        assert_eq!(
            ObjectId::for_blob(crate::ObjectFormat::Sha1, &bytes).to_string(),
            expected_id
        );
    }

    /// Keeps format display names compatible with the names used in Git configuration.
    #[rstest]
    #[case::sha1(ObjectFormat::Sha1, "sha1")]
    #[case::sha256(ObjectFormat::Sha256, "sha256")]
    fn displays_git_object_format_names(#[case] format: ObjectFormat, #[case] expected: &str) {
        assert_eq!(format.to_string(), expected);
    }

    /// Accepts either hexadecimal case as the same full SHA-1 identity.
    #[rstest]
    #[case::lowercase("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")]
    #[case::uppercase("E69DE29BB2D1D6434B8B29AE775AD8C2E48C5391")]
    fn parses_full_sha1_identifiers(#[case] value: &str) {
        let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"");
        assert_eq!(value.parse(), Ok(id));
    }

    /// Checks that raw-byte construction and access preserve all identity bytes.
    #[test]
    fn preserves_raw_identity_bytes() {
        let id = ObjectId::for_blob(crate::ObjectFormat::Sha1, b"");
        assert_eq!(
            ObjectId::from_bytes(id.format(), id.as_bytes()).unwrap(),
            id
        );
    }

    /// Rejects abbreviated, non-hexadecimal, and non-ASCII identity strings.
    #[rstest]
    #[case::abbreviated("abc".to_owned())]
    #[case::non_hexadecimal("g".repeat(40))]
    #[case::non_ascii("é".repeat(20))]
    fn rejects_invalid_sha1_identifiers(#[case] value: String) {
        assert_eq!(value.parse::<ObjectId>(), Err(ParseObjectIdError));
    }
}

#[cfg(test)]
mod format_tests {
    use std::collections::{BTreeMap, HashMap};

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1, 20)]
    #[case::sha256(ObjectFormat::Sha256, 32)]
    fn preserves_full_digest_and_null(#[case] format: ObjectFormat, #[case] length: usize) {
        let mut bytes = vec![0; length];
        bytes[length - 1] = 0xff;
        let id = ObjectId::from_bytes(format, &bytes).unwrap();
        assert_eq!(id.as_bytes(), bytes);
        assert_eq!(format.digest_len(), length);
        assert!(!id.is_null());
        assert!(ObjectId::null(format).is_null());
        assert_eq!(ObjectId::null(format).to_string(), "0".repeat(length * 2));
        assert_eq!(
            ObjectId::from_hex(format, &id.to_string().to_uppercase()),
            Ok(id)
        );
    }

    #[rstest]
    #[case::sha1_short(ObjectFormat::Sha1, 19)]
    #[case::sha1_long(ObjectFormat::Sha1, 21)]
    #[case::sha1_other(ObjectFormat::Sha1, 32)]
    #[case::sha256_short(ObjectFormat::Sha256, 31)]
    #[case::sha256_long(ObjectFormat::Sha256, 33)]
    #[case::sha256_other(ObjectFormat::Sha256, 20)]
    fn rejects_wrong_raw_width(#[case] format: ObjectFormat, #[case] length: usize) {
        assert_eq!(
            ObjectId::from_bytes(format, &vec![0; length]),
            Err(ParseObjectIdError)
        );
    }

    #[rstest]
    #[case::short_sha1("0".repeat(39))]
    #[case::long_sha1("0".repeat(41))]
    #[case::short_sha256("0".repeat(63))]
    #[case::long_sha256("0".repeat(65))]
    #[case::invalid_sha256("g".repeat(64))]
    #[case::unicode_sha256("é".repeat(32))]
    fn rejects_invalid_hex(#[case] value: String) {
        assert_eq!(value.parse::<ObjectId>(), Err(ParseObjectIdError));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1, ObjectFormat::Sha256)]
    #[case::sha256(ObjectFormat::Sha256, ObjectFormat::Sha1)]
    fn rejects_other_formats_hex(#[case] expected: ObjectFormat, #[case] actual: ObjectFormat) {
        assert_eq!(
            ObjectId::from_hex(expected, &ObjectId::null(actual).to_string()),
            Err(ParseObjectIdError)
        );
    }

    /// Equal prefixes, nulls and differences in the final byte remain distinct lookup keys.
    #[test]
    fn mixed_format_keys_preserve_all_bytes() {
        let sha1 = ObjectId::null(ObjectFormat::Sha1);
        let sha256 = ObjectId::null(ObjectFormat::Sha256);
        let mut bytes = [0; 32];
        bytes[31] = 1;
        let tail = ObjectId::Sha256(bytes);
        let ordered = BTreeMap::from([(tail, "tail"), (sha256, "sha256"), (sha1, "sha1")]);
        let hashed = HashMap::from([(sha1, 1), (sha256, 2), (tail, 3)]);
        assert_eq!(
            ordered.into_keys().collect::<Vec<_>>(),
            [sha1, sha256, tail]
        );
        assert_eq!(hashed.len(), 3);
        assert_eq!(hashed[&sha1], 1);
        assert_eq!(hashed[&sha256], 2);
        assert_eq!(hashed[&tail], 3);
    }
}
