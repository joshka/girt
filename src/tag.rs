use crate::{CommitError, ObjectId, Signature};

/// An owned SHA-1 or SHA-256 annotated tag object, including its exact original payload.
///
/// A tag object names an object and carries optional tagger metadata and a message. It is
/// independent of a tag reference: creating or storing it does not create `refs/tags/...`.
/// Targets may be blobs, trees, commits, or other tags. Use [`crate::Objects::peel`] to resolve
/// them through storage; parsing a tag alone performs no lookup.
///
/// [`Self::parse`] preserves accepted bytes, including uppercase IDs, date spelling, absent
/// taggers, extra header lines, and opaque signatures embedded in the message. Names and messages
/// need not be UTF-8. [`Self::new`] validates decoded fields and emits canonical framing;
/// reconstructing parsed fields can therefore change identity. Fields are immutable once stored.
///
/// Reading requires leading `object`, `type`, and `tag` lines with LF terminators and an exact
/// format-width hexadecimal target ID. Later known headers do not replace those target records.
/// The first physical tagger is available separately through [`Self::tagger`]; malformed dates
/// do not block target decoding. Unknown, repeated, folded and unterminated trailing records
/// remain byte-identical. Without an empty separator line the message is empty.
///
/// This is not full fsck validation. Target existence/type, reference-name rules, and cryptographic
/// signatures are not checked. Memory grows with the input and decoded fields; bound input before
/// parsing or use [`crate::LooseObjects::read_tag`] with a payload limit.
///
/// ```
/// use girt::{ObjectId, ObjectKind, Tag, TagFields};
/// let tag = Tag::new(TagFields {
///     target: ObjectId::for_blob(girt::ObjectFormat::Sha1, b"release bytes"),
///     target_kind: ObjectKind::Blob,
///     name: b"v1".to_vec(),
///     tagger: None,
///     extra_headers: vec![],
///     message: b"First release\n".to_vec(),
/// })?;
/// let parsed = Tag::parse(girt::ObjectFormat::Sha1, tag.as_bytes())?;
/// assert_eq!(parsed.to_fields()?.name, b"v1");
/// assert_eq!(parsed.id(), tag.id());
/// # Ok::<(), girt::TagError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tag {
    target: ObjectId,
    target_kind: ObjectKind,
    payload: Vec<u8>,
}

impl Tag {
    /// Validates owned fields and constructs lowercase IDs, canonical dates, and a blank separator.
    ///
    /// Header order and message bytes are unchanged; no final message newline is added.
    ///
    /// # Errors
    ///
    /// Returns the construction failures described by [`Self::validate`]. Has no external effects.
    /// The target identity determines the tag's format.
    pub fn new(fields: TagFields) -> Result<Self, TagError> {
        fields.validate()?;
        let headers = format!(
            "object {}\ntype {}\ntag ",
            fields.target,
            fields.target_kind.as_str()
        );
        let mut payload = headers.into_bytes();
        payload.extend_from_slice(&fields.name);
        payload.push(b'\n');
        if let Some(tagger) = &fields.tagger {
            tagger.encode(b"tagger", &mut payload);
        }
        for line in &fields.extra_headers {
            payload.extend_from_slice(line);
            payload.push(b'\n');
        }
        payload.push(b'\n');
        payload.extend_from_slice(&fields.message);
        Self::parse(fields.target.format(), &payload)
    }

