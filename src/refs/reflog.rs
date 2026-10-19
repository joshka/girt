#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]
use std::path::PathBuf;

use super::store::{malformed, read_optional};
use super::{RefName, ReferenceError, References};
use crate::{ObjectId, Signature};

/// One format-bearing reflog record, in oldest-to-newest file order.
///
/// Zero IDs represent absence. Parsing preserves message and email bytes, trims name delimiter
/// whitespace, and normalizes numeric timestamps/timezones. Use [`ReflogRecord`] to retain the
/// original lexical record. An omitted message separator maps to
/// an empty message. Object existence and continuity
/// between records are not checked.
///
/// ```
/// use girt::{ObjectFormat, ObjectId};
/// let old = ObjectId::null(ObjectFormat::Sha1);
/// let new = ObjectId::for_blob(ObjectFormat::Sha1, b"manifest");
/// let bytes = format!("{old} {new} A <a@example.com> 60 +0000\trecord\n");
/// let entries = girt::refs::ReflogEntry::parse(ObjectFormat::Sha1, bytes.as_bytes())?;
/// assert!(entries[0].old.is_null());
/// assert_eq!(entries[0].message, b"record");
/// # Ok::<(), girt::refs::ReferenceError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReflogEntry {
    /// Previous object ID, or zero for creation.
    pub old: ObjectId,
    /// New object ID, or zero for deletion.
    pub new: ObjectId,
    /// Caller-supplied identity, Unix seconds and timezone.
    pub committer: Signature,
    /// Message bytes without the files separator or final newline. Imported binary records may
    /// retain embedded control bytes.
    pub message: Vec<u8>,
}

/// A parsed imported record with its exact original bytes, including the final newline.
///
/// Interpretation may normalize name padding and numeric dates. Retain this value when copying
/// history: constructing a new entry applies stricter canonical identity and date rules.
///
/// ```
/// use girt::{ObjectFormat, ObjectId};
/// let old = ObjectId::null(ObjectFormat::Sha1);
/// let new = ObjectId::for_blob(ObjectFormat::Sha1, b"manifest");
/// let bytes = format!("{old} {new} A <a@example.com> 60 +0000\trecord\n");
/// let records = girt::refs::ReflogRecord::parse(ObjectFormat::Sha1, bytes.as_bytes())?;
/// assert_eq!(records[0].as_bytes(), bytes.as_bytes());
/// assert_eq!(records[0].entry().new, new);
/// # Ok::<(), girt::refs::ReferenceError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReflogRecord {
    bytes: Vec<u8>,
    entry: ReflogEntry,
}

impl ReflogRecord {
    /// Parses a complete log and retains every record byte.
    ///
    /// # Errors
    ///
    /// Returns the framing and numeric limits documented by [`ReflogEntry::parse`].
    ///
    /// ```
    /// use girt::{ObjectFormat, ObjectId};
    /// let old = ObjectId::null(ObjectFormat::Sha1);
    /// let new = ObjectId::for_blob(ObjectFormat::Sha1, b"manifest");
    /// let bytes = format!("{old} {new} A <a@example.com> 60 +0000\trecord\n");
    /// let records = girt::refs::ReflogRecord::parse(ObjectFormat::Sha1, bytes.as_bytes())?;
    /// assert_eq!(records[0].as_bytes(), bytes.as_bytes());
    /// # Ok::<(), girt::refs::ReferenceError>(())
    /// ```
    pub fn parse(format: crate::ObjectFormat, bytes: &[u8]) -> Result<Vec<Self>, ReferenceError> {
        let entries = ReflogEntry::parse(format, bytes)?;
        Ok(bytes
            .split_inclusive(|b| *b == b'\n')
            .zip(entries)
            .map(|(bytes, entry)| Self {
                bytes: bytes.to_vec(),
                entry,
            })
            .collect())
    }

    /// Returns the exact imported record, including delimiters and newline.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns interpreted fields without applying construction validation.
    pub fn entry(&self) -> &ReflogEntry {
        &self.entry
    }
}

