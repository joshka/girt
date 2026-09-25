mod identity;
mod payload;
pub use identity::{IdentityDate, IdentityRef};
pub use payload::{CommitHeaderRef, CommitPayload};

use crate::ObjectId;

/// An owned SHA-1 or SHA-256 commit describing a tree, ordered parents, people, and a message.
///
/// [`Self::parse`] retains the original payload as well as decoded graph records. [`Self::encode`]
/// and [`Self::id`] therefore preserve every accepted byte, including hexadecimal case, date
/// spelling, negative-zero offsets, unknown headers, and message bytes. No UTF-8 conversion is
/// performed on names, email addresses, header values, or messages. Fields are immutable after
/// construction.
///
/// [`Self::new`] validates caller-supplied fields and emits canonical framing. [`Self::validate`]
/// checks the same field rules on a parsed object without modifying it. Reconstructing parsed
/// fields with `new` can change identity even when validation succeeds: representation details such
/// as date spelling belong to the original payload, not the decoded fields.
///
/// Reading requires a leading tree record followed by contiguous parent records, with exact
/// format-width hexadecimal IDs and LF terminators. Later physical author/committer records are
/// interpreted independently; the last occurrence wins, and continuations do not extend people.
/// Missing people, malformed dates, opaque legacy lines, and absent message separators remain
/// readable. A trailing parent prefix shorter than a complete format-width record is opaque
/// metadata, matching Git traversal. Later tree/parent records do not replace graph records. Use
/// [`Self::author`] and [`Self::committer`] for explicit absence and interpretation errors, or
/// [`Self::to_fields`] to request editable construction fields with both people and dates.
///
/// This is not full `git fsck` validation. References (including zero and duplicate parent IDs) are
/// not resolved, messages are unrestricted, and extra headers are opaque even when named `gpgsig`
/// or `mergetag`. No signature verification, history traversal, refs, or storage discovery occurs.
/// Memory grows with the payload and decoded fields; bound untrusted input before parsing, or use
/// [`crate::LooseObjects::read_commit`] with a payload limit.
///
/// ```
/// use girt::{Commit, CommitFields, Signature, Tree};
/// let person = Signature {
///     name: b"A. Writer".to_vec(),
///     email: b"writer@example.com".to_vec(),
///     seconds: 1_700_000_000,
///     offset_minutes: -420,
/// };
/// let commit = Commit::new(CommitFields {
///     tree: Tree::new(girt::ObjectFormat::Sha1, vec![])?.id(),
///     parents: vec![],
///     author: person.clone(),
///     committer: person,
///     extra_headers: vec![],
///     message: b"Initial snapshot\n".to_vec(),
/// })?;
/// let parsed = Commit::parse(girt::ObjectFormat::Sha1, commit.as_bytes())?;
/// assert_eq!(parsed.to_fields()?.message, b"Initial snapshot\n");
/// assert_eq!(parsed.id(), commit.id());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Commit {
    tree: ObjectId,
    parents: Vec<ObjectId>,
    payload: Vec<u8>,
}

impl Commit {
    /// Validates owned fields and encodes a new commit with lowercase IDs and decimal seconds.
    ///
    /// Parent and extra-header order is preserved. Zero offset encodes as `+0000`. Every newline
    /// in an extra-header value receives one continuation space. Messages are copied unchanged;
    /// neither a final newline nor an encoding header is added.
    ///
    /// # Errors
    ///
    /// Returns the field errors described by [`Self::validate`]. No filesystem operations occur.
    /// Mixed tree/parent formats return [`CommitError::ObjectFormat`].
    pub fn new(fields: CommitFields) -> Result<Self, CommitError> {
        fields.validate()?;
        let mut payload = format!("tree {}\n", fields.tree).into_bytes();
        for parent in &fields.parents {
            payload.extend_from_slice(format!("parent {parent}\n").as_bytes());
        }
        fields.author.encode(b"author", &mut payload);
        fields.committer.encode(b"committer", &mut payload);
        for header in &fields.extra_headers {
            payload.extend_from_slice(&header.name);
            payload.push(b' ');
            for (index, line) in header.value.split(|&byte| byte == b'\n').enumerate() {
                if index != 0 {
                    payload.extend_from_slice(b"\n ");
                }
                payload.extend_from_slice(line);
            }
            payload.push(b'\n');
        }
        payload.push(b'\n');
        payload.extend_from_slice(&fields.message);
        Ok(Self {
            tree: fields.tree,
            parents: fields.parents,
            payload,
        })
    }