    /// Decodes mandatory target records while retaining all imported bytes.
    ///
    /// Does not require a tagger or interpret its date. Extra records remain opaque; repeated
    /// target keys do not replace the first three physical records.
    ///
    /// # Errors
    ///
    /// Returns [`TagError`] for missing, reordered or unterminated mandatory records, invalid or
    /// foreign-format IDs, or unknown target types. No object lookup occurs.
    pub fn parse(format: crate::ObjectFormat, payload: &[u8]) -> Result<Self, TagError> {
        let mut physical = payload.split_inclusive(|&byte| byte == b'\n');
        let mut required = || {
            physical
                .next()
                .and_then(|line| line.strip_suffix(b"\n"))
                .ok_or(TagError::UnterminatedHeader)
        };
        let target_bytes = required_line(Some(required()?), b"object ")?;
        if target_bytes.len() != format.digest_len() * 2 {
            return Err(TagError::InvalidObjectId);
        }
        let target = std::str::from_utf8(target_bytes)
            .ok()
            .and_then(|value| value.parse().ok())
            .ok_or(TagError::InvalidObjectId)?;
        let target_kind = ObjectKind::parse(required_line(Some(required()?), b"type ")?)?;
        required_line(Some(required()?), b"tag ")?;
        Ok(Self {
            target,
            target_kind,
            payload: payload.to_vec(),
        })
    }

    /// The format of the target reference.
    pub fn object_format(&self) -> crate::ObjectFormat {
        self.target.format()
    }

    /// Copies editable construction fields, interpreting the first tagger if present.
    ///
    /// # Errors
    ///
    /// Returns [`TagError::Tagger`] for an uninterpretable identity or date. Target access and
    /// peeling do not require this conversion. Reconstructing fields can change the object ID.
    pub fn to_fields(&self) -> Result<TagFields, TagError> {
        let tagger = self
            .tagger()?
            .map(crate::IdentityRef::signature)
            .transpose()
            .map_err(TagError::Tagger)?;
        let extra_headers = self
            .headers()
            .skip(3)
            .filter(|line| !line.starts_with(b"tagger "))
            .map(<[u8]>::to_vec)
            .collect();
        Ok(TagFields {
            target: self.target,
            target_kind: self.target_kind,
            name: self.name().to_vec(),
            tagger,
            extra_headers,
            message: self.message().to_vec(),
        })
    }

    fn headers(&self) -> impl Iterator<Item = &[u8]> {
        self.payload
            .split(|&b| b == b'\n')
            .take_while(|line| !line.is_empty())
    }

    /// Imported tag name bytes, without applying reference-name or construction validation.
    pub fn name(&self) -> &[u8] {
        self.headers()
            .nth(2)
            .expect("parsed third header")
            .strip_prefix(b"tag ")
            .expect("parsed tag name")
    }

    /// Exact message bytes, including any embedded signature armor.
    pub fn message(&self) -> &[u8] {
        self.payload
            .windows(2)
            .position(|p| p == b"\n\n")
            .map_or(&[], |i| &self.payload[i + 2..])
    }

    /// Referenced object identity, without resolving it.
    pub fn target(&self) -> ObjectId {
        self.target
    }

    /// Declared target kind; use object-store peeling to verify it.
    pub fn target_kind(&self) -> ObjectKind {
        self.target_kind
    }

    /// Borrows the first tagger's identity independently of date interpretation.
    ///
    /// # Errors
    ///
    /// Returns [`TagError::Tagger`] for malformed identity delimiters.
    pub fn tagger(&self) -> Result<Option<crate::IdentityRef<'_>>, TagError> {
        let headers = self
            .payload
            .split(|&b| b == b'\n')
            .take_while(|line| !line.is_empty());
        headers
            .filter_map(|line| line.strip_prefix(b"tagger "))
            .next()
            .map(crate::IdentityRef::parse)
            .transpose()
            .map_err(TagError::Tagger)
    }

    /// Checks construction rules without modifying the retained payload.
    ///
    /// # Errors
    ///
    /// Rejects empty names and names containing ASCII whitespace or NUL. This is a header-name
    /// rule, not validation of a tag ref path. Optional taggers follow [`crate::Commit::validate`]
    /// identity/date rules. Extra lines must be nonempty, contain no NUL, CR, or LF, and cannot
    /// begin with a reserved header key (`object`, `type`, `tag`, `tagger`) followed by a space or
    /// end of line. Messages are unrestricted. Absent taggers are valid for this API.
    pub fn validate(&self) -> Result<(), TagError> {
        self.to_fields()?.validate()
    }

    /// Borrows the exact payload without its object header or compression.
    pub fn as_bytes(&self) -> &[u8] {
        &self.payload
    }

    /// Copies the exact payload without validation or normalization.
    pub fn encode(&self) -> Vec<u8> {
        self.payload.clone()
    }

    /// Hashes the canonical tag object header and exact retained payload without copying it.
    pub fn id(&self) -> ObjectId {
        self.object_format()
            .hash_object(crate::ObjectKind::Tag, &self.payload)
    }
}