/// Explicit policy for every name affected by an operation.
///
/// ```
/// # use girt::refs::{Expected, RefEdit, RefName, Reflog, Target};
/// # use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};
/// # let directory = tempfile::tempdir()?;
/// # let repo = Repository::init(ObjectFormat::Sha1, directory.path().join("r.git"), InitKind::Bare)?;
/// # let loose = repo.loose_objects();
/// # let person = Signature { name: b"A. Writer".to_vec(), email: b"a@example.com".to_vec(),
/// #                          seconds: 1_700_000_000, offset_minutes: 0 };
/// # let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
/// # let id = loose.write_commit(&Commit::new(CommitFields {
/// #     tree, parents: vec![], author: person.clone(), committer: person.clone(),
/// #     extra_headers: vec![], message: b"Initial snapshot\n".to_vec(),
/// # })?)?;
/// # let refs = repo.references()?;
/// # let main = RefName::new("refs/heads/main")?;
/// let policy = Reflog::AppendIfChanged { committer: person, message: b"publish".to_vec() };
/// let first = RefEdit::new(main.clone(), Some(Target::Direct(id)), Expected::Absent, policy.clone());
/// refs.transaction(&[first])?;
/// let unchanged = RefEdit::new(main.clone(), Some(Target::Direct(id)), Expected::Value(Target::Direct(id)), policy);
/// refs.transaction(&[unchanged])?;
/// assert_eq!(refs.reflog(&main)?.ok_or("log missing")?.len(), 1); // No duplicate entry.
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Reflog {
    /// Leave existing logs unchanged and create none, including on deletion.
    Preserve,
    /// Remove the edited name's log after deleting its reference. Valid only for deletion.
    /// Resolved deletion removes only the terminal log, preserving symbolic aliases' logs.
    Delete,
    /// Create a missing log or append to an existing log, even for an unchanged ID.
    ///
    /// Resolved operations log every name in the symbolic chain, using the terminal old/new IDs.
    /// Deletion appends a zero new ID and retains the file. Environment, hooks and Git config are
    /// never consulted. Existing files bytes are preserved without interpreting older records.
    /// A nonempty unterminated tail refuses preparation so the new record cannot be hidden inside
    /// an earlier message; the caller must inspect and repair it explicitly. Stored symbolic edits
    /// log only the edited name, resolving old and new terminal IDs under locks.
    Append {
        /// Validated like commit construction, additionally requiring nonnegative seconds.
        committer: Signature,
        /// Exact bytes; NUL, CR and LF are rejected. No whitespace normalization is performed.
        message: Vec<u8>,
    },
    /// Append only when the edited destination's stored target changes under transaction locks.
    ///
    /// Uses the same identity, terminal old/new IDs and logging scope as [`Self::Append`]. The
    /// comparison uses the stored target, including whether it is symbolic: replacing symbolic
    /// `HEAD` with its current terminal ID appends, while repeating that direct ID does not.
    /// For a dereferenced edit, the comparison applies to the terminal destination.
    ///
    /// Expectations and symbolic chains are still checked. Files transactions acquire the same
    /// reference and log locks even when skipping, but do not read or validate the skipped log's
    /// existing bytes. An absent log stays absent; malformed or unterminated existing bytes stay
    /// untouched. The supplied identity and message are always validated. A skipped operation has
    /// no log outcomes; reference publication follows the ordinary transaction contract.
    AppendIfChanged {
        /// Validated like commit construction, additionally requiring nonnegative seconds.
        committer: Signature,
        /// Exact bytes; NUL, CR and LF are rejected. No whitespace normalization is performed.
        message: Vec<u8>,
    },
    /// Append to existing logs only when the edited destination's stored target changes.
    ///
    /// Uses the stored-target comparison, identity validation, terminal IDs and logging scope of
    /// [`Self::AppendIfChanged`]. Each affected log's presence is checked under its transaction
    /// lock: a regular files-backend log, including an empty file, counts as present; a reftable
    /// log exists when the locked snapshot contains live records for its name. No missing log is
    /// created, including when creating a reference for the first time.
    ///
    /// Unchanged targets leave existing bytes untouched without validating their tail. Changed
    /// targets append only to present logs, with the ordinary append validation. Expectations and
    /// reference/log locks are still enforced when skipping; skipped logs have no log outcomes.
    /// This does not consult Git configuration or coordinate with writers that ignore these locks.
    AppendExistingIfChanged {
        /// Validated like commit construction, additionally requiring nonnegative seconds.
        committer: Signature,
        /// Exact bytes; NUL, CR and LF are rejected. No whitespace normalization is performed.
        message: Vec<u8>,
    },
}

impl Reflog {
    pub(super) fn append_fields(&self) -> Option<(&Signature, &[u8])> {
        match self {
            Self::Append { committer, message }
            | Self::AppendIfChanged { committer, message }
            | Self::AppendExistingIfChanged { committer, message } => Some((committer, message)),
            Self::Preserve | Self::Delete => None,
        }
    }
}

