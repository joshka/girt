//! Partial-clone object filters, as `git clone --filter=<spec>` spells them.

use std::fmt;
use std::str::FromStr;

use crate::ObjectKind;

/// Which objects a partial fetch asks the source to leave out.
///
/// A filter is sent to the source as `filter <spec>`. The source then omits matching objects, and
/// the fetched packs only promise them. Parse a spec with [`str::parse`], which reads numbers as
/// Git does: `010` is octal and `0x10` is hex. [`Display`](fmt::Display) writes an equivalent
/// spec with decimal numbers and size suffixes expanded, so `blob:limit=1k` is written as
/// `blob:limit=1024`, as Git writes it. Git sends the parts of a `combine:` spec as written;
/// girt writes each part in the same canonical form and percent-encodes it again.
///
/// Parsing accepts `combine:` specs nested at most [`Self::MAX_NESTING`] levels deep. Git accepts
/// deeper nesting, but each level costs stack while parsing, matching, writing and dropping, so a
/// spec from an untrusted peer could otherwise exhaust it. Values built directly are not bounded.
///
/// Values built directly must be valid specs: a [`Self::Combine`] with no parts or a
/// [`Self::SparseOid`] with an empty name writes a spec that does not parse.
///
/// ```
/// use girt::fetch::ObjectFilter;
///
/// let filter: ObjectFilter = "blob:limit=1k".parse()?;
/// assert_eq!(filter, ObjectFilter::BlobLimit(1024));
/// assert_eq!(filter.to_string(), "blob:limit=1024");
///
/// let combined: ObjectFilter = "combine:blob:none+tree:0".parse()?;
/// assert_eq!(
///     combined,
///     ObjectFilter::Combine(vec![ObjectFilter::BlobNone, ObjectFilter::TreeDepth(0)])
/// );
/// # Ok::<(), girt::fetch::ObjectFilterParseError>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ObjectFilter {
    /// `blob:none`: leave out every blob.
    BlobNone,
    /// `blob:limit=<n>[kmg]`: leave out blobs of at least this many bytes.
    BlobLimit(u64),
    /// `tree:<depth>`: leave out trees and blobs this many levels or more below a commit's root
    /// tree. `tree:0` leaves out every tree and blob.
    TreeDepth(u64),
    /// `object:type=<type>`: keep only objects of this kind.
    ObjectType(ObjectKind),
    /// `sparse:oid=<blob-ish>`: keep only blobs that the sparse-checkout patterns stored in the
    /// named blob select. The source resolves the name, so it is kept as text.
    SparseOid(String),
    /// `combine:<spec>+<spec>...`: leave out an object if any part would leave it out.
    Combine(Vec<ObjectFilter>),
}

/// Why a filter spec could not be parsed.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ObjectFilterParseError {
    /// The spec is not one of the filter forms.
    #[error("invalid filter-spec '{0}'")]
    Unknown(String),
    /// The `blob:limit` size is not a number with an optional `k`, `m` or `g` suffix, or does
    /// not fit in 64 bits.
    #[error("invalid blob size limit '{0}'")]
    BlobLimit(String),
    /// The `tree` depth is not a number that fits in 64 bits.
    #[error("expected 'tree:<depth>', found '{0}'")]
    TreeDepth(String),
    /// The `object:type` value is not `blob`, `tree`, `commit` or `tag`.
    #[error("'{0}' is not a valid object type for 'object:type=<type>'")]
    ObjectType(String),
    /// `sparse:oid=` has no object name.
    #[error("expected an object name after 'sparse:oid='")]
    SparseOid,
    /// A `combine:` spec has no parts, or one of its parts is empty.
    #[error("empty part in combined filter-spec '{0}'")]
    EmptyCombinePart(String),
    /// A `combine:` part contains a character that must be percent-encoded.
    #[error("character '{0}' must be percent-encoded in a combined filter-spec part")]
    Unescaped(char),
    /// A `combine:` part decodes to bytes that are not UTF-8.
    #[error("combined filter-spec part '{0}' does not decode to UTF-8")]
    Escape(String),
    /// `combine:` specs are nested more than [`ObjectFilter::MAX_NESTING`] levels deep.
    #[error(
        "combined filter-spec nested more than {} levels deep",
        ObjectFilter::MAX_NESTING
    )]
    TooDeep,
}