/// Editable tag content consumed by [`Tag::new`].
///
/// Lexical details, such as date spelling and an omitted message separator, belong to the
/// original [`Tag`] and are not retained by these fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TagFields {
    /// Target identity (which determines the constructed tag's format); zero IDs and missing
    /// targets are not rejected.
    pub target: ObjectId,

    /// Declared target type, without checking the referenced object.
    pub target_kind: ObjectKind,

    /// Tag name bytes, independent of any tag reference path.
    pub name: Vec<u8>,

    /// Optional person and date metadata; this is not a cryptographic signature.
    pub tagger: Option<Signature>,

    /// Opaque header lines in order, excluding their LF terminators.
    ///
    /// Repeated unknown keys and lines starting with spaces are retained verbatim. No semantic
    /// interpretation or unfolding is performed. See [`Tag::validate`] for construction rules.
    pub extra_headers: Vec<Vec<u8>>,

    /// Arbitrary message bytes, including any embedded signature armor and NUL bytes.
    pub message: Vec<u8>,
}

impl TagFields {
    fn validate(&self) -> Result<(), TagError> {
        if self.name.is_empty()
            || self
                .name
                .iter()
                .any(|byte| *byte == 0 || byte.is_ascii_whitespace())
        {
            return Err(TagError::InvalidName);
        }
        if let Some(tagger) = &self.tagger {
            tagger.validate().map_err(TagError::Tagger)?;
        }
        for line in &self.extra_headers {
            check_extra_line(line)?;
            if line.iter().any(|byte| matches!(byte, 0 | b'\r' | b'\n')) {
                return Err(TagError::InvalidHeader);
            }
        }
        Ok(())
    }
}

/// One of Git's four object kinds, also used to declare an annotated tag's target type.
///
/// In a tag this declaration does not prove that the target exists or has the stated type.
/// In a verified [`crate::Object`] it identifies the kind included in the object's identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectKind {
    /// File content.
    Blob,
    /// A directory snapshot.
    Tree,
    /// A recorded snapshot and its history metadata.
    Commit,
    /// An annotated tag; no recursive peeling is performed.
    Tag,
}

impl ObjectKind {
    /// Returns the type's spelling in Git object headers.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
            Self::Commit => "commit",
            Self::Tag => "tag",
        }
    }

    fn parse(bytes: &[u8]) -> Result<Self, TagError> {
        match bytes {
            b"blob" => Ok(Self::Blob),
            b"tree" => Ok(Self::Tree),
            b"commit" => Ok(Self::Commit),
            b"tag" => Ok(Self::Tag),
            _ => Err(TagError::InvalidObjectKind),
        }
    }
}

/// Unsupported tag framing or a construction validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TagError {
    /// A header has no terminating LF and no message separator precedes it.
    #[error("unterminated tag header")]
    UnterminatedHeader,
    /// A required header is missing or reordered.
    #[error("missing or misplaced required tag header")]
    RequiredHeader,
    /// The target does not have the selected hexadecimal format.
    #[error("invalid or unsupported tag target identity")]
    InvalidObjectId,
    /// The declared target type is not blob, tree, commit, or tag.
    #[error("invalid tag target type")]
    InvalidObjectKind,
    /// Construction requires a nonempty name without ASCII whitespace or NUL.
    #[error("invalid tag name")]
    InvalidName,
    /// An extra line is empty, repeats a known header, or fails construction rules.
    #[error("invalid or misplaced extra tag header")]
    InvalidHeader,
    /// Tagger identity or date parsing/validation failed; retains the shared metadata error.
    #[error("invalid tagger metadata: {0}")]
    Tagger(#[source] CommitError),
}