impl ReflogEntry {
    /// Parses a complete newline-terminated reflog, returning records in file order.
    ///
    /// The caller selects the repository format, including for empty input; every ID must match.
    /// Imported names may be empty or padded; trailing name whitespace is trimmed in the
    /// interpreted value. Signed `i64` seconds and four-digit signed zones are accepted, including
    /// noncanonical minutes such as `+0060`. UTF-8 is not required. Messages may contain tabs.
    /// Use [`ReflogRecord`] for exact lexical retention. Short zones, suffix text, and seconds
    /// outside `i64` are explicit reader limits; they are not claims of Git invalidity.
    ///
    /// # Errors
    ///
    /// Rejects malformed or foreign-format IDs, identity/date framing, invalid identities, NUL/CR
    /// messages, and truncated records. Errors identify the format rule; filesystem readers add
    /// the log path.
    pub fn parse(format: crate::ObjectFormat, bytes: &[u8]) -> Result<Vec<Self>, ReferenceError> {
        parse(format, bytes, &std::path::PathBuf::from("<reflog>"))
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, ReferenceError> {
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
    /// Reports whether a reflog exists without reading its complete history.
    ///
    /// Files storage rejects symlinks and non-regular log paths. Reftable reads a bounded
    /// snapshot; absence means no live log records. This is a live observation, not a lock or a
    /// promise that a subsequent append will succeed.
    ///
    /// # Errors
    ///
    /// Returns unsafe path, filesystem or reftable snapshot failures.
    pub fn has_reflog(&self, name: &RefName) -> Result<bool, ReferenceError> {
        if self.reference_backend == super::Backend::Reftable {
            return Ok(super::reftable::backend::reflog(self, name)?.is_some());
        }
        let path = self.reflog_path(name)?;
        super::store::check_path(&path)?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(super::store::io_error(&path, error)),
        }
    }

    /// Reads a complete reflog in oldest-to-newest order, or returns `None` if absent.
    ///
    /// Reftable uses a bounded snapshot, omits tombstones and strips one trailing message newline.
    /// It returns `None` when no live log records remain. Unsigned timestamps beyond `i64` fail
    /// interpretation; use [`super::reftable::Snapshot`] for lossless access. The remaining
    /// live-file details below describe the files backend.
    ///
    /// Uses the same worktree routing as references. Reads are live and may see an incomplete
    /// append by another writer. Prefer [`Self::imported_reflog`] for bounded recoverable reads.
    /// Memory is proportional to the whole log; this reader does not stream records or expire
    /// history. [`crate::Repository::expire_reflogs`] separately expires eligible files or
    /// reftable records under caller-owned maintenance isolation on Unix.
    ///
    /// # Errors
    ///
    /// Reports filesystem, symlink, and malformed-record errors, including a truncated tail.
    ///
    /// ```
    /// # use girt::refs::{Expected, RefEdit, RefName, Reflog, Target};
    /// # use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};
    /// # let directory = tempfile::tempdir()?;
    /// # let repo = Repository::init(ObjectFormat::Sha1, directory.path().join("r.git"), InitKind::Bare)?;
    /// # let loose = repo.loose_objects();
    /// # let person = Signature { name: b"A. Writer".to_vec(), email: b"a@example.com".to_vec(),
    /// #                          seconds: 1_700_000_000, offset_minutes: 0 };
    /// # let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
    /// # let id = loose.write_commit(&Commit::new(CommitFields {
    /// #     tree, parents: vec![], author: person.clone(), committer: person.clone(),
    /// #     extra_headers: vec![], message: b"Initial snapshot\n".to_vec(),
    /// # })?)?;
    /// # let refs = repo.references()?;
    /// # let main = RefName::new("refs/heads/main")?;
    /// # let edit = RefEdit::new(main.clone(), Some(Target::Direct(id)), Expected::Absent,
    /// #     Reflog::Append { committer: person, message: b"commit (initial)".to_vec() });
    /// # refs.transaction(&[edit])?;
    /// let log = refs.reflog(&main)?.ok_or("log missing")?;
    /// assert!(log[0].old.is_null()); // Zero marks creation, not a stored empty object.
    /// assert_eq!(log[0].new, id);
    /// assert_eq!(log[0].committer.seconds, 1_700_000_000);
    /// assert_eq!(log[0].message, b"commit (initial)");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn reflog(&self, name: &RefName) -> Result<Option<Vec<ReflogEntry>>, ReferenceError> {
        if self.reference_backend == super::Backend::Reftable {
            return super::reftable::backend::reflog(self, name);
        }
        let path = self.reflog_path(name)?;
        read_optional(&path)?
            .map(|bytes| parse(self.object_format, &bytes, &path))
            .transpose()
    }

