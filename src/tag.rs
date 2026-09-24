use crate::{CommitError, ObjectId, Signature};

/// An owned SHA-1 annotated tag object, including its exact original payload.
///
/// A tag object names an object and carries optional tagger metadata and a message. It is
/// independent of a tag reference: creating or storing it does not create `refs/tags/...`.
/// Targets may be blobs, trees, commits, or other tags. No target lookup or peeling occurs.
///
/// [`Self::parse`] preserves accepted bytes, including uppercase IDs, date spelling, absent
/// taggers, extra header lines, and opaque signatures embedded in the message. Names and messages
/// need not be UTF-8. [`Self::new`] validates decoded fields and emits canonical framing;
/// reconstructing parsed fields can therefore change identity. Fields are immutable once stored.
///
/// Supported framing is `object`, `type`, `tag`, an optional `tagger`, then opaque extra lines.
/// Each header ends in LF. A blank line introduces the message; without it, the payload must end
/// after a header newline and the message is empty. Required headers cannot be reordered or
/// repeated. IDs must contain 40 hexadecimal SHA-1 digits. Tagger dates use the same grammar as
/// [`crate::Commit`]. SHA-256 IDs and other date grammars are unsupported.
///
/// This is not full fsck validation. Target existence/type, reference-name rules, and cryptographic
/// signatures are not checked. Memory grows with the input and decoded fields; bound input before
/// parsing or use [`crate::LooseObjects::read_tag`] with a payload limit.
///
/// ```
/// use girt::{ObjectId, ObjectKind, Tag, TagFields};
/// let tag = Tag::new(TagFields {
///     target: ObjectId::for_blob(b"release bytes"),
///     target_kind: ObjectKind::Blob,
///     name: b"v1".to_vec(),
///     tagger: None,
///     extra_headers: vec![],
///     message: b"First release\n".to_vec(),
/// })?;
/// let parsed = Tag::parse(tag.as_bytes())?;
/// assert_eq!(parsed.fields().name, b"v1");
/// assert_eq!(parsed.id(), tag.id());
/// # Ok::<(), girt::TagError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tag {
    fields: TagFields,
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
        Ok(Self { fields, payload })
    }

    /// Copies supported framing into decoded fields while retaining the exact original bytes.
    ///
    /// Empty names, negative dates, and NUL/CR in header values remain inspectable. Call
    /// [`Self::validate`] separately to apply construction rules without discarding these bytes.
    /// Extra lines are opaque, including spaces and repeated unknown keys; no commit-style
    /// continuation decoding is applied to them.
    ///
    /// # Errors
    ///
    /// Returns [`TagError`] for missing, reordered, or repeated known headers, unterminated
    /// headers, invalid SHA-1 IDs, unsupported target types, or unreadable tagger metadata.
    pub fn parse(payload: &[u8]) -> Result<Self, TagError> {
        let (headers, message) = match payload.windows(2).position(|pair| pair == b"\n\n") {
            Some(separator) => (&payload[..separator], &payload[separator + 2..]),
            None => (
                payload
                    .strip_suffix(b"\n")
                    .ok_or(TagError::UnterminatedHeader)?,
                b"".as_slice(),
            ),
        };
        let mut lines = headers.split(|&byte| byte == b'\n').peekable();
        let target_bytes = required_line(lines.next(), b"object ")?;
        let target = std::str::from_utf8(target_bytes)
            .ok()
            .and_then(|value| value.parse().ok())
            .ok_or(TagError::InvalidObjectId)?;
        let target_kind = ObjectKind::parse(required_line(lines.next(), b"type ")?)?;
        let name = required_line(lines.next(), b"tag ")?.to_vec();
        let tagger = if lines
            .peek()
            .is_some_and(|line| line.starts_with(b"tagger "))
        {
            Some(
                Signature::parse(required_line(lines.next(), b"tagger ")?)
                    .map_err(TagError::Tagger)?,
            )
        } else {
            None
        };
        let mut extra_headers = Vec::new();
        for line in lines {
            check_extra_line(line)?;
            extra_headers.push(line.to_vec());
        }
        Ok(Self {
            fields: TagFields {
                target,
                target_kind,
                name,
                tagger,
                extra_headers,
                message: message.to_vec(),
            },
            payload: payload.to_vec(),
        })
    }

    /// Borrows decoded fields; clone them for explicit reconstruction with [`Self::new`].
    pub fn fields(&self) -> &TagFields {
        &self.fields
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
        self.fields.validate()
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
        ObjectId::for_object("tag", &self.payload)
    }
}