    /// Decodes graph records and retains the exact payload without requiring valid metadata.
    ///
    /// [`Self::author`], [`Self::committer`] and [`Self::to_fields`] interpret metadata separately.
    /// Canonical construction and date spelling are not prerequisites for graph traversal.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError`] for a missing or unterminated leading tree/parent record, invalid
    /// or foreign-format IDs, or a payload ending immediately after those graph records.
    pub fn parse(format: crate::ObjectFormat, payload: &[u8]) -> Result<Self, CommitError> {
        // Git graph traversal consumes the leading tree and contiguous parent records only.
        let mut lines = payload.split_inclusive(|&byte| byte == b'\n').peekable();
        let first = lines.next().ok_or(CommitError::RequiredHeader)?;
        let tree = parse_id(format, required_line(first.strip_suffix(b"\n"), b"tree ")?)?;
        let mut parents = Vec::new();
        // A short trailing parent-like fragment is metadata, not an incomplete graph edge.
        let mut remaining = payload.len() - first.len();
        while remaining >= format.digest_len() * 2 + 8
            && lines
                .peek()
                .is_some_and(|line| line.starts_with(b"parent "))
        {
            let line = lines.next().expect("peeked parent");
            remaining -= line.len();
            parents.push(parse_id(
                format,
                required_line(line.strip_suffix(b"\n"), b"parent ")?,
            )?);
        }
        if lines.peek().is_none() {
            return Err(CommitError::RequiredHeader);
        }
        Ok(Self {
            tree,
            parents,
            payload: payload.to_vec(),
        })
    }

    /// The format of the tree and parent references.
    pub fn object_format(&self) -> crate::ObjectFormat {
        self.tree.format()
    }

    /// Root tree identity from the first physical header line.
    pub fn tree(&self) -> ObjectId {
        self.tree
    }

    /// Ordered parents from complete contiguous records immediately following the tree.
    /// A short trailing parent-like fragment is not a graph edge.
    pub fn parents(&self) -> &[ObjectId] {
        &self.parents
    }