impl ObjectFilter {
    /// The deepest `combine:` nesting that parsing accepts; `combine:blob:none` is one level.
    pub const MAX_NESTING: usize = 16;

    fn parse_nested(spec: &str, depth: usize) -> Result<Self, ObjectFilterParseError> {
        if let Some(parts) = spec.strip_prefix("combine:") {
            if depth == Self::MAX_NESTING {
                return Err(ObjectFilterParseError::TooDeep);
            }
            return parts
                .split('+')
                .map(|part| {
                    if part.is_empty() {
                        return Err(ObjectFilterParseError::EmptyCombinePart(spec.to_owned()));
                    }
                    Self::parse_nested(&decode_part(part)?, depth + 1)
                })
                .collect::<Result<_, _>>()
                .map(Self::Combine);
        }
        Self::parse_simple(spec)
    }
}

impl FromStr for ObjectFilter {
    type Err = ObjectFilterParseError;

    fn from_str(spec: &str) -> Result<Self, Self::Err> {
        Self::parse_nested(spec, 0)
    }
}

impl ObjectFilter {
    /// Parses every form except `combine:`.
    fn parse_simple(spec: &str) -> Result<Self, ObjectFilterParseError> {
        if spec == "blob:none" {
            return Ok(Self::BlobNone);
        }
        if let Some(limit) = spec.strip_prefix("blob:limit=") {
            return parse_size(limit)
                .map(Self::BlobLimit)
                .ok_or_else(|| ObjectFilterParseError::BlobLimit(limit.to_owned()));
        }
        if let Some(depth) = spec.strip_prefix("tree:") {
            return parse_number(depth)
                .map(Self::TreeDepth)
                .ok_or_else(|| ObjectFilterParseError::TreeDepth(depth.to_owned()));
        }
        if let Some(kind) = spec.strip_prefix("object:type=") {
            let kind = match kind {
                "blob" => ObjectKind::Blob,
                "tree" => ObjectKind::Tree,
                "commit" => ObjectKind::Commit,
                "tag" => ObjectKind::Tag,
                other => return Err(ObjectFilterParseError::ObjectType(other.to_owned())),
            };
            return Ok(Self::ObjectType(kind));
        }
        if let Some(name) = spec.strip_prefix("sparse:oid=") {
            if name.is_empty() {
                return Err(ObjectFilterParseError::SparseOid);
            }
            return Ok(Self::SparseOid(name.to_owned()));
        }
        Err(ObjectFilterParseError::Unknown(spec.to_owned()))
    }
}

impl ObjectFilter {
    /// Whether a server applying this filter may leave out an object of `kind`, reached from a
    /// commit's root-tree edge when `root_tree` is set.
    ///
    /// Selected tips are always sent, so this concerns only objects they reach. The answer is
    /// an upper bound: a blob's size and an object's depth below the root tree beyond the first
    /// level are not known for an object that was not received, so `blob:limit` admits any
    /// missing blob and `tree:<depth>` with a depth of 1 or more admits any missing object below a
    /// root tree.
    pub(crate) fn may_omit(&self, kind: ObjectKind, root_tree: bool) -> bool {
        match self {
            Self::BlobNone | Self::BlobLimit(_) | Self::SparseOid(_) => kind == ObjectKind::Blob,
            Self::TreeDepth(depth) => match kind {
                ObjectKind::Blob => true,
                ObjectKind::Tree => *depth == 0 || !root_tree,
                ObjectKind::Commit | ObjectKind::Tag => false,
            },
            Self::ObjectType(kept) => kind != *kept,
            Self::Combine(parts) => parts.iter().any(|part| part.may_omit(kind, root_tree)),
        }
    }
}

