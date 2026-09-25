use std::path::PathBuf;

use super::store::{malformed, read_optional};
use super::{RefName, ReferenceError, References};
use crate::{ObjectId, Signature};

/// One format-bearing reflog record, in oldest-to-newest file order.
///
/// Zero IDs represent absence. Parsing preserves message and identity bytes, but normalizes the
/// numeric timestamp and timezone (including negative zero). An omitted message separator maps to
/// an empty message. Object existence and continuity
/// between records are not checked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReflogEntry {
    /// Previous object ID, or zero for creation.
    pub old: ObjectId,
    /// New object ID, or zero for deletion.
    pub new: ObjectId,
    /// Caller-supplied identity, Unix seconds and timezone.
    pub committer: Signature,
    /// Single-line message bytes, without the tab separator or final newline.
    pub message: Vec<u8>,
}

/// Explicit policy for every name affected by an operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reflog {
    /// Leave existing logs unchanged and create none, including on deletion.
    Preserve,
    /// Create a missing log or append to an existing log, even for an unchanged ID.
    ///
    /// Resolved operations log every name in the symbolic chain, using the terminal old/new IDs.
    /// Deletion appends a zero new ID and retains the file. Environment, hooks and Git config are
    /// never consulted. Stored symbolic replacement/deletion with logging is unsupported.
    Append {
        /// Validated like commit construction, additionally requiring nonnegative seconds.
        committer: Signature,
        /// Exact bytes; NUL, CR and LF are rejected. No whitespace normalization is performed.
        message: Vec<u8>,
    },
}

impl ReflogEntry {
    /// Parses a complete newline-terminated reflog, returning records in file order.
    ///
    /// The caller selects the repository format, including for empty input; every ID must match.
    /// Names/emails must be nonempty, timestamps nonnegative, timezone
    /// hours at most 23 and minutes at most 59. UTF-8 is not required. Messages may contain tabs.
    ///
    /// # Errors
    ///
    /// Rejects malformed or foreign-format IDs, identity/date framing, invalid identities, NUL/CR
    /// messages, and truncated records. Errors identify the format rule; filesystem readers add
    /// the log path.
    pub fn parse(format: crate::ObjectFormat, bytes: &[u8]) -> Result<Vec<Self>, ReferenceError> {
        parse(format, bytes, &std::path::PathBuf::from("<reflog>"))
    }

    pub(super) fn encode(&self) -> Result<Vec<u8>, ReferenceError> {
        self.new.require_format(self.old.format())?;
        validate(&self.committer, &self.message)?;
        let mut bytes = format!("{} {}", self.old, self.new).into_bytes();
        self.committer.encode(b"", &mut bytes);
        bytes.pop();
        bytes.push(b'\t');
        bytes.extend_from_slice(&self.message);
        bytes.push(b'\n');
        Ok(bytes)
    }
}

impl References<'_> {
    /// Reads a complete reflog in oldest-to-newest order, or returns `None` if absent.
    ///
    /// Uses the same worktree routing as references. Reads are live and may see an incomplete
    /// append by another writer. Memory is proportional to the whole log; expiry and streaming
    /// reads are deferred.
    ///
    /// # Errors
    ///
    /// Reports filesystem, symlink, and malformed-record errors, including a truncated tail.
    pub fn reflog(&self, name: &RefName) -> Result<Option<Vec<ReflogEntry>>, ReferenceError> {
        let path = self.reflog_path(name)?;
        read_optional(&path)?
            .map(|bytes| parse(self.repository.object_format(), &bytes, &path))
            .transpose()
    }

    pub(super) fn reflog_path(&self, name: &RefName) -> Result<PathBuf, ReferenceError> {
        let path = self.path(name)?;
        let root = if name.per_worktree() {
            self.repository.git_dir()
        } else {
            self.repository.common_dir()
        };
        Ok(root.join("logs").join(path.strip_prefix(root).unwrap()))
    }
}

pub(super) fn validate(committer: &Signature, message: &[u8]) -> Result<(), ReferenceError> {
    if committer.name.contains(&b'\t') || committer.email.contains(&b'\t') {
        return Err(ReferenceError::Unsupported("tab in reflog identity"));
    }
    // Reflog policy is narrower than commit/tag timestamp construction.
    if committer.seconds < 0 {
        return Err(ReferenceError::Unsupported("negative reflog timestamp"));
    }
    committer
        .validate()
        .map_err(|_| ReferenceError::Unsupported("invalid reflog identity or date"))?;
    if message.iter().any(|b| matches!(b, 0 | b'\r' | b'\n')) {
        return Err(ReferenceError::Unsupported(
            "multiline or NUL reflog message",
        ));
    }
    Ok(())
}

pub(super) fn parse(
    format: crate::ObjectFormat,
    bytes: &[u8],
    path: &std::path::Path,
) -> Result<Vec<ReflogEntry>, ReferenceError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let body = bytes
        .strip_suffix(b"\n")
        .ok_or_else(|| malformed(path, "unterminated reflog record"))?;
    body.split(|b| *b == b'\n')
        .map(|line| parse_line(format, line, path))
        .collect()
}

