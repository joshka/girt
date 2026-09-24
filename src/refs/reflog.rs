use std::path::PathBuf;

use super::store::{malformed, read_optional};
use super::{RefName, ReferenceError, References};
use crate::{ObjectId, Signature};

/// One SHA-1 reflog record, in oldest-to-newest file order.
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
        /// Validated using the same identity/date rules as commit construction.
        committer: Signature,
        /// Exact bytes; NUL, CR and LF are rejected. No whitespace normalization is performed.
        message: Vec<u8>,
    },
}

impl ReflogEntry {
    /// Parses a complete newline-terminated reflog, returning records in file order.
    ///
    /// Empty input is valid. Names/emails must be nonempty, timestamps nonnegative, timezone
    /// hours at most 23 and minutes at most 59. UTF-8 is not required. Messages may contain tabs.
    ///
    /// # Errors
    ///
    /// Rejects malformed IDs, identity/date framing, invalid identities, NUL/CR messages, and
    /// truncated records. Errors identify the format rule; filesystem readers add the log path.
    pub fn parse(bytes: &[u8]) -> Result<Vec<Self>, ReferenceError> {
        parse(bytes, &std::path::PathBuf::from("<reflog>"))
    }

    pub(super) fn encode(&self) -> Result<Vec<u8>, ReferenceError> {
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
            .map(|bytes| parse(&bytes, &path))
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
        .map(|line| parse_line(line, path))
        .collect()
}

fn parse_line(line: &[u8], path: &std::path::Path) -> Result<ReflogEntry, ReferenceError> {
    if line.get(40) != Some(&b' ') || line.get(81) != Some(&b' ') {
        return Err(malformed(path, "expected two SHA-1 reflog IDs"));
    }
    let parse_id = |bytes| {
        std::str::from_utf8(bytes)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| malformed(path, "invalid reflog ID"))
    };
    let old = parse_id(&line[..40])?;
    let new = parse_id(&line[41..81])?;
    let rest = &line[82..];
    let tab = rest.iter().position(|b| *b == b'\t').unwrap_or(rest.len());
    let committer = Signature::parse(&rest[..tab])
        .map_err(|_| malformed(path, "invalid reflog identity or date"))?;
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
        let mut bytes = format!(
            "{} {} ",
            ObjectId::from_bytes([0; 20]),
            ObjectId::from_bytes([1; 20])
        )
        .into_bytes();
        bytes.extend_from_slice(tail);
        bytes
    }

    #[rstest]
    #[case::truncated(b"A <a@b> 1 +0000\tmessage")]
    #[case::invalid_zone(b"A <a@b> 1 +2460\tmessage\n")]
    #[case::negative_time(b"A <a@b> -1 +0000\tmessage\n")]
    #[case::empty_name(b" <a@b> 1 +0000\tmessage\n")]
    #[case::nul(b"A <a@b> 1 +0000\tx\0y\n")]
    #[case::cr(b"A <a@b> 1 +0000\tx\ry\n")]
    #[case::missing_separator(b"A <a@b> 1 +0000 message\n")]
    fn rejects_malformed_records(#[case] tail: &[u8]) {
        assert!(matches!(
            ReflogEntry::parse(&record(tail)),
            Err(ReferenceError::Malformed { .. })
        ));
    }

    #[test]
    fn parses_byte_identity_and_message_without_utf8() {
        let bytes = record(b"A\xff <a@b> 123 -0330\tm\xff\tend\n");
        let entries = ReflogEntry::parse(&bytes).unwrap();
        assert_eq!(entries[0].committer.name, b"A\xff");
        assert_eq!(entries[0].committer.offset_minutes, -210);
        assert_eq!(entries[0].encode().unwrap(), bytes);
    }

    #[test]
    fn rejects_bad_ids() {
        let mut bytes = record(b"A <a@b> 1 +0000\tm\n");
        bytes[0] = b'z';
        assert!(ReflogEntry::parse(&bytes).is_err());
    }
}