    /// Interprets the last physical author header, independently of its date.
    ///
    /// Returns `None` for an absent header. A malformed identity returns an error; date
    /// interpretation is available separately through [`IdentityRef::date`].
    ///
    /// # Errors
    ///
    /// Returns [`CommitError::InvalidSignature`] for missing identity delimiters.
    pub fn author(&self) -> Result<Option<IdentityRef<'_>>, CommitError> {
        self.person(b"author ")
    }

    /// Interprets the last physical committer header, independently of its date.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError::InvalidSignature`] for missing identity delimiters.
    pub fn committer(&self) -> Result<Option<IdentityRef<'_>>, CommitError> {
        self.person(b"committer ")
    }

    fn person(&self, prefix: &[u8]) -> Result<Option<IdentityRef<'_>>, CommitError> {
        self.header_bytes()
            .split(|&b| b == b'\n')
            .filter_map(|line| line.strip_prefix(prefix))
            .next_back()
            .map(IdentityRef::parse)
            .transpose()
    }

    fn header_bytes(&self) -> &[u8] {
        let end = self
            .payload
            .windows(2)
            .position(|p| p == b"\n\n")
            .unwrap_or(self.payload.len());
        &self.payload[..end]
    }

    /// Borrows message bytes after the first empty line, or an empty slice if absent.
    pub fn message(&self) -> &[u8] {
        self.payload
            .windows(2)
            .position(|p| p == b"\n\n")
            .map_or(&[], |end| &self.payload[end + 2..])
    }

    /// Builds editable construction fields from the imported object.
    ///
    /// Graph access and identity inspection do not require this conversion. Repeated known
    /// headers use the decoded selection policy; reconstruction can change the object identity.
    ///
    /// # Errors
    ///
    /// Requires both people and interpretable dates. Opaque legacy header framing which cannot
    /// be represented by [`CommitHeader`] returns [`CommitError::InvalidHeader`].
    pub fn to_fields(&self) -> Result<CommitFields, CommitError> {
        let author = self
            .author()?
            .ok_or(CommitError::RequiredHeader)?
            .signature()?;
        let committer = self
            .committer()?
            .ok_or(CommitError::RequiredHeader)?
            .signature()?;
        let mut extra_headers: Vec<CommitHeader> = Vec::new();
        let mut continue_extra = false;
        for line in self.header_bytes().split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            if let Some(value) = line.strip_prefix(b" ") {
                if let Some(header) = extra_headers.last_mut().filter(|_| continue_extra) {
                    header.value.push(b'\n');
                    header.value.extend_from_slice(value);
                }
                continue;
            }
            let space = line
                .iter()
                .position(|&b| b == b' ')
                .ok_or(CommitError::InvalidHeader)?;
            let name = &line[..space];
            if matches!(name, b"tree" | b"parent" | b"author" | b"committer") {
                continue_extra = false;
                continue;
            }
            continue_extra = true;
            extra_headers.push(CommitHeader {
                name: name.to_vec(),
                value: line[space + 1..].to_vec(),
            });
        }
        Ok(CommitFields {
            tree: self.tree,
            parents: self.parents.clone(),
            author,
            committer,
            extra_headers,
            message: self.message().to_vec(),
        })
    }

    /// Checks construction rules without repairing or discarding the original payload.
    ///
    /// # Errors
    ///
    /// Rejects empty names/emails, NUL, CR, LF, `<` or `>` in either identity component, and
    /// leading/trailing ASCII whitespace. Seconds may span the full `i64` range; offset minutes
    /// must be in `-1439..=1439`. Extra-header names must be nonempty printable ASCII without
    /// spaces and cannot be `tree`, `parent`, `author`, or `committer`; values cannot contain
    /// NUL or CR. No email syntax, message encoding, reference existence, or signature validity
    /// is checked.
    pub fn validate(&self) -> Result<(), CommitError> {
        self.to_fields()?.validate()
    }

    /// Borrows the exact payload, excluding the object header and compression.
    pub fn as_bytes(&self) -> &[u8] {
        &self.payload
    }

    /// Copies the exact payload into an owned buffer, without normalization or validation.
    pub fn encode(&self) -> Vec<u8> {
        self.payload.clone()
    }

    /// Hashes the canonical commit object header and exact retained payload without copying it.
    pub fn id(&self) -> ObjectId {
        self.object_format()
            .hash_object(crate::ObjectKind::Commit, &self.payload)
    }
}

/// Decoded commit content, editable before passing ownership to [`Commit::new`].
///
/// This representation does not retain lexical details such as uppercase IDs or negative-zero
/// offsets. Use the original [`Commit`] to preserve an existing object's bytes and identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitFields {
    /// Referenced root tree; existence and type are not checked.
    pub tree: ObjectId,

    /// Parent commit IDs in their original order; empty for a root commit.
    pub parents: Vec<ObjectId>,

    /// Person and time responsible for the original change.
    pub author: Signature,

    /// Person and time responsible for recording this commit.
    pub committer: Signature,

    /// Opaque additional headers in order, including repeated names and multiline values.
    pub extra_headers: Vec<CommitHeader>,

    /// Arbitrary bytes after the header/message separator, including NUL and non-UTF-8 bytes.
    pub message: Vec<u8>,
}