    pub(crate) fn reflog_path(&self, name: &RefName) -> Result<PathBuf, ReferenceError> {
        let path = self.path(name)?;
        let root = if name.per_worktree() {
            self.git_dir
        } else {
            self.common_dir
        };
        #[expect(
            clippy::unwrap_used,
            reason = "the reference path was joined onto this root"
        )]
        Ok(root.join("logs").join(path.strip_prefix(root).unwrap()))
    }
}

// Inspect only framing at the append boundary; imported grammar does not constrain construction.
pub(super) fn check_append_tail(path: &std::path::Path) -> Result<(), ReferenceError> {
    check_append_tail_with::<false>(path, usize::MAX).map(|_| ())
}

pub(super) fn check_append_tail_controlled(
    path: &std::path::Path,
    max_bytes: usize,
) -> Result<usize, ReferenceError> {
    check_append_tail_with::<true>(path, max_bytes)
}

fn check_append_tail_with<const CONTROLLED: bool>(
    path: &std::path::Path,
    max_bytes: usize,
) -> Result<usize, ReferenceError> {
    use std::io::{Read, Seek, SeekFrom};
    super::store::check_path(path)?;
    let mut file = if CONTROLLED {
        let Some(file) = super::enumerate::open_walked(path)? else {
            return Ok(0);
        };
        file
    } else {
        match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(super::store::io_error(path, error)),
        }
    };
    let size = file
        .seek(SeekFrom::End(0))
        .map_err(|error| super::store::io_error(path, error))?;
    if size == 0 {
        return Ok(0);
    }
    if CONTROLLED && max_bytes == 0 {
        return Err(ReferenceError::Limit("reference bytes"));
    }
    file.seek(SeekFrom::End(-1))
        .map_err(|error| super::store::io_error(path, error))?;
    let mut last = [0];
    file.read_exact(&mut last)
        .map_err(|error| super::store::io_error(path, error))?;
    if last[0] != b'\n' {
        return Err(malformed(
            path,
            "unterminated reflog append tail; inspect and repair before retry",
        ));
    }
    Ok(1)
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
    let field = |range: std::ops::Range<usize>| {
        line.get(range)
            .ok_or_else(|| malformed(path, "expected two repository-format reflog IDs"))
    };
    let old = parse_id(field(0..width)?)?;
    let new = parse_id(field(width + 1..2 * width + 1)?)?;
    let rest = field(2 * width + 2..line.len())?;
    let (identity, message) = crate::bytes::split_once(rest, b'\t').unwrap_or((rest, &[]));
    let date = crate::IdentityRef::parse(identity)
        .map_err(|_| malformed(path, "invalid reflog identity"))?
        .date_bytes;
    let zone = date
        .trim_ascii_start()
        .splitn(2, |&b| b == b' ')
        .nth(1)
        .unwrap_or_default();
    if !date.first().is_some_and(u8::is_ascii_whitespace)
        || !matches!(zone, [b'+' | b'-', digits @ ..]
            if digits.len() == 4 && digits.iter().all(u8::is_ascii_digit))
    {
        return Err(malformed(path, "invalid reflog date framing"));
    }
    let committer = crate::IdentityRef::parse(identity)
        .and_then(crate::IdentityRef::signature)
        .map_err(|_| {
            malformed(
                path,
                "reflog identity or numeric date exceeds reader limits",
            )
        })?;
    let message = message.to_vec();
    if identity.contains(&0)
        || identity.contains(&b'\r')
        || message.iter().any(|b| matches!(b, 0 | b'\r'))
    {
        return Err(malformed(path, "NUL or CR in reflog record"));
    }
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
    #[case::cr_name(b"A\r <a@b> 1 +0000\tmessage\n")]
    #[case::nul(b"A <a@b> 1 +0000\tx\0y\n")]
    #[case::cr(b"A <a@b> 1 +0000\tx\ry\n")]
    #[case::missing_separator(b"A <a@b> 1 +0000 message\n")]
    #[case::sign_only_zone(b"A <a@b> 1 +\tmessage\n")]
    #[case::short_zone(b"A <a@b> 1 +000\tmessage\n")]
    #[case::text_zone(b"A <a@b> 1 +00x0\tmessage\n")]
    #[case::no_identity(b"")]
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
