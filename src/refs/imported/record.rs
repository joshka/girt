#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]
use crate::refs::reftable::LogValue;
use crate::{IdentityRef, ObjectFormat, ObjectId};

/// Exact imported storage data for one history record.
///
/// Files records retain their newline when present, including incomplete prefixes on stopped reads.
/// Reftable records retain decoded binary fields and their update index; compressed table bytes
/// belong to the reftable codec. No message normalization or construction validation occurs.
///
/// Retain this representation when exact storage bytes or wider binary fields matter. Field
/// interpretation can fail while independent fixed-position IDs remain inspectable; recovered IDs
/// are only conservative candidates.
///
/// ```
/// use girt::refs::{ImportedReflog, ReflogLimits};
/// use girt::{ObjectFormat, ObjectId};
///
/// let format = ObjectFormat::Sha1;
/// let id = ObjectId::Sha1([1; 20]);
/// let bytes = format!(
///     "{} {id} A <a@example.com> 60 +0130\tnote\n",
///     ObjectId::null(format)
/// );
///
/// let broken = bytes.replace("60 +0130", "invalid date");
/// let log = ImportedReflog::read(
///     format,
///     broken.as_bytes(),
///     ReflogLimits::default(),
///     &std::sync::atomic::AtomicBool::new(false),
/// );
///
/// assert!(log.records()[0].fields().is_err());
/// assert_eq!(
///     log.records()[0].recoverable_roots().collect::<Vec<_>>(),
///     [id]
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ImportedRecord {
    /// One files-backend line or a final incomplete prefix.
    File {
        /// Hash format used to recover fixed-width object IDs.
        format: ObjectFormat,
        /// Exact bytes read, including arbitrary identity/message bytes.
        bytes: Vec<u8>,
    },
    /// One live binary record after stack precedence and tombstones are resolved.
    Reftable {
        /// Transaction index in the source stack.
        update_index: u64,
        /// Exact decoded fields, including the full unsigned timestamp range.
        value: LogValue,
    },
}

/// Borrowed interpreted fields without canonical write validation or Git display fallbacks.
///
/// Use interpreted fields for display or analysis while retaining the ImportedRecord for exact
/// export. Numeric interpretation and byte preservation are separate; converting to a construction
/// Signature can impose narrower rules.
///
/// ```
/// use girt::refs::{ImportedReflog, ReflogLimits};
/// use girt::{ObjectFormat, ObjectId};
///
/// let format = ObjectFormat::Sha1;
/// let id = ObjectId::Sha1([1; 20]);
/// let bytes = format!(
///     "{} {id} A <a@example.com> 60 +0130\tnote\n",
///     ObjectId::null(format)
/// );
///
/// let log = ImportedReflog::read(
///     format,
///     bytes.as_bytes(),
///     ReflogLimits::default(),
///     &std::sync::atomic::AtomicBool::new(false),
/// );
///
/// let fields = log.records()[0].fields()?;
/// assert_eq!((fields.seconds, fields.offset_minutes), (60, 90));
/// assert_eq!(fields.message, b"note");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReflogFields<'a> {
    /// Previous identity, including zero for creation.
    pub old: ObjectId,
    /// Replacement identity, including zero for deletion.
    pub new: ObjectId,
    /// Name bytes, with files delimiter padding trimmed.
    pub name: &'a [u8],
    /// Email bytes without files angle brackets.
    pub email: &'a [u8],
    /// Decimal seconds. The wider range includes every reftable unsigned timestamp.
    pub seconds: i128,
    /// Signed timezone offset in minutes; noncanonical HHMM minutes are arithmetic.
    pub offset_minutes: i16,
    /// Message bytes without the files tab separator and one final storage LF on either backend.
    pub message: &'a [u8],
}

/// Interpretation failed while the original bytes and recoverable IDs remain available.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ReflogInterpretationError {
    /// File record lacks its final newline, including a bounded partial read.
    /// Inspect whether scanning stopped or storage has an unterminated record; retain the prefix
    /// and never assume complete history.
    #[error("incomplete reflog record")]
    Incomplete,
    /// One or both fixed-width IDs or their separating spaces are invalid.
    /// Inspect the original fixed-position IDs and selected hash format; recoverable candidates may
    /// be incomplete.
    #[error("malformed reflog object IDs")]
    ObjectIds,
    /// Identity brackets or decimal date framing cannot be interpreted.
    /// Retain the record bytes and report the uninterpretable identity or date without inventing a
    /// fallback.
    #[error("malformed reflog identity or date")]
    Identity,
    /// Decimal seconds exceed i128.
    /// Preserve raw decimal bytes or report the supported range; truncating the timestamp changes
    /// meaning.
    #[error("reflog date exceeds interpretation range")]
    DateRange,
}

impl ImportedRecord {
    /// Recovers each nonzero fixed-position ID independently of the other ID, identity and date.
    ///
    /// These are conservative candidates, not a complete GC root set. Check
    /// [`super::ImportedReflog::is_complete`] before using a scan to justify removal. Corrupt
    /// separators or IDs can conceal additional roots; no search of arbitrary message hex occurs.
    pub fn recoverable_roots(&self) -> impl Iterator<Item = ObjectId> {
        let ids = match self {
            Self::File { format, bytes } => file_ids(*format, bytes),
            Self::Reftable { value, .. } => [Some(value.old), Some(value.new)],
        };
        ids.into_iter()
            .flatten()
            .filter(|id| *id != ObjectId::null(id.format()))
    }