impl CommitFields {
    fn validate(&self) -> Result<(), CommitError> {
        for parent in &self.parents {
            parent.require_format(self.tree.format())?;
        }
        self.author.validate()?;
        self.committer.validate()?;
        for header in &self.extra_headers {
            validate_header_name(&header.name)?;
            if header.value.contains(&0) || header.value.contains(&b'\r') {
                return Err(CommitError::InvalidHeader);
            }
        }
        Ok(())
    }
}

/// A Git author, committer, or tagger identity and date, independent of text encoding.
///
/// This is identity metadata, not a cryptographic signature. Fields are unchecked until used by
/// [`Commit::new`] or [`crate::Tag::new`]; see [`Commit::validate`] for construction rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Signature {
    /// Person's name bytes, excluding framing whitespace before the opening email delimiter.
    /// Parsed names may contain a closing angle bracket; construction rejects angle brackets.
    pub name: Vec<u8>,

    /// Email bytes between the first opening and following closing angle brackets.
    /// Parsed email may contain another opening bracket; construction rejects angle brackets.
    pub email: Vec<u8>,

    /// Signed seconds since the Unix epoch, including dates before it, independent of the offset.
    pub seconds: i64,

    /// Signed minutes east of UTC. Parsing maps both `+0000` and `-0000` to zero.
    pub offset_minutes: i16,
}

impl Signature {
    /// Copies identity bytes and interprets a numeric date through [`IdentityRef::date`].
    ///
    /// The first `<` and following `>` delimit email bytes. ASCII whitespace immediately before
    /// `<` is framing; leading name whitespace and embedded angle brackets remain inspectable.
    /// ASCII whitespace between `>` and seconds is skipped. The original representation belongs
    /// to the caller; use [`CommitPayload`] to retain it independently of interpretation success.
    /// Parsing does not enforce the stricter identity rules of [`Commit::new`].
    ///
    /// # Errors
    ///
    /// Returns identity delimiter errors or date interpretation errors from [`IdentityRef`].
    /// No fallback date is invented. Use [`IdentityRef::parse`] to inspect name/email when the date
    /// is absent or malformed.
    pub fn parse(bytes: &[u8]) -> Result<Self, CommitError> {
        IdentityRef::parse(bytes)?.signature()
    }

    pub(crate) fn validate(&self) -> Result<(), CommitError> {
        for component in [&self.name, &self.email] {
            if component.is_empty()
                || component
                    .iter()
                    .any(|byte| matches!(byte, 0 | b'\r' | b'\n' | b'<' | b'>'))
                || component.first().is_some_and(u8::is_ascii_whitespace)
                || component.last().is_some_and(u8::is_ascii_whitespace)
            {
                return Err(CommitError::InvalidSignature);
            }
        }
        if !(-1439..=1439).contains(&self.offset_minutes) {
            return Err(CommitError::InvalidDate);
        }
        Ok(())
    }

    pub(crate) fn encode(&self, key: &[u8], output: &mut Vec<u8>) {
        output.extend_from_slice(key);
        output.push(b' ');
        output.extend_from_slice(&self.name);
        output.extend_from_slice(b" <");
        output.extend_from_slice(&self.email);
        let offset = self.offset_minutes.abs(); // Validated before encoding; cannot be i16::MIN.
        let sign = if self.offset_minutes < 0 { '-' } else { '+' };
        let date = format!(
            "> {} {sign}{:02}{:02}\n",
            self.seconds,
            offset / 60,
            offset % 60
        );
        output.extend_from_slice(date.as_bytes());
    }
}

/// An opaque extra commit header, including an optional multiline value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitHeader {
    /// Nonempty printable ASCII key without spaces; required commit keys are reserved.
    pub name: Vec<u8>,

    /// Value bytes with exactly one framing space removed from each continuation line.
    /// Embedded LF denotes continuation; an empty final line is represented by a trailing LF.
    /// Additional spaces are content and remain unchanged. Construction rejects NUL and CR.
    pub value: Vec<u8>,
}

