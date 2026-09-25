mod payload;

pub use payload::{CommitHeaderRef, CommitPayload};

use crate::ObjectId;

/// An owned SHA-1 commit describing a tree, ordered parents, people, and a message.
///
/// [`Self::parse`] retains the original payload as well as decoded fields. [`Self::encode`] and
/// [`Self::id`] therefore preserve every accepted byte, including hexadecimal case, date spelling,
/// negative-zero offsets, unknown headers, and message bytes. No UTF-8 conversion is performed on
/// names, email addresses, header values, or messages. Fields are immutable after construction.
///
/// [`Self::new`] validates caller-supplied fields and emits canonical framing. [`Self::validate`]
/// checks the same field rules on a parsed object without modifying it. Reconstructing parsed
/// fields with `new` can change identity even when validation succeeds: representation details such
/// as date spelling belong to the original payload, not the decoded fields.
///
/// The supported grammar requires tree, zero or more parents, author, committer, then extra
/// headers, followed by a blank line and arbitrary message bytes. IDs are exactly 40 hexadecimal
/// SHA-1 digits. Dates fit signed 64-bit seconds and offsets use `+HHMM` or `-HHMM`, with hours
/// below 24 and minutes below 60. Reordered or repeated required headers, continuations on required
/// headers, SHA-256 IDs, missing separators, and other date grammars are rejected explicitly.
/// Use [`CommitPayload`] to retain structurally framed bytes independently of these restrictions.
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
///     tree: Tree::new(vec![])?.id(),
///     parents: vec![],
///     author: person.clone(),
///     committer: person,
///     extra_headers: vec![],
///     message: b"Initial snapshot\n".to_vec(),
/// })?;
/// let parsed = Commit::parse(commit.as_bytes())?;
/// assert_eq!(parsed.fields().message, b"Initial snapshot\n");
/// assert_eq!(parsed.id(), commit.id());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Commit {
    fields: CommitFields,
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
        Ok(Self { fields, payload })
    }

    /// Parses supported commit framing and copies the original payload and decoded fields.
    ///
    /// Empty identities, negative seconds, and NUL/CR in values can be inspected; use
    /// [`Self::validate`] when construction rules are required. Signed or zero-padded seconds and
    /// uppercase IDs remain unchanged in the payload. Unknown headers retain order and duplicates.
    ///
    /// # Errors
    ///
    /// Returns [`CommitError`] for unsupported or malformed framing, missing required headers,
    /// invalid SHA-1 IDs, or unreadable identities/dates. See [`Commit`] for the supported grammar.
    pub fn parse(payload: &[u8]) -> Result<Self, CommitError> {
        let separator = payload
            .windows(2)
            .position(|pair| pair == b"\n\n")
            .ok_or(CommitError::MissingSeparator)?;
        let mut lines = payload[..separator].split(|&byte| byte == b'\n').peekable();
        let tree = parse_id(required_line(lines.next(), b"tree ")?)?;
        let mut parents = Vec::new();
        while lines
            .peek()
            .is_some_and(|line| line.starts_with(b"parent "))
        {
            parents.push(parse_id(required_line(lines.next(), b"parent ")?)?);
        }
        let author = Signature::parse(required_line(lines.next(), b"author ")?)?;
        let committer = Signature::parse(required_line(lines.next(), b"committer ")?)?;
        let mut extra_headers: Vec<CommitHeader> = Vec::new();
        for line in lines {
            if let Some(continuation) = line.strip_prefix(b" ") {
                let header = extra_headers.last_mut().ok_or(CommitError::InvalidHeader)?;
                header.value.push(b'\n');
                header.value.extend_from_slice(continuation);
            } else {
                let space = line
                    .iter()
                    .position(|&byte| byte == b' ')
                    .ok_or(CommitError::InvalidHeader)?;
                let name = &line[..space];
                validate_header_name(name)?;
                extra_headers.push(CommitHeader {
                    name: name.to_vec(),
                    value: line[space + 1..].to_vec(),
                });
            }
        }
        Ok(Self {
            fields: CommitFields {
                tree,
                parents,
                author,
                committer,
                extra_headers,
                message: payload[separator + 2..].to_vec(),
            },
            payload: payload.to_vec(),
        })
    }

    /// Borrows decoded fields. Clone them and call [`Self::new`] for an explicit reconstruction.
    pub fn fields(&self) -> &CommitFields {
        &self.fields
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
        self.fields.validate()
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
        ObjectId::for_object("commit", &self.payload)
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
    /// Interprets identity bytes and a signed seconds/four-digit offset date.
    ///
    /// The first `<` and following `>` delimit email bytes. ASCII whitespace immediately before
    /// `<` is framing; leading name whitespace and embedded angle brackets remain inspectable.
    /// ASCII whitespace between `>` and seconds is skipped. The original representation belongs
    /// to the caller; use [`CommitPayload`] to retain it independently of interpretation success.
    /// Parsing does not enforce the stricter identity rules of [`Commit::new`].
    ///
    /// # Errors
    ///
    /// Returns [`CommitError::InvalidSignature`] for missing brackets/date separation, or
    /// [`CommitError::InvalidDate`] unless seconds fit `i64` and the offset has signed four-digit
    /// `HHMM` spelling with hours below 24 and minutes below 60. No fallback date is invented.
    pub fn parse(bytes: &[u8]) -> Result<Self, CommitError> {
        let open = bytes
            .iter()
            .position(|&byte| byte == b'<')
            .ok_or(CommitError::InvalidSignature)?;
        let email_start = open + 1;
        let close = bytes[email_start..]
            .iter()
            .position(|&byte| byte == b'>')
            .map(|index| email_start + index)
            .ok_or(CommitError::InvalidSignature)?;
        let suffix = &bytes[close + 1..];
        if !suffix.first().is_some_and(u8::is_ascii_whitespace) {
            return Err(CommitError::InvalidSignature);
        }
        let date = suffix.trim_ascii_start();
        let space = date
            .iter()
            .position(|&byte| byte == b' ')
            .ok_or(CommitError::InvalidDate)?;
        let seconds = std::str::from_utf8(&date[..space])
            .ok()
            .and_then(|value| value.parse().ok())
            .ok_or(CommitError::InvalidDate)?;
        let zone = &date[space + 1..];
        if zone.len() != 5
            || !matches!(zone[0], b'+' | b'-')
            || !zone[1..].iter().all(u8::is_ascii_digit)
        {
            return Err(CommitError::InvalidDate);
        }
        let hours = i16::from(zone[1] - b'0') * 10 + i16::from(zone[2] - b'0');
        let minutes = i16::from(zone[3] - b'0') * 10 + i16::from(zone[4] - b'0');
        if hours > 23 || minutes > 59 {
            return Err(CommitError::InvalidDate);
        }
        let offset = hours * 60 + minutes;
        Ok(Self {
            name: bytes[..open].trim_ascii_end().to_vec(),
            email: bytes[email_start..close].to_vec(),
            seconds,
            offset_minutes: if zone[0] == b'-' { -offset } else { offset },
        })
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
    /// The blank line separating headers from message is missing.
    #[error("missing commit header/message separator")]
    MissingSeparator,
    /// A required header is absent, reordered, repeated, or has an unsupported continuation.
    #[error("missing or misplaced required commit header")]
    RequiredHeader,
    /// A tree or parent identity is not exactly 40 hexadecimal SHA-1 digits.
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

fn parse_id(value: &[u8]) -> Result<ObjectId, CommitError> {
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
        Commit::parse(&payload(PERSON, b"", b"Initial\n"))
            .unwrap()
            .fields()
            .clone()
    }

    #[test]
    fn constructs_root_with_literal_payload_and_identity() {
        let commit = Commit::new(fields()).unwrap();
        assert_eq!(commit.encode(), payload(PERSON, b"", b"Initial\n"));
        assert_eq!(commit.fields().parents, []);
        assert_eq!(commit.validate(), Ok(()));
        assert_eq!(commit.fields().author.offset_minutes, 330);
        assert_eq!(
            commit.id().to_string(),
            "454b0eab56d2228f210a70360ab4d915f87b79c9"
        );
    }

    #[test]
    fn preserves_ordered_merge_parents_including_duplicates() {
        let mut fields = fields();
        fields.parents = vec![
            ObjectId::from_bytes([2; 20]),
            ObjectId::from_bytes([1; 20]),
            ObjectId::from_bytes([2; 20]),
        ];
        let commit = Commit::new(fields.clone()).unwrap();
        let expected = format!(
            "tree {TREE}\nparent {}\nparent {}\nparent {}\nauthor {PERSON}\ncommitter {PERSON}\n\nInitial\n",
            "02".repeat(20),
            "01".repeat(20),
            "02".repeat(20)
        );
        assert_eq!(commit.as_bytes(), expected.as_bytes());
        assert_eq!(Commit::parse(commit.as_bytes()).unwrap().fields(), &fields);
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::no_final_newline(b"message")]
    #[case::binary(b"\xff\0\r\n\nauthor fake\n")]
    fn preserves_message_bytes(#[case] message: &[u8]) {
        let input = payload(PERSON, b"", message);
        let commit = Commit::parse(&input).unwrap();
        assert_eq!(commit.fields().message, message);
        assert_eq!(commit.encode(), input);
        assert_eq!(
            Commit::new(commit.fields().clone()).unwrap().encode(),
            input
        );
    }

    #[test]
    fn preserves_unknown_repeated_and_multiline_headers() {
        let extra = b"encoding ISO-8859-1\nx-other \xff\ngpgsig first\n \n  indented\n last\n \nx-other again\n";
        let input = payload(PERSON, extra, b"body");
        let commit = Commit::parse(&input).unwrap();
        assert_eq!(
            commit.fields().extra_headers,
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
            Commit::new(commit.fields().clone()).unwrap().encode(),
            input
        );
    }

    #[test]
    fn retains_lexical_details_until_explicit_reconstruction() {
        let input = format!(
            "tree {}\nauthor A <a@example.com> +00042 -0000\ncommitter {PERSON}\n\n",
            TREE.to_uppercase()
        );
        let parsed = Commit::parse(input.as_bytes()).unwrap();
        assert_eq!(parsed.fields().author.seconds, 42);
        assert_eq!(parsed.fields().author.offset_minutes, 0);
        assert_eq!(parsed.validate(), Ok(()));
        assert_eq!(parsed.as_bytes(), input.as_bytes());
        assert_ne!(
            Commit::new(parsed.fields().clone()).unwrap().id(),
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
        let parsed = Commit::parse(&input).unwrap();
        assert_eq!(parsed.validate(), Err(error));
        assert_eq!(Commit::new(parsed.fields().clone()), Err(error));
        assert_eq!(parsed.encode(), input);
    }

    #[rstest]
    #[case::negative_offset("A <a@example.com> 0 -1200", 0, -720)]
    #[case::max_seconds("A <a@example.com> 9223372036854775807 +2359", i64::MAX, 1439)]
    #[case::min_seconds("A <a@example.com> -9223372036854775808 -2359", i64::MIN, -1439)]
    fn parses_date_boundaries(#[case] author: &str, #[case] seconds: i64, #[case] offset: i16) {
        let parsed = Commit::parse(&payload(author, b"", b"")).unwrap();
        assert_eq!(parsed.fields().author.seconds, seconds);
        assert_eq!(parsed.fields().author.offset_minutes, offset);
        assert_eq!(
            Commit::new(parsed.fields().clone()).unwrap().fields(),
            parsed.fields()
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
    #[case::short_zone("A <a> 1 +000")]
    #[case::long_zone("A <a> 1 +00000")]
    #[case::hours("A <a> 1 +2400")]
    #[case::minutes("A <a> 1 -0060")]
    #[case::zone_letters("A <a> 1 +ab00")]
    #[case::trailing_space("A <a> 1 +0000 ")]
    fn rejects_unreadable_dates(#[case] author: &str) {
        assert_eq!(
            Commit::parse(&payload(author, b"", b"")),
            Err(CommitError::InvalidDate)
        );
    }

    #[rstest]
    #[case::no_email("A 1 +0000")]
    #[case::no_closing_bracket("A <a 1 +0000")]
    #[case::no_date_separator("A <a>1 +0000")]
    fn rejects_unreadable_identities(#[case] author: &str) {
        assert_eq!(
            Commit::parse(&payload(author, b"", b"")),
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
        let parsed = Commit::parse(&input).unwrap();
        assert_eq!(parsed.encode(), input);
        assert_eq!(parsed.validate(), Err(CommitError::InvalidHeader));
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::newline(b"A\nB")]
    #[case::angle(b"A<B")]
    #[case::closing_angle(b"A>B")]
    #[case::trailing_space(b"A ")]
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
    #[case::missing_separator(b"tree abc", CommitError::MissingSeparator)]
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
    #[case::missing_author(
        b"tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\n",
        CommitError::RequiredHeader
    )]
    #[case::bad_parent(
        b"tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nparent short\n\n",
        CommitError::InvalidObjectId
    )]
    #[case::missing_committer(
        b"tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nauthor A <a> 1 +0000\n\n",
        CommitError::RequiredHeader
    )]
    #[case::continued_tree(b"tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n continuation\nauthor A <a> 1 +0000\ncommitter C <c> 1 +0000\n\n", CommitError::RequiredHeader)]
    #[case::continued_author(b"tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nauthor A <a> 1 +0000\n continuation\ncommitter C <c> 1 +0000\n\n", CommitError::RequiredHeader)]
    #[case::reordered(
        b"author A <a> 1 +0000\ntree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\n",
        CommitError::RequiredHeader
    )]
    fn rejects_incomplete_or_unsupported_headers(#[case] input: &[u8], #[case] error: CommitError) {
        assert_eq!(Commit::parse(input), Err(error));
    }

    #[rstest]
    #[case::orphan_continuation(b" continued\n")]
    #[case::missing_space(b"x-value\n")]
    #[case::tab_continuation(b"\tcontinued\n")]
    #[case::repeated_tree(b"tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n")]
    #[case::misplaced_parent(b"parent aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n")]
    #[case::repeated_author(b"author A <a> 1 +0000\n")]
    #[case::repeated_committer(b"committer A <a> 1 +0000\n")]
    fn rejects_misplaced_extra_headers(#[case] extra: &[u8]) {
        assert_eq!(
            Commit::parse(&payload(PERSON, extra, b"")),
            Err(CommitError::InvalidHeader)
        );
    }
}