/// Editable tag content consumed by [`Tag::new`].
///
/// Lexical details, such as date spelling and an omitted message separator, belong to the
/// original [`Tag`] and are not retained by these fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TagFields {
    /// Target SHA-1 identity; zero IDs and missing targets are not rejected.
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
    /// The target is not exactly 40 hexadecimal SHA-1 digits.
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
        Tag::parse(&payload(b"tagger A <a> 1 +0000\n\nrelease\n"))
            .unwrap()
            .fields()
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
        assert_eq!(Tag::parse(tag.as_bytes()).unwrap().fields(), &fields);
    }

    #[rstest]
    #[case::without_tagger(b"", None)]
    #[case::blank_separator(b"\n", None)]
    #[case::tagger_only(b"tagger A <a> 1 +0000\n", Some(1))]
    #[case::tagger_separator(b"tagger A <a> 1 +0000\n\n", Some(1))]
    fn preserves_empty_message_framing(#[case] tail: &[u8], #[case] seconds: Option<i64>) {
        let input = payload(tail);
        let tag = Tag::parse(&input).unwrap();
        assert_eq!(tag.encode(), input);
        assert_eq!(tag.fields().message, b"");
        assert_eq!(
            tag.fields().tagger.as_ref().map(|person| person.seconds),
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
        let parsed = Tag::parse(tag.as_bytes()).unwrap();
        assert_eq!(parsed.fields().message, message);
        assert_eq!(parsed.encode(), tag.encode());
    }

    #[test]
    fn preserves_opaque_extra_lines() {
        let input =
            payload(b"tagger A <a> 1 +0000\nx first\n continued\nx second\nbare-line\n\nbody");
        let parsed = Tag::parse(&input).unwrap();
        assert_eq!(
            parsed.fields().extra_headers,
            [
                b"x first".to_vec(),
                b" continued".to_vec(),
                b"x second".to_vec(),
                b"bare-line".to_vec()
            ]
        );
        assert_eq!(parsed.encode(), input);
        assert_eq!(Tag::new(parsed.fields().clone()).unwrap().encode(), input);
    }

    #[test]
    fn retains_lexical_details_until_reconstruction() {
        let input = format!(
            "object {}\ntype blob\ntag v1\ntagger A <a> +00042 -0000\n",
            TARGET.to_uppercase()
        );
        let tag = Tag::parse(input.as_bytes()).unwrap();
        assert_eq!(tag.fields().tagger.as_ref().unwrap().seconds, 42);
        assert_eq!(tag.encode(), input.as_bytes());
        assert_eq!(tag.validate(), Ok(()));
        assert_ne!(Tag::new(tag.fields().clone()).unwrap().id(), tag.id());
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::space(b"a b")]
    #[case::nul(b"a\0b")]
    #[case::cr(b"a\rb")]
    fn retains_names_rejected_by_construction(#[case] name: &[u8]) {
        let headers = format!("object {TARGET}\ntype blob\ntag ");
        let input = [headers.as_bytes(), name, b"\n\n"].concat();
        let parsed = Tag::parse(&input).unwrap();
        assert_eq!(parsed.encode(), input);
        assert_eq!(parsed.validate(), Err(TagError::InvalidName));
        assert_eq!(
            Tag::new(parsed.fields().clone()),
            Err(TagError::InvalidName)
        );
    }

    #[test]
    fn accepts_byte_names_without_ref_validation() {
        let mut fields = fields();
        fields.name = b"../v\xff".to_vec();
        let tag = Tag::new(fields).unwrap();
        assert_eq!(Tag::parse(tag.as_bytes()).unwrap(), tag);
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
        let tag = Tag::parse(&input).unwrap();
        assert_eq!(tag.encode(), input);
        assert_eq!(tag.validate(), Err(TagError::InvalidHeader));
    }

    #[rstest]
    #[case::negative("A <a> -1 +0000", CommitError::InvalidDate)]
    #[case::empty(" <> 1 +0000", CommitError::InvalidSignature)]
    fn retains_tagger_rejected_by_validation(#[case] person: &str, #[case] error: CommitError) {
        let input = payload(format!("tagger {person}\n\n").as_bytes());
        let tag = Tag::parse(&input).unwrap();
        assert_eq!(tag.encode(), input);
        assert_eq!(tag.validate(), Err(TagError::Tagger(error)));
        assert_eq!(Tag::new(tag.fields().clone()), Err(TagError::Tagger(error)));
    }

    #[rstest]
    #[case::identity("not an identity", CommitError::InvalidSignature)]
    #[case::overflow("A <a> 9223372036854775808 +0000", CommitError::InvalidDate)]
    #[case::offset("A <a> 1 +2400", CommitError::InvalidDate)]
    fn rejects_unreadable_tagger(#[case] person: &str, #[case] error: CommitError) {
        assert_eq!(
            Tag::parse(&payload(format!("tagger {person}\n\n").as_bytes())),
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
        TagError::RequiredHeader
    )]
    fn rejects_malformed_headers(#[case] input: &[u8], #[case] error: TagError) {
        assert_eq!(Tag::parse(input), Err(error));
    }

    #[rstest]
    #[case::object(b"object other\n\n")]
    #[case::kind(b"type blob\n\n")]
    #[case::name(b"tag other\n\n")]
    #[case::misplaced_tagger(b"x extra\ntagger A <a> 1 +0000\n\n")]
    #[case::repeated_tagger(b"tagger A <a> 1 +0000\ntagger A <a> 1 +0000\n\n")]
    fn rejects_repeated_or_misplaced_headers(#[case] tail: &[u8]) {
        assert_eq!(Tag::parse(&payload(tail)), Err(TagError::InvalidHeader));
    }

    #[test]
    fn matches_independent_literal_identity() {
        let tag = Tag::parse(&payload(b"tagger A <a> 1 +0000\n")).unwrap();
        assert_eq!(
            tag.id().to_string(),
            "1316e0263ce4c0bc7afc85d20286be352811d4cf"
        );
    }
}