impl fmt::Display for ObjectFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlobNone => f.write_str("blob:none"),
            Self::BlobLimit(limit) => write!(f, "blob:limit={limit}"),
            Self::TreeDepth(depth) => write!(f, "tree:{depth}"),
            Self::ObjectType(kind) => write!(f, "object:type={}", kind.as_str()),
            Self::SparseOid(name) => write!(f, "sparse:oid={name}"),
            Self::Combine(parts) => {
                f.write_str("combine:")?;
                for (index, part) in parts.iter().enumerate() {
                    if index > 0 {
                        f.write_str("+")?;
                    }
                    for byte in part.to_string().bytes() {
                        if needs_escape(byte) {
                            write!(f, "%{byte:02x}")?;
                        } else {
                            f.write_str(std::str::from_utf8(&[byte]).expect("ASCII byte"))?;
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

/// Characters Git refuses unescaped in a `combine:` part.
const RESERVED: &[u8] = b"~!@#$^&*()[]{};'\"<>?,\\`";

/// Whether to percent-encode a byte of a `combine:` part: a [`RESERVED`] character, the `%` and
/// `+` the encoding itself uses, or anything outside printable ASCII.
fn needs_escape(byte: u8) -> bool {
    !byte.is_ascii_graphic() || byte == b'%' || byte == b'+' || RESERVED.contains(&byte)
}

/// Decodes a `combine:` part as Git does: `%` followed by two hex digits other than `%00` is a
/// byte, and any other `%` is kept as written.
fn decode_part(part: &str) -> Result<String, ObjectFilterParseError> {
    let bytes = part.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let escaped = bytes
            .get(index + 1..index + 3)
            .filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))
            .map(|hex| {
                let hex = std::str::from_utf8(hex).expect("hex digits are ASCII");
                u8::from_str_radix(hex, 16).expect("two hex digits fit in a byte")
            })
            .filter(|&value| value != 0);
        if let (b'%', Some(value)) = (byte, escaped) {
            decoded.push(value);
            index += 3;
        } else if RESERVED.contains(&byte) {
            return Err(ObjectFilterParseError::Unescaped(char::from(byte)));
        } else {
            decoded.push(byte);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| ObjectFilterParseError::Escape(part.to_owned()))
}

/// Parses a number as C's `strtoul` with base 0 does, which is how Git reads filter numbers:
/// leading whitespace and `+` are skipped, then `0x` starts hex, `0` starts octal, and anything
/// else is decimal. The whole of `text` must be the number.
fn parse_number(text: &str) -> Option<u64> {
    let text = text.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let text = text.strip_prefix('+').unwrap_or(text);
    let (digits, radix) =
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (hex, 16)
        } else if text.len() > 1
            && let Some(octal) = text.strip_prefix('0')
        {
            (octal, 8)
        } else {
            (text, 10)
        };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    u64::from_str_radix(digits, radix).ok()
}

fn parse_size(size: &str) -> Option<u64> {
    let (number, scale) = match size.as_bytes().last()?.to_ascii_lowercase() {
        b'k' => (&size[..size.len() - 1], 1 << 10),
        b'm' => (&size[..size.len() - 1], 1 << 20),
        b'g' => (&size[..size.len() - 1], 1 << 30),
        _ => (size, 1),
    };
    parse_number(number)?.checked_mul(scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(spec: &str) -> Result<ObjectFilter, ObjectFilterParseError> {
        spec.parse()
    }

    /// Specs Git 2.56 accepts, and the spec it sends for each.
    #[test]
    fn parses_and_writes_git_specs() {
        for (spec, sent) in [
            ("blob:none", "blob:none"),
            ("blob:limit=0", "blob:limit=0"),
            ("blob:limit=1k", "blob:limit=1024"),
            ("blob:limit=1K", "blob:limit=1024"),
            ("blob:limit=2M", "blob:limit=2097152"),
            ("blob:limit=1g", "blob:limit=1073741824"),
            ("tree:0", "tree:0"),
            ("tree:01", "tree:1"),
            ("tree:0x10", "tree:16"),
            ("blob:limit=010", "blob:limit=8"),
            ("blob:limit=0x10k", "blob:limit=16384"),
            ("blob:limit=0X10", "blob:limit=16"),
            ("blob:limit= 5", "blob:limit=5"),
            ("blob:limit=+5", "blob:limit=5"),
            ("object:type=tree", "object:type=tree"),
            ("sparse:oid=main:x", "sparse:oid=main:x"),
            ("combine:blob:none+tree:0", "combine:blob:none+tree:0"),
            ("combine:blob:none", "combine:blob:none"),
            ("combine:tree%3a1", "combine:tree:1"),
            (
                "combine:tree%3A1+blob:limit=1k",
                "combine:tree:1+blob:limit=1024",
            ),
        ] {
            let filter = parse(spec).unwrap_or_else(|error| panic!("{spec}: {error}"));
            assert_eq!(filter.to_string(), sent, "{spec}");
            assert_eq!(parse(sent), Ok(filter), "{sent} round trip");
        }
    }

    #[test]
    fn rejects_specs_git_rejects() {
        use ObjectFilterParseError as E;
        for (spec, error) in [
            ("foo", E::Unknown("foo".into())),
            ("BLOB:NONE", E::Unknown("BLOB:NONE".into())),
            ("blob:none ", E::Unknown("blob:none ".into())),
            ("blob:limit=x", E::BlobLimit("x".into())),
            ("blob:limit=1kb", E::BlobLimit("1kb".into())),
            ("blob:limit=-1", E::BlobLimit("-1".into())),
            ("blob:limit=08", E::BlobLimit("08".into())),
            ("blob:limit=0x", E::BlobLimit("0x".into())),
            ("tree:08", E::TreeDepth("08".into())),
            (
                "blob:limit=99999999999999999999",
                E::BlobLimit("99999999999999999999".into()),
            ),
            (
                "blob:limit=17179869184g",
                E::BlobLimit("17179869184g".into()),
            ),
            ("tree:", E::TreeDepth(String::new())),
            ("tree:-1", E::TreeDepth("-1".into())),
            ("object:type=TREE", E::ObjectType("TREE".into())),
            ("sparse:oid=", E::SparseOid),
            ("combine:", E::EmptyCombinePart("combine:".into())),
            ("combine:tree:1~", E::Unescaped('~')),
            ("combine:blob:none+tree:0%", E::TreeDepth("0%".into())),
            ("combine:tree%3", E::Unknown("tree%3".into())),
            ("combine:blob:none+tree%ff1", E::Escape("tree%ff1".into())),
        ] {
            assert_eq!(parse(spec), Err(error), "{spec}");
        }
    }

    /// Git sends these, but each has an empty part, so girt refuses them.
    #[test]
    fn rejects_empty_combine_parts() {
        for spec in ["combine:blob:none+", "combine:+tree:0"] {
            assert_eq!(
                parse(spec),
                Err(ObjectFilterParseError::EmptyCombinePart(spec.into()))
            );
        }
    }

    /// Git keeps `%00` and a `%` without two hex digits as written.
    #[test]
    fn keeps_undecodable_escapes() {
        for (spec, name) in [
            ("combine:sparse:oid=a%00b", "a%00b"),
            ("combine:sparse:oid=a%zz", "a%zz"),
            ("combine:sparse:oid=a%41", "aA"),
        ] {
            let filter = parse(spec).unwrap();
            assert_eq!(
                filter,
                ObjectFilter::Combine(vec![ObjectFilter::SparseOid(name.into())])
            );
            assert_eq!(parse(&filter.to_string()), Ok(filter));
        }
    }

    /// Git accepts nested `combine:` specs such as `combine:combine%3Ablob%3Anone`; girt accepts
    /// them up to [`ObjectFilter::MAX_NESTING`] levels and refuses deeper ones without recursing
    /// further.
    #[test]
    fn bounds_combine_nesting() {
        let nested = |levels: usize| {
            let mut spec = "blob:none".to_owned();
            for _ in 0..levels {
                spec = format!("combine:{}", spec.replace('%', "%25").replace(':', "%3a"));
            }
            spec
        };
        let mut filter = parse(&nested(ObjectFilter::MAX_NESTING)).unwrap();
        for _ in 0..ObjectFilter::MAX_NESTING {
            let ObjectFilter::Combine(mut parts) = filter else {
                panic!("expected combine");
            };
            filter = parts.remove(0);
        }
        assert_eq!(filter, ObjectFilter::BlobNone);
        assert_eq!(
            parse(&nested(ObjectFilter::MAX_NESTING + 1)),
            Err(ObjectFilterParseError::TooDeep)
        );
        // Unescaped nesting, as an attacker may send it, stops at the same depth.
        let raw = format!("{}blob:none", "combine:".repeat(8000));
        assert_eq!(parse(&raw), Err(ObjectFilterParseError::TooDeep));
    }

    #[test]
    fn escapes_reserved_characters_in_combine_parts() {
        let filter = ObjectFilter::Combine(vec![
            ObjectFilter::SparseOid("a~b+c%d".into()),
            ObjectFilter::BlobNone,
        ]);
        let spec = filter.to_string();
        assert_eq!(spec, "combine:sparse:oid=a%7eb%2bc%25d+blob:none");
        assert_eq!(parse(&spec), Ok(filter));
    }
}