/// A commit lies outside the supported grammar or fails construction validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CommitError {
    /// A parent identity differs from the commit tree's format.
    #[error(transparent)]
    ObjectFormat(#[from] crate::ObjectFormatError),
    /// The blank line separating headers from message is missing.
    #[error("missing commit header/message separator")]
    MissingSeparator,
    /// A required graph record or requested construction identity is absent or misplaced.
    #[error("missing or misplaced required commit header")]
    RequiredHeader,
    /// A tree or parent identity does not have the selected hexadecimal format.
    #[error("invalid or unsupported commit object identity")]
    InvalidObjectId,
    /// Identity framing is unreadable or name/email fails construction rules.
    #[error("invalid commit identity")]
    InvalidSignature,
    /// A date is unreadable or outside the supported seconds/offset range.
    #[error("invalid or unsupported commit date")]
    InvalidDate,
    /// An extra header has invalid framing, a reserved key, or a disallowed value.
    #[error("invalid or misplaced extra commit header")]
    InvalidHeader,
}

fn required_line<'a>(line: Option<&'a [u8]>, prefix: &[u8]) -> Result<&'a [u8], CommitError> {
    line.and_then(|line| line.strip_prefix(prefix))
        .ok_or(CommitError::RequiredHeader)
}

fn parse_id(format: crate::ObjectFormat, value: &[u8]) -> Result<ObjectId, CommitError> {
    if value.len() != format.digest_len() * 2 {
        return Err(CommitError::InvalidObjectId);
    }
    std::str::from_utf8(value)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or(CommitError::InvalidObjectId)
}

