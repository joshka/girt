use crate::refs::reftable::LogValue;
use crate::{IdentityRef, ObjectFormat, ObjectId};

/// Exact imported storage data for one history record.
///
/// Files records retain their newline when present, including incomplete prefixes on stopped reads.
/// Reftable records retain decoded binary fields and their update index; compressed table bytes
/// belong to the reftable codec. No message normalization or construction validation occurs.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    /// Exact message bytes; files exclude one tab separator and final LF, binary includes its LF.
    pub message: &'a [u8],
}

/// Interpretation failed while the original bytes and recoverable IDs remain available.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReflogInterpretationError {
    /// File record lacks its final newline, including a bounded partial read.
    #[error("incomplete reflog record")]
    Incomplete,
    /// One or both fixed-width IDs or their separating spaces are invalid.
    #[error("malformed reflog object IDs")]
    ObjectIds,
    /// Identity brackets or decimal date framing cannot be interpreted.
    #[error("malformed reflog identity or date")]
    Identity,
    /// Seconds exceed i128 or the arithmetic offset exceeds i16 minutes.
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
                message: &value.message,
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
    let identity = IdentityRef::parse(&line[2 * width + 2..]).map_err(|_| E::Identity)?;
    let date = identity.date_bytes.strip_prefix(b" ").ok_or(E::Identity)?;
    let split = date.iter().position(|b| *b == b' ').ok_or(E::Identity)?;
    let seconds = std::str::from_utf8(&date[..split]).map_err(|_| E::Identity)?;
    let digits = seconds.strip_prefix(['+', '-']).unwrap_or(seconds);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(E::Identity);
    }
    let seconds = seconds.parse::<i128>().map_err(|_| E::DateRange)?;
    let zone = &date[split + 1..];
    if zone.len() < 5
        || !matches!(zone[0], b'+' | b'-')
        || !zone[1..5].iter().all(u8::is_ascii_digit)
    {
        return Err(E::Identity);
    }
    let hhmm = std::str::from_utf8(&zone[..5])
        .unwrap()
        .parse::<i16>()
        .unwrap();
    let message = zone[5..].strip_prefix(b"\t").unwrap_or(&zone[5..]);
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
