//! Split patch mailboxes without decoding or publishing their messages.
//!
//! [`split`] preserves each Unix `From ` envelope and all body bytes except CRLF line endings,
//! which become LF by default. Escaped `>From` lines stay escaped for the later mail decoder.
//! [`split_with`] also accepts a bare RFC message when requested; that mode returns the entire
//! bare input as one message. Leading spaces, tabs, CR and LF before the first message are
//! discarded. Filesystem traversal, Maildir ordering and output names remain caller-owned.
//!
//! ```
//! let input = b"From writer Mon Sep 17 00:00:00 2001\nSubject: Patch\n\nBody\n";
//! let messages = girt::mailbox::split(input)?;
//! assert_eq!(messages, vec![input.to_vec()]);
//! # Ok::<(), girt::mailbox::Error>(())
//! ```

use std::sync::atomic::{AtomicBool, Ordering};

/// How message boundaries and line endings are interpreted.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct Options {
    /// Accept a first message without a Unix envelope; its entire body stays one message.
    pub allow_bare: bool,
    /// Preserve CRLF instead of replacing each CRLF with LF.
    pub keep_cr: bool,
}

impl Options {
    /// Sets [`Self::allow_bare`].
    #[must_use]
    pub fn with_allow_bare(mut self, value: bool) -> Self {
        self.allow_bare = value;
        self
    }

    /// Sets [`Self::keep_cr`].
    #[must_use]
    pub fn with_keep_cr(mut self, value: bool) -> Self {
        self.keep_cr = value;
        self
    }
}

/// Admission bounds for one split operation, independent of allocator failure or elapsed time.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Entire supplied input, including discarded initial whitespace (default 64 MiB).
    pub max_input_bytes: usize,
    /// Returned messages (default 1000).
    pub max_messages: usize,
    /// Bytes in one normalized message (default 16 MiB).
    pub max_message_bytes: usize,
    /// Sum of normalized message bytes (default 64 MiB).
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 * 1024 * 1024,
            max_messages: 1000,
            max_message_bytes: 16 * 1024 * 1024,
            max_output_bytes: 64 * 1024 * 1024,
        }
    }
}

impl Limits {
    /// Sets [`Self::max_input_bytes`].
    #[must_use]
    pub fn with_max_input_bytes(mut self, value: usize) -> Self {
        self.max_input_bytes = value;
        self
    }

    /// Sets [`Self::max_messages`].
    #[must_use]
    pub fn with_max_messages(mut self, value: usize) -> Self {
        self.max_messages = value;
        self
    }

    /// Sets [`Self::max_message_bytes`].
    #[must_use]
    pub fn with_max_message_bytes(mut self, value: usize) -> Self {
        self.max_message_bytes = value;
        self
    }

    /// Sets [`Self::max_output_bytes`].
    #[must_use]
    pub fn with_max_output_bytes(mut self, value: usize) -> Self {
        self.max_output_bytes = value;
        self
    }
}

/// A mailbox cannot be split within the requested policy and bounds.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No non-whitespace message bytes were supplied.
    #[error("empty mailbox")]
    Empty,
    /// The first non-whitespace line is not an envelope and bare mode is disabled.
    #[error("mailbox does not start with a Unix From envelope")]
    MissingEnvelope,
    /// A named admission bound was exceeded.
    #[error("mailbox exceeds {0} limit")]
    Limit(&'static str),
    /// The caller requested cancellation.
    #[error("mailbox splitting cancelled")]
    Cancelled,
}

/// Splits an enveloped mailbox with default bounds and CRLF normalization.
///
/// # Errors
///
/// Returns [`Error::Empty`], [`Error::MissingEnvelope`] or [`Error::Limit`] without partial output.
pub fn split(input: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    split_with(
        input,
        Options::default(),
        Limits::default(),
        &AtomicBool::new(false),
    )
}

/// Splits mailbox bytes using explicit policy, bounds and cancellation.
///
/// Date-shaped Unix envelopes begin with `From ` and use Git's structural clock/year heuristic;
/// weekday/month names and calendar ranges are not validated. A recognized envelope in the body
/// starts another message; escaped `>From` lines do not. Bare mode only changes input lacking an
/// initial envelope. No MIME, header or transfer decoding occurs, and missing final newlines stay
/// missing. Work and retained output are linear in admitted input bytes; cancellation is checked
/// per line.
///
/// # Errors
///
/// Returns the same errors as [`split`], plus [`Error::Cancelled`]. Every failure discards the
/// accumulated messages. This function performs no filesystem operations.
pub fn split_with(
    input: &[u8],
    options: Options,
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Vec<Vec<u8>>, Error> {
    check(cancel)?;
    if input.len() > limits.max_input_bytes {
        return Err(Error::Limit("input bytes"));
    }
    let start = input
        .iter()
        .position(|byte| !separator(byte))
        .unwrap_or(input.len());
    let input = &input[start..];
    if input.is_empty() {
        return Err(Error::Empty);
    }
    let first = input
        .split_inclusive(|&byte| byte == b'\n')
        .next()
        .unwrap_or(input);
    let bare = !envelope(first);
    if bare && !options.allow_bare {
        return Err(Error::MissingEnvelope);
    }
    let mut messages = Vec::new();
    let mut message = Vec::new();
    let mut total = 0usize;
    for line in input.split_inclusive(|&byte| byte == b'\n') {
        check(cancel)?;
        if !bare && !message.is_empty() && envelope(line) {
            messages.push(std::mem::take(&mut message));
        }
        if messages.len() >= limits.max_messages {
            return Err(Error::Limit("messages"));
        }
        let normalized = if !options.keep_cr && line.ends_with(b"\r\n") {
            &line[..line.len() - 2]
        } else {
            line
        };
        let newline = normalized.len() != line.len();
        let size = normalized.len() + usize::from(newline);
        if size > limits.max_message_bytes.saturating_sub(message.len()) {
            return Err(Error::Limit("message bytes"));
        }
        if size > limits.max_output_bytes.saturating_sub(total) {
            return Err(Error::Limit("output bytes"));
        }
        message.extend_from_slice(normalized);
        if newline {
            message.push(b'\n');
        }
        total += size;
    }
    check(cancel)?;
    messages.push(message);
    Ok(messages)
}

fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn separator(byte: &u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn envelope(line: &[u8]) -> bool {
    if line.len() < 20 || !line.starts_with(b"From ") {
        return false;
    }
    let Some(colon) = line.iter().rposition(|&byte| byte == b':') else {
        return false;
    };
    // The CLI recognizes the last clock fragment, not a calendar date. Its preceding
    // separator need not be whitespace (for example 12x34:56 is accepted).
    if colon < 4 || colon + 2 >= line.len() {
        return false;
    }
    if ![colon - 4, colon - 2, colon - 1, colon + 1, colon + 2]
        .into_iter()
        .all(|index| line[index].is_ascii_digit())
    {
        return false;
    }
    let suffix = &line[colon + 3..];
    let start = suffix
        .iter()
        .position(|byte| !byte.is_ascii_whitespace() && *byte != 0x0b)
        .unwrap_or(suffix.len());
    year(&suffix[start..])
}

fn year(field: &[u8]) -> bool {
    let field = field.strip_prefix(b"+").unwrap_or(field);
    let mut value = 0u8;
    for byte in field.iter().take_while(|byte| byte.is_ascii_digit()) {
        value = value.saturating_mul(10).saturating_add(byte - b'0');
        if value > 90 {
            return true;
        }
    }
    false
}