fn validate_header_name(name: &[u8]) -> Result<(), CommitError> {
    if name.is_empty()
        || !name.iter().all(|byte| (b'!'..=b'~').contains(byte))
        || matches!(name, b"tree" | b"parent" | b"author" | b"committer")
    {
        return Err(CommitError::InvalidHeader);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    const PERSON: &str = "A <a@example.com> 1700000000 +0530";

    fn payload(author: &str, extra: &[u8], message: &[u8]) -> Vec<u8> {
        [
            format!("tree {TREE}\nauthor {author}\ncommitter {PERSON}\n").as_bytes(),
            extra,
            b"\n",
            message,
        ]
        .concat()
    }

    fn fields() -> CommitFields {
        Commit::parse(
            crate::ObjectFormat::Sha1,
            &payload(PERSON, b"", b"Initial\n"),
        )
        .unwrap()
        .to_fields()
        .unwrap()
        .clone()
    }

    #[test]
    fn constructs_root_with_literal_payload_and_identity() {
        let commit = Commit::new(fields()).unwrap();
        assert_eq!(commit.encode(), payload(PERSON, b"", b"Initial\n"));
        assert_eq!(commit.to_fields().unwrap().parents, []);
        assert_eq!(commit.validate(), Ok(()));
        assert_eq!(commit.to_fields().unwrap().author.offset_minutes, 330);
        assert_eq!(
            commit.id().to_string(),
            "454b0eab56d2228f210a70360ab4d915f87b79c9"
        );
    }

    #[test]
    fn preserves_ordered_merge_parents_including_duplicates() {
        let mut fields = fields();
        fields.parents = vec![
            ObjectId::Sha1([2; 20]),
            ObjectId::Sha1([1; 20]),
            ObjectId::Sha1([2; 20]),
        ];
        let commit = Commit::new(fields.clone()).unwrap();
        let expected = format!(
            "tree {TREE}\nparent {}\nparent {}\nparent {}\nauthor {PERSON}\ncommitter {PERSON}\n\nInitial\n",
            "02".repeat(20),
            "01".repeat(20),
            "02".repeat(20)
        );
        assert_eq!(commit.as_bytes(), expected.as_bytes());
        assert_eq!(
            Commit::parse(crate::ObjectFormat::Sha1, commit.as_bytes())
                .unwrap()
                .to_fields()
                .unwrap(),
            fields
        );
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::no_final_newline(b"message")]
    #[case::binary(b"\xff\0\r\n\nauthor fake\n")]
    fn preserves_message_bytes(#[case] message: &[u8]) {
        let input = payload(PERSON, b"", message);
        let commit = Commit::parse(crate::ObjectFormat::Sha1, &input).unwrap();
        assert_eq!(commit.to_fields().unwrap().message, message);
        assert_eq!(commit.encode(), input);
        assert_eq!(
            Commit::new(commit.to_fields().unwrap()).unwrap().encode(),
            input
        );
    }

    #[test]
    fn preserves_unknown_repeated_and_multiline_headers() {
        let extra = b"encoding ISO-8859-1\nx-other \xff\ngpgsig first\n \n  indented\n last\n \nx-other again\n";
        let input = payload(PERSON, extra, b"body");
        let commit = Commit::parse(crate::ObjectFormat::Sha1, &input).unwrap();
        assert_eq!(
            commit.to_fields().unwrap().extra_headers,
            vec![
                CommitHeader {
                    name: b"encoding".to_vec(),
                    value: b"ISO-8859-1".to_vec()
                },
                CommitHeader {
                    name: b"x-other".to_vec(),
                    value: b"\xff".to_vec()
                },
                CommitHeader {
                    name: b"gpgsig".to_vec(),
                    value: b"first\n\n indented\nlast\n".to_vec()
                },
                CommitHeader {
                    name: b"x-other".to_vec(),
                    value: b"again".to_vec()
                },
            ]
        );
        assert_eq!(commit.encode(), input);
        assert_eq!(
            Commit::new(commit.to_fields().unwrap()).unwrap().encode(),
            input
        );
    }

    #[test]
    fn retains_lexical_details_until_explicit_reconstruction() {
        let input = format!(
            "tree {}\nauthor A <a@example.com> +00042 -0000\ncommitter {PERSON}\n\n",
            TREE.to_uppercase()
        );
        let parsed = Commit::parse(crate::ObjectFormat::Sha1, input.as_bytes()).unwrap();
        assert_eq!(parsed.to_fields().unwrap().author.seconds, 42);
        assert_eq!(parsed.to_fields().unwrap().author.offset_minutes, 0);
        assert_eq!(parsed.validate(), Ok(()));
        assert_eq!(parsed.as_bytes(), input.as_bytes());
        assert_ne!(
            Commit::new(parsed.to_fields().unwrap()).unwrap().id(),
            parsed.id()
        );
    }

    #[rstest]
    #[case::empty_name(" <a@example.com> 1 +0000", CommitError::InvalidSignature)]
    #[case::empty_email("A <> 1 +0000", CommitError::InvalidSignature)]
    #[case::nul("A\0 <a@example.com> 1 +0000", CommitError::InvalidSignature)]
    #[case::cr("A\rB <a@example.com> 1 +0000", CommitError::InvalidSignature)]
    #[case::leading_space(" A <a@example.com> 1 +0000", CommitError::InvalidSignature)]
    fn preserves_readable_fields_that_fail_validation(
        #[case] author: &str,
        #[case] error: CommitError,
    ) {
        let input = payload(author, b"", b"");
        let parsed = Commit::parse(crate::ObjectFormat::Sha1, &input).unwrap();
        assert_eq!(parsed.validate(), Err(error));
        assert_eq!(Commit::new(parsed.to_fields().unwrap()), Err(error));
        assert_eq!(parsed.encode(), input);
    }

    #[rstest]
    #[case::negative_offset("A <a@example.com> 0 -1200", 0, -720)]
    #[case::max_seconds("A <a@example.com> 9223372036854775807 +2359", i64::MAX, 1439)]
    #[case::min_seconds("A <a@example.com> -9223372036854775808 -2359", i64::MIN, -1439)]
    fn parses_date_boundaries(#[case] author: &str, #[case] seconds: i64, #[case] offset: i16) {
        let parsed = Commit::parse(crate::ObjectFormat::Sha1, &payload(author, b"", b"")).unwrap();
        assert_eq!(parsed.to_fields().unwrap().author.seconds, seconds);
        assert_eq!(parsed.to_fields().unwrap().author.offset_minutes, offset);
        assert_eq!(
            Commit::new(parsed.to_fields().unwrap())
                .unwrap()
                .to_fields()
                .unwrap(),
            parsed.to_fields().unwrap()
        );
    }

    #[rstest]
    #[case::no_space(b"A<a> 1 +0000", b"A", b"a")]
    #[case::overlap(b"A <B <a> 1 +0000", b"A", b"B <a")]
    #[case::closing(b"A > B <a> 1 +0000", b"A > B", b"a")]
    #[case::whitespace(b" A \t<a>\t1 +0000", b" A", b"a")]
    fn interprets_identity_delimiters(
        #[case] bytes: &[u8],
        #[case] name: &[u8],
        #[case] email: &[u8],
    ) {
        let parsed = Signature::parse(bytes).unwrap();
        assert_eq!(parsed.name, name);
        assert_eq!(parsed.email, email);
        assert_eq!(parsed.seconds, 1);
    }

    #[rstest]
    #[case::overflow("A <a> 9223372036854775808 +0000")]
    #[case::underflow("A <a> -9223372036854775809 +0000")]
    #[case::empty_seconds("A <a>  +0000")]
    #[case::nonnumeric("A <a> now +0000")]
    #[case::no_offset("A <a> 1")]
    #[case::no_sign("A <a> 1 0000")]
    #[case::zone_letters("A <a> 1 +ab00")]
    fn rejects_unreadable_dates(#[case] author: &str) {
        assert_eq!(
            Commit::parse(crate::ObjectFormat::Sha1, &payload(author, b"", b""))
                .unwrap()
                .to_fields(),
            Err(CommitError::InvalidDate)
        );
    }

    #[rstest]
    #[case::no_email("A 1 +0000")]
    #[case::no_closing_bracket("A <a 1 +0000")]
    fn rejects_unreadable_identities(#[case] author: &str) {
        assert_eq!(
            Commit::parse(crate::ObjectFormat::Sha1, &payload(author, b"", b""))
                .unwrap()
                .to_fields(),
            Err(CommitError::InvalidSignature)
        );
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::space(b"a b")]
    #[case::newline(b"a\nb")]
    #[case::non_ascii(b"\xff")]
    #[case::nul(b"a\0")]
    #[case::tree(b"tree")]
    #[case::parent(b"parent")]
    #[case::author(b"author")]
    #[case::committer(b"committer")]
    fn rejects_new_invalid_header_names(#[case] name: &[u8]) {
        let mut fields = fields();
        fields.extra_headers.push(CommitHeader {
            name: name.to_vec(),
            value: b"value".to_vec(),
        });
        assert_eq!(Commit::new(fields), Err(CommitError::InvalidHeader));
    }

    #[rstest]
    #[case::nul(b"opaque\0value")]
    #[case::cr(b"opaque\rvalue")]
    fn retains_header_values_rejected_by_construction(#[case] value: &[u8]) {
        let extra = [b"x ".as_slice(), value, b"\n"].concat();
        let input = payload(PERSON, &extra, b"");
        let parsed = Commit::parse(crate::ObjectFormat::Sha1, &input).unwrap();
        assert_eq!(parsed.encode(), input);
        assert_eq!(parsed.validate(), Err(CommitError::InvalidHeader));
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::newline(b"A\nB")]
    #[case::angle(b"A<B")]
    #[case::closing_angle(b"A>B")]
    fn rejects_new_identity_components(#[case] component: &[u8]) {
        let mut author_fields = fields();
        author_fields.author.name = component.to_vec();
        let mut committer_fields = fields();
        committer_fields.committer.email = component.to_vec();
        assert_eq!(
            Commit::new(author_fields),
            Err(CommitError::InvalidSignature)
        );
        assert_eq!(
            Commit::new(committer_fields),
            Err(CommitError::InvalidSignature)
        );
    }

    #[rstest]
    #[case::positive(1440)]
    #[case::negative(-1440)]
    #[case::minimum(i16::MIN)]
    fn rejects_new_offset_out_of_range(#[case] offset: i16) {
        let mut fields = fields();
        fields.author.offset_minutes = offset;
        assert_eq!(Commit::new(fields), Err(CommitError::InvalidDate));
    }

    #[rstest]
    #[case::missing_separator(b"tree abc", CommitError::RequiredHeader)]
    #[case::missing_headers(b"\n\n", CommitError::RequiredHeader)]
    #[case::short_id(b"tree abcd\n\n", CommitError::InvalidObjectId)]
    #[case::sha256(
        b"tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\n",
        CommitError::InvalidObjectId
    )]
    #[case::nonhex(
        b"tree gggggggggggggggggggggggggggggggggggggggg\n\n",
        CommitError::InvalidObjectId
    )]
    #[case::bad_parent(
        b"tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nparent gggggggggggggggggggggggggggggggggggggggg\n\n",
        CommitError::InvalidObjectId
    )]
    #[case::reordered(
        b"author A <a> 1 +0000\ntree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\n",
        CommitError::RequiredHeader
    )]
    fn rejects_incomplete_or_unsupported_headers(#[case] input: &[u8], #[case] error: CommitError) {
        assert_eq!(Commit::parse(crate::ObjectFormat::Sha1, input), Err(error));
    }

    #[rstest]
    #[case::orphan_continuation(b" continued\n")]
    #[case::missing_space(b"x-value\n")]
    #[case::tab_continuation(b"\tcontinued\n")]
    #[case::repeated_tree(b"tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n")]
    #[case::misplaced_parent(b"parent aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n")]
    #[case::repeated_author(b"author A <a> 1 +0000\n")]
    #[case::repeated_committer(b"committer A <a> 1 +0000\n")]
    fn retains_legacy_extra_headers(#[case] extra: &[u8]) {
        let input = payload(PERSON, extra, b"");
        let parsed = Commit::parse(crate::ObjectFormat::Sha1, &input).unwrap();
        assert_eq!(parsed.encode(), input);
        assert_eq!(parsed.parents(), []);
    }
}