fn required_line<'a>(line: Option<&'a [u8]>, prefix: &[u8]) -> Result<&'a [u8], TagError> {
    line.and_then(|line| line.strip_prefix(prefix))
        .ok_or(TagError::RequiredHeader)
}

fn check_extra_line(line: &[u8]) -> Result<(), TagError> {
    let key = line.split(|&byte| byte == b' ').next().unwrap_or_default();
    if line.is_empty() || matches!(key, b"object" | b"type" | b"tag" | b"tagger") {
        return Err(TagError::InvalidHeader);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const TARGET: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

    fn payload(tail: &[u8]) -> Vec<u8> {
        let headers = format!("object {TARGET}\ntype blob\ntag v1\n");
        [headers.as_bytes(), tail].concat()
    }

    fn fields() -> TagFields {
        Tag::parse(
            crate::ObjectFormat::Sha1,
            &payload(b"tagger A <a> 1 +0000\n\nrelease\n"),
        )
        .unwrap()
        .to_fields()
        .unwrap()
        .clone()
    }

    #[rstest]
    #[case::blob(ObjectKind::Blob, "blob")]
    #[case::tree(ObjectKind::Tree, "tree")]
    #[case::commit(ObjectKind::Commit, "commit")]
    #[case::tag(ObjectKind::Tag, "tag")]
    fn constructs_every_target_kind(#[case] kind: ObjectKind, #[case] spelling: &str) {
        let mut fields = fields();
        fields.target_kind = kind;
        let tag = Tag::new(fields.clone()).unwrap();
        let expected =
            format!("object {TARGET}\ntype {spelling}\ntag v1\ntagger A <a> 1 +0000\n\nrelease\n");
        assert_eq!(tag.as_bytes(), expected.as_bytes());
        assert_eq!(
            Tag::parse(crate::ObjectFormat::Sha1, tag.as_bytes())
                .unwrap()
                .to_fields()
                .unwrap(),
            fields
        );
    }

    #[rstest]
    #[case::without_tagger(b"", None)]
    #[case::blank_separator(b"\n", None)]
    #[case::tagger_only(b"tagger A <a> 1 +0000\n", Some(1))]
    #[case::tagger_separator(b"tagger A <a> 1 +0000\n\n", Some(1))]
    fn preserves_empty_message_framing(#[case] tail: &[u8], #[case] seconds: Option<i64>) {
        let input = payload(tail);
        let tag = Tag::parse(crate::ObjectFormat::Sha1, &input).unwrap();
        assert_eq!(tag.encode(), input);
        assert_eq!(tag.to_fields().unwrap().message, b"");
        assert_eq!(
            tag.to_fields()
                .unwrap()
                .tagger
                .as_ref()
                .map(|person| person.seconds),
            seconds
        );
        assert_eq!(tag.validate(), Ok(()));
    }

    #[rstest]
    #[case::binary(b"\xff\0\r\n\ntag fake")]
    #[case::pgp(b"release\n-----BEGIN PGP SIGNATURE-----\nopaque\n-----END PGP SIGNATURE-----\n")]
    #[case::ssh(b"-----BEGIN SSH SIGNATURE-----\nopaque\n-----END SSH SIGNATURE-----")]
    #[case::empty(b"")]
    fn preserves_opaque_message(#[case] message: &[u8]) {
        let mut fields = fields();
        fields.message = message.to_vec();
        let tag = Tag::new(fields).unwrap();
        let parsed = Tag::parse(crate::ObjectFormat::Sha1, tag.as_bytes()).unwrap();
        assert_eq!(parsed.to_fields().unwrap().message, message);
        assert_eq!(parsed.encode(), tag.encode());
    }

    #[test]
    fn preserves_opaque_extra_lines() {
        let input =
            payload(b"tagger A <a> 1 +0000\nx first\n continued\nx second\nbare-line\n\nbody");
        let parsed = Tag::parse(crate::ObjectFormat::Sha1, &input).unwrap();
        assert_eq!(
            parsed.to_fields().unwrap().extra_headers,
            [
                b"x first".to_vec(),
                b" continued".to_vec(),
                b"x second".to_vec(),
                b"bare-line".to_vec()
            ]
        );
        assert_eq!(parsed.encode(), input);
        assert_eq!(
            Tag::new(parsed.to_fields().unwrap()).unwrap().encode(),
            input
        );
    }

    #[test]
    fn retains_lexical_details_until_reconstruction() {
        let input = format!(
            "object {}\ntype blob\ntag v1\ntagger A <a> +00042 -0000\n",
            TARGET.to_uppercase()
        );
        let tag = Tag::parse(crate::ObjectFormat::Sha1, input.as_bytes()).unwrap();
        assert_eq!(
            tag.to_fields().unwrap().tagger.as_ref().unwrap().seconds,
            42
        );
        assert_eq!(tag.encode(), input.as_bytes());
        assert_eq!(tag.validate(), Ok(()));
        assert_ne!(Tag::new(tag.to_fields().unwrap()).unwrap().id(), tag.id());
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::space(b"a b")]
    #[case::nul(b"a\0b")]
    #[case::cr(b"a\rb")]
    fn retains_names_rejected_by_construction(#[case] name: &[u8]) {
        let headers = format!("object {TARGET}\ntype blob\ntag ");
        let input = [headers.as_bytes(), name, b"\n\n"].concat();
        let parsed = Tag::parse(crate::ObjectFormat::Sha1, &input).unwrap();
        assert_eq!(parsed.encode(), input);
        assert_eq!(parsed.validate(), Err(TagError::InvalidName));
        assert_eq!(
            Tag::new(parsed.to_fields().unwrap()),
            Err(TagError::InvalidName)
        );
    }

    #[test]
    fn accepts_byte_names_without_ref_validation() {
        let mut fields = fields();
        fields.name = b"../v\xff".to_vec();
        let tag = Tag::new(fields).unwrap();
        assert_eq!(
            Tag::parse(crate::ObjectFormat::Sha1, tag.as_bytes()).unwrap(),
            tag
        );
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::lf(b"x\ny")]
    #[case::cr(b"x\ry")]
    #[case::nul(b"x\0y")]
    #[case::object(b"object other")]
    #[case::kind(b"type blob")]
    #[case::name(b"tag other")]
    #[case::tagger(b"tagger A <a> 1 +0000")]
    fn rejects_invalid_constructed_extra_lines(#[case] line: &[u8]) {
        let mut fields = fields();
        fields.extra_headers.push(line.to_vec());
        assert_eq!(Tag::new(fields), Err(TagError::InvalidHeader));
    }

    #[test]
    fn retains_extra_values_rejected_by_validation() {
        let input = payload(b"x \0\r\n\n");
        let tag = Tag::parse(crate::ObjectFormat::Sha1, &input).unwrap();
        assert_eq!(tag.encode(), input);
        assert_eq!(tag.validate(), Err(TagError::InvalidHeader));
    }

    #[test]
    fn constructs_negative_tagger_seconds() {
        let input = payload(b"tagger A <a> -1 +0000\n\n");
        let tag = Tag::parse(crate::ObjectFormat::Sha1, &input).unwrap();
        assert_eq!(
            Tag::new(tag.to_fields().unwrap()).unwrap().as_bytes(),
            input
        );
    }

    #[rstest]
    #[case::empty(" <> 1 +0000", CommitError::InvalidSignature)]
    fn retains_tagger_rejected_by_validation(#[case] person: &str, #[case] error: CommitError) {
        let input = payload(format!("tagger {person}\n\n").as_bytes());
        let tag = Tag::parse(crate::ObjectFormat::Sha1, &input).unwrap();
        assert_eq!(tag.encode(), input);
        assert_eq!(tag.validate(), Err(TagError::Tagger(error)));
        assert_eq!(
            Tag::new(tag.to_fields().unwrap()),
            Err(TagError::Tagger(error))
        );
    }

    #[rstest]
    #[case::identity("not an identity", CommitError::InvalidSignature)]
    #[case::overflow("A <a> 9223372036854775808 +0000", CommitError::InvalidDate)]
    fn rejects_unreadable_tagger(#[case] person: &str, #[case] error: CommitError) {
        assert_eq!(
            Tag::parse(
                crate::ObjectFormat::Sha1,
                &payload(format!("tagger {person}\n\n").as_bytes())
            )
            .unwrap()
            .to_fields(),
            Err(TagError::Tagger(error))
        );
    }

    #[rstest]
    #[case::empty(b"", TagError::UnterminatedHeader)]
    #[case::truncated(b"object abc", TagError::UnterminatedHeader)]
    #[case::missing(b"\n\n", TagError::RequiredHeader)]
    #[case::reordered(b"type blob\nobject abc\n\n", TagError::RequiredHeader)]
    #[case::short(b"object abc\n\n", TagError::InvalidObjectId)]
    #[case::sha256(
        b"object aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\n",
        TagError::InvalidObjectId
    )]
    #[case::nonhex(
        b"object gggggggggggggggggggggggggggggggggggggggg\n\n",
        TagError::InvalidObjectId
    )]
    #[case::missing_kind(
        b"object aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\n",
        TagError::RequiredHeader
    )]
    #[case::bad_kind(
        b"object aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\ntype unknown\ntag v1\n",
        TagError::InvalidObjectKind
    )]
    #[case::missing_name(
        b"object aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\ntype tag\n",
        TagError::UnterminatedHeader
    )]
    fn rejects_malformed_headers(#[case] input: &[u8], #[case] error: TagError) {
        assert_eq!(Tag::parse(crate::ObjectFormat::Sha1, input), Err(error));
    }

    #[rstest]
    #[case::object(b"object other\n\n")]
    #[case::kind(b"type blob\n\n")]
    #[case::name(b"tag other\n\n")]
    #[case::misplaced_tagger(b"x extra\ntagger A <a> 1 +0000\n\n")]
    #[case::repeated_tagger(b"tagger A <a> 1 +0000\ntagger A <a> 1 +0000\n\n")]
    fn retains_repeated_or_misplaced_headers(#[case] tail: &[u8]) {
        let input = payload(tail);
        let tag = Tag::parse(crate::ObjectFormat::Sha1, &input).unwrap();
        assert_eq!(tag.encode(), input);
        assert_eq!(tag.target_kind(), ObjectKind::Blob);
    }

    #[test]
    fn matches_independent_literal_identity() {
        let tag = Tag::parse(
            crate::ObjectFormat::Sha1,
            &payload(b"tagger A <a> 1 +0000\n"),
        )
        .unwrap();
        assert_eq!(
            tag.id().to_string(),
            "1316e0263ce4c0bc7afc85d20286be352811d4cf"
        );
    }
}

#[cfg(test)]
mod format_boundary_tests {
    use super::*;

    #[test]
    fn constructs_sha256_target() {
        let result = Tag::new(TagFields {
            target: ObjectId::Sha256([1; 32]),
            target_kind: ObjectKind::Blob,
            name: b"v1".to_vec(),
            tagger: None,
            extra_headers: vec![],
            message: vec![],
        });
        assert_eq!(result.unwrap().object_format(), crate::ObjectFormat::Sha256);
    }
}

#[cfg(test)]
mod dual_format_tests {
    use rstest::rstest;

    use super::*;
    use crate::ObjectFormat;

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1, ObjectFormat::Sha256)]
    #[case::sha256(ObjectFormat::Sha256, ObjectFormat::Sha1)]
    fn parsing_rejects_foreign_target(#[case] format: ObjectFormat, #[case] foreign: ObjectFormat) {
        let bytes = format!("object {}\ntype blob\ntag v1\n\n", ObjectId::null(foreign));
        assert_eq!(
            Tag::parse(format, bytes.as_bytes()),
            Err(TagError::InvalidObjectId)
        );
    }
}