    /// Interprets fields while preserving the source representation in this record.
    ///
    /// Files accept signed i128 seconds and exactly four signed HHMM digits. Text after those
    /// digits is message data, with one optional leading tab removed. Short zones remain explicit
    /// interpretation failures; CR/NUL and non-UTF-8 message bytes are retained. This describes
    /// this operation, not a universal Git grammar or `git reflog show` display behavior.
    ///
    /// # Errors
    ///
    /// Reports incomplete files framing, malformed IDs/identity/date and numeric overflow.
    pub fn fields(&self) -> Result<ReflogFields<'_>, ReflogInterpretationError> {
        match self {
            Self::File { format, bytes } => file_fields(*format, bytes),
            Self::Reftable { value, .. } => Ok(ReflogFields {
                old: value.old,
                new: value.new,
                name: &value.name,
                email: &value.email,
                seconds: i128::from(value.seconds),
                offset_minutes: value.offset_minutes,
                message: value.message.strip_suffix(b"\n").unwrap_or(&value.message),
            }),
        }
    }
}

fn file_ids(format: ObjectFormat, bytes: &[u8]) -> [Option<ObjectId>; 2] {
    let width = format.digest_len() * 2;
    let id = |start| {
        bytes
            .get(start..start + width)
            .and_then(|s| std::str::from_utf8(s).ok())
            .and_then(|s| ObjectId::from_hex(format, s).ok())
    };
    [id(0), id(width + 1)]
}

fn file_fields(
    format: ObjectFormat,
    bytes: &[u8],
) -> Result<ReflogFields<'_>, ReflogInterpretationError> {
    use ReflogInterpretationError as E;
    let line = bytes.strip_suffix(b"\n").ok_or(E::Incomplete)?;
    let width = format.digest_len() * 2;
    let [Some(old), Some(new)] = file_ids(format, line) else {
        return Err(E::ObjectIds);
    };
    if line.get(width) != Some(&b' ') || line.get(2 * width + 1) != Some(&b' ') {
        return Err(E::ObjectIds);
    }
    let identity = line
        .get(2 * width + 2..)
        .and_then(|identity| IdentityRef::parse(identity).ok())
        .ok_or(E::Identity)?;
    let date = identity.date_bytes.strip_prefix(b" ").ok_or(E::Identity)?;
    let (seconds, zone) = crate::bytes::split_once(date, b' ').ok_or(E::Identity)?;
    let seconds = std::str::from_utf8(seconds).map_err(|_| E::Identity)?;
    let digits = seconds.strip_prefix(['+', '-']).unwrap_or(seconds);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(E::Identity);
    }
    let seconds = seconds.parse::<i128>().map_err(|_| E::DateRange)?;
    let Some((&[sign, digits @ ..], message)) = zone.split_first_chunk::<5>() else {
        return Err(E::Identity);
    };
    if !matches!(sign, b'+' | b'-') || !digits.iter().all(u8::is_ascii_digit) {
        return Err(E::Identity);
    }
    // Four decimal digits are at most 9999, which fits in i16.
    let hhmm = digits
        .iter()
        .fold(0_i16, |value, digit| value * 10 + i16::from(digit - b'0'));
    let hhmm = if sign == b'-' { -hhmm } else { hhmm };
    let message = message.strip_prefix(b"\t").unwrap_or(message);
    Ok(ReflogFields {
        old,
        new,
        name: identity.name,
        email: identity.email,
        seconds,
        offset_minutes: hhmm / 100 * 60 + hhmm % 100,
        message,
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn fields(tail: &[u8]) -> Result<(i16, Vec<u8>), ReflogInterpretationError> {
        let mut bytes =
            format!("{} {} ", ObjectId::Sha1([0; 20]), ObjectId::Sha1([1; 20])).into_bytes();
        bytes.extend_from_slice(tail);
        let record = ImportedRecord::File {
            format: ObjectFormat::Sha1,
            bytes,
        };
        record
            .fields()
            .map(|fields| (fields.offset_minutes, fields.message.to_vec()))
    }

    #[rstest]
    #[case::east(b"A <a> 1 +0130\tm\n", 90, b"m")]
    #[case::west(b"A <a> 1 -0130m\n", -90, b"m")]
    #[case::maximum(b"A <a> 1 +9999\n", 6039, b"")]
    fn interprets_four_digit_zones(
        #[case] tail: &[u8],
        #[case] offset: i16,
        #[case] message: &[u8],
    ) {
        assert_eq!(fields(tail), Ok((offset, message.to_vec())));
    }

    #[rstest]
    #[case::sign_only(b"A <a> 1 +\n")]
    #[case::short(b"A <a> 1 +013\n")]
    #[case::text(b"A <a> 1 +01x0\n")]
    #[case::unsigned(b"A <a> 1 01300\n")]
    #[case::no_zone(b"A <a> 1\n")]
    #[case::no_identity(b"\n")]
    fn rejects_malformed_zones(#[case] tail: &[u8]) {
        assert_eq!(fields(tail), Err(ReflogInterpretationError::Identity));
    }
}