#[cfg(test)]
mod format_boundary_tests {
    use super::*;

    #[rstest::rstest]
    #[case::tree(ObjectId::Sha256([1; 32]), ObjectId::Sha1([1; 20]))]
    #[case::parent(ObjectId::Sha1([1; 20]), ObjectId::Sha256([1; 32]))]
    fn rejects_sha256_fields(#[case] tree: ObjectId, #[case] parent: ObjectId) {
        let payload = format!(
            "tree {}\nauthor A <a> 0 +0000\ncommitter C <c> 0 +0000\n\n",
            ObjectId::Sha1([1; 20])
        );
        let mut fields = Commit::parse(crate::ObjectFormat::Sha1, payload.as_bytes())
            .unwrap()
            .to_fields()
            .unwrap()
            .clone();
        fields.tree = tree;
        fields.parents = vec![parent];
        assert!(matches!(
            Commit::new(fields),
            Err(CommitError::ObjectFormat(_))
        ));
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
    fn parsing_rejects_foreign_parent(#[case] format: ObjectFormat, #[case] foreign: ObjectFormat) {
        let bytes = format!(
            "tree {}\nparent {}\nauthor A <a> 1 +0000\ncommitter C <c> 1 +0000\n\n",
            ObjectId::null(format),
            ObjectId::null(foreign)
        );
        assert_eq!(
            Commit::parse(format, bytes.as_bytes()),
            Err(CommitError::InvalidObjectId)
        );
    }

    #[test]
    fn sha256_nonhex_tree_is_not_an_identity() {
        let bytes = format!(
            "tree {}\nauthor A <a> 1 +0000\ncommitter C <c> 1 +0000\n\n",
            "z".repeat(64)
        );
        assert_eq!(
            Commit::parse(ObjectFormat::Sha256, bytes.as_bytes()),
            Err(CommitError::InvalidObjectId)
        );
    }
}
