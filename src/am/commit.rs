use std::sync::atomic::{AtomicBool, Ordering};

use super::State;
use crate::{Commit, ObjectId, Signature};

/// Admission bound for mail artifacts and the caller-provided committer identity.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct CommitLimits {
    /// Combined author-script, final-message, committer-name/email bytes (default 1 MiB).
    pub max_metadata_bytes: usize,
}
impl Default for CommitLimits {
    fn default() -> Self {
        Self {
            max_metadata_bytes: 1024 * 1024,
        }
    }
}
impl CommitLimits {
    /// Sets [`Self::max_metadata_bytes`].
    #[must_use]
    pub fn with_max_metadata_bytes(mut self, value: usize) -> Self {
        self.max_metadata_bytes = value;
        self
    }
}

/// Mail metadata cannot produce an unsigned commit under the admitted policy.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PrepareError {
    /// Required current mail or prepared artifact is absent.
    #[error("am commit metadata is incomplete: {0}")]
    Missing(&'static str),
    /// The author script is outside Git's fixed assignment/quoting syntax.
    #[error("unsupported am author-script syntax")]
    Script,
    /// Only canonical numeric-zone mail dates without comments or leap seconds are supported.
    #[error("unsupported am author date")]
    DateSyntax,
    /// An admitted canonical date is not a valid calendar instant.
    #[error("invalid am author date")]
    Date(#[source] jiff::Error),
    /// Aggregate metadata exceeds the caller's bound.
    #[error("am commit metadata limit exceeded")]
    Limit,
    /// Cancellation was observed; no commit is returned.
    #[error("am commit preparation cancelled")]
    Cancelled,
    /// Proposed tree/parent formats or constructed identities/headers are invalid.
    #[error("constructing am commit")]
    Commit(#[from] crate::CommitError),
}

impl State<'_> {
    /// Constructs an unsigned commit without installing objects or changing repository state.
    ///
    /// The caller supplies a tree identity from its staged snapshot, an explicit parent (or `None`
    /// for a root), and a committer. Tree/parent existence and agreement with the current index or
    /// HEAD are not checked. This proposal is not a resume/publication authorization.
    ///
    /// Author identity/date comes from Git's three single-quoted `author-script` assignments;
    /// apostrophe/exclamation escapes are decoded as data, never evaluated by a shell. Extra
    /// assignments, commands and alternate quoting are refused. Dates must use the canonical
    /// numeric-zone mail form, with four-digit years and seconds below 60; comments and
    /// relative dates are refused. The exact `final-commit` bytes become the message. No hooks,
    /// signing, cleanup, encoding conversion, empty-commit policy, environment lookup or
    /// mail-range advancement occurs.
    ///
    /// ```no_run
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::am::{CommitLimits, State};
    /// use girt::{Commit, ObjectId, Signature};
    ///
    /// fn propose(state: &State<'_>, staged_tree: ObjectId, parent: ObjectId) -> girt::Result<Commit> {
    ///     let committer = Signature::parse(b"Recorder <r@example.com> 1000684800 +0000")?;
    ///     Ok(state.prepare_commit(
    ///         staged_tree,
    ///         Some(parent),
    ///         committer,
    ///         CommitLimits::default(),
    ///         &AtomicBool::new(false),
    ///     )?)
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`PrepareError`] for absent artifacts/current mail, unsupported/malformed script or
    /// date, invalid commit metadata, admission or cancellation. Input bounds precede copying;
    /// parsing work is linear in admitted metadata and date parsing is capped at 64 bytes.
    pub fn prepare_commit(
        &self,
        tree: ObjectId,
        parent: Option<ObjectId>,
        committer: Signature,
        limits: CommitLimits,
        cancel: &AtomicBool,
    ) -> Result<Commit, PrepareError> {
        check(cancel)?;
        self.mail.ok_or(PrepareError::Missing("current mail"))?;
        let script = self
            .author_script
            .ok_or(PrepareError::Missing("author-script"))?;
        let message = self.message.ok_or(PrepareError::Missing("final-commit"))?;
        let mut remaining = limits.max_metadata_bytes;
        for bytes in [script, message, &committer.name, &committer.email] {
            remaining = remaining
                .checked_sub(bytes.len())
                .ok_or(PrepareError::Limit)?;
        }
        let (name, input) = assignment(script, b"GIT_AUTHOR_NAME", cancel)?;
        let (email, input) = assignment(input, b"GIT_AUTHOR_EMAIL", cancel)?;
        let (date, input) = assignment(input, b"GIT_AUTHOR_DATE", cancel)?;
        if !input.is_empty() {
            return Err(PrepareError::Script);
        }
        let date = date_value(&date)?;
        let author = Signature {
            name,
            email,
            seconds: date.timestamp().as_second(),
            offset_minutes: (date.offset().seconds() / 60)
                .try_into()
                .map_err(|_| PrepareError::DateSyntax)?,
        };
        check(cancel)?;
        let commit = Commit::builder(tree, author, message)
            .parents(parent)
            .committer(committer)
            .build()?;
        check(cancel)?;
        Ok(commit)
    }
}

fn assignment<'a>(
    input: &'a [u8],
    key: &[u8],
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, &'a [u8]), PrepareError> {
    let mut input = input
        .strip_prefix(key)
        .and_then(|input| input.strip_prefix(b"='"))
        .ok_or(PrepareError::Script)?;
    let mut value = Vec::new();
    loop {
        check(cancel)?;
        let end = input
            .iter()
            .position(|&byte| byte == b'\'')
            .ok_or(PrepareError::Script)?;
        value.extend_from_slice(&input[..end]);
        input = &input[end + 1..];
        if input.len() >= 3
            && input[0] == b'\\'
            && input[2] == b'\''
            && matches!(input[1], b'\'' | b'!')
        {
            value.push(input[1]);
            input = &input[3..];
        } else {
            let rest = input.strip_prefix(b"\n").ok_or(PrepareError::Script)?;
            return Ok((value, rest));
        }
    }
}

fn date_value(bytes: &[u8]) -> Result<jiff::Zoned, PrepareError> {
    if bytes.len() > 64 || !bytes.is_ascii() {
        return Err(PrepareError::DateSyntax);
    }
    let fields: Vec<_> = bytes
        .split(|&byte| byte == b' ')
        .filter(|field| !field.is_empty())
        .collect();
    let [weekday, day, month, year, time, zone] = fields.as_slice() else {
        return Err(PrepareError::DateSyntax);
    };
    let digits = |bytes: &[u8]| bytes.iter().all(u8::is_ascii_digit);
    if weekday.len() != 4
        || weekday[3] != b','
        || !(1..=2).contains(&day.len())
        || !digits(day)
        || month.len() != 3
        || year.len() != 4
        || !digits(year)
        || time.len() != 8
        || time[2] != b':'
        || time[5] != b':'
        || !digits(&time[..2])
        || !digits(&time[3..5])
        || !digits(&time[6..])
        || time[6] >= b'6'
        || zone.len() != 5
        || !matches!(zone[0], b'+' | b'-')
        || !digits(&zone[1..])
    {
        return Err(PrepareError::DateSyntax);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| PrepareError::DateSyntax)?;
    jiff::fmt::rfc2822::parse(text).map_err(PrepareError::Date)
}
fn check(cancel: &AtomicBool) -> Result<(), PrepareError> {
    if cancel.load(Ordering::Relaxed) {
        Err(PrepareError::Cancelled)
    } else {
        Ok(())
    }
}