fn parse_line(
    format: crate::ObjectFormat,
    line: &[u8],
    path: &std::path::Path,
) -> Result<ReflogEntry, ReferenceError> {
    let width = format.digest_len() * 2;
    if line.get(width) != Some(&b' ') || line.get(2 * width + 1) != Some(&b' ') {
        return Err(malformed(path, "expected two repository-format reflog IDs"));
    }
    let parse_id = |bytes| {
        std::str::from_utf8(bytes)
            .ok()
            .and_then(|s| ObjectId::from_hex(format, s).ok())
            .ok_or_else(|| malformed(path, "invalid reflog ID"))
    };
    let old = parse_id(&line[..width])?;
    let new = parse_id(&line[width + 1..2 * width + 1])?;
    let rest = &line[2 * width + 2..];
    let tab = rest.iter().position(|b| *b == b'\t').unwrap_or(rest.len());
    let identity = &rest[..tab];
    let mut committer = Signature::parse(identity)
        .map_err(|_| malformed(path, "invalid reflog identity or date"))?;
    // Reflogs preserve and validate the raw name, including padding that commit interpretation
    // treats as delimiter whitespace. Keep that existing policy at the reflog boundary.
    let name_end = identity
        .windows(2)
        .position(|pair| pair == b" <")
        .ok_or_else(|| malformed(path, "missing space before reflog email"))?;
    committer.name = identity[..name_end].to_vec();
    let message = rest.get(tab + 1..).unwrap_or_default().to_vec();
    validate(&committer, &message)
        .map_err(|_| malformed(path, "invalid reflog identity, date or message"))?;
    Ok(ReflogEntry {
        old,
        new,
        committer,
        message,
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn record(tail: &[u8]) -> Vec<u8> {
        let mut bytes =
            format!("{} {} ", ObjectId::Sha1([0; 20]), ObjectId::Sha1([1; 20])).into_bytes();
        bytes.extend_from_slice(tail);
        bytes
    }

    #[rstest]
    #[case::truncated(b"A <a@b> 1 +0000\tmessage")]
    #[case::invalid_zone(b"A <a@b> 1 +2460\tmessage\n")]
    #[case::padded_name(b"A  <a@b> 1 +0000\tmessage\n")]
    #[case::cr_name(b"A\r <a@b> 1 +0000\tmessage\n")]
    #[case::no_space(b"A<a@b> 1 +0000\tmessage\n")]
    #[case::negative_time(b"A <a@b> -1 +0000\tmessage\n")]
    #[case::empty_name(b" <a@b> 1 +0000\tmessage\n")]
    #[case::nul(b"A <a@b> 1 +0000\tx\0y\n")]
    #[case::cr(b"A <a@b> 1 +0000\tx\ry\n")]
    #[case::missing_separator(b"A <a@b> 1 +0000 message\n")]
    fn rejects_malformed_records(#[case] tail: &[u8]) {
        assert!(matches!(
            ReflogEntry::parse(crate::ObjectFormat::Sha1, &record(tail)),
            Err(ReferenceError::Malformed { .. })
        ));
    }

    #[test]
    fn parses_byte_identity_and_message_without_utf8() {
        let bytes = record(b"A\xff <a@b> 123 -0330\tm\xff\tend\n");
        let entries = ReflogEntry::parse(crate::ObjectFormat::Sha1, &bytes).unwrap();
        assert_eq!(entries[0].committer.name, b"A\xff");
        assert_eq!(entries[0].committer.offset_minutes, -210);
        assert_eq!(entries[0].encode().unwrap(), bytes);
    }

    #[test]
    fn rejects_bad_ids() {
        let mut bytes = record(b"A <a@b> 1 +0000\tm\n");
        bytes[0] = b'z';
        assert!(ReflogEntry::parse(crate::ObjectFormat::Sha1, &bytes).is_err());
    }
}

#[cfg(test)]
mod format_tests {
    use rstest::rstest;

    use super::*;
    use crate::ObjectFormat;

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1, ObjectFormat::Sha256)]
    #[case::sha256(ObjectFormat::Sha256, ObjectFormat::Sha1)]
    fn null_records_use_the_selected_format(
        #[case] format: ObjectFormat,
        #[case] other: ObjectFormat,
    ) {
        let bytes = format!(
            "{} {} A <a@b> 1 +0000\tmessage\n",
            ObjectId::null(format),
            ObjectId::null(format)
        );
        let records = ReflogEntry::parse(format, bytes.as_bytes()).unwrap();
        assert_eq!(records[0].old, ObjectId::null(format));
        assert_eq!(records[0].new, ObjectId::null(format));
        assert_eq!(records[0].encode().unwrap(), bytes.as_bytes());
        assert!(ReflogEntry::parse(other, bytes.as_bytes()).is_err());
        assert!(ReflogEntry::parse(format, b"").unwrap().is_empty());
        let mixed = ReflogEntry {
            new: ObjectId::null(other),
            ..records[0].clone()
        };
        assert!(matches!(
            mixed.encode(),
            Err(ReferenceError::ObjectFormat(_))
        ));
    }
}
