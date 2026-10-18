/// Which subject prefixes to retain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Subject {
    /// Remove reply and bracket prefixes and collapse whitespace.
    #[default]
    Clean,
    /// Preserve the decoded subject (`mailinfo -k`).
    Keep,
    /// Preserve brackets that do not contain PATCH (`mailinfo -b`).
    KeepNonPatch,
}

/// Caller-owned mail extraction policy; no configuration is consulted.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct Options {
    /// Subject prefix handling.
    pub subject: Subject,
    /// Discard log text preceding a standard dashed scissors line.
    /// Other marker-bearing forms are explicitly refused.
    pub scissors: bool,
    /// Append `Message-ID: ...` to the log message when a patch is found.
    pub message_id: bool,
}
impl Options {
    /// Sets [`Self::subject`].
    #[must_use]
    pub fn with_subject(mut self, value: Subject) -> Self {
        self.subject = value;
        self
    }
    /// Sets [`Self::scissors`].
    #[must_use]
    pub fn with_scissors(mut self, value: bool) -> Self {
        self.scissors = value;
        self
    }
    /// Sets [`Self::message_id`].
    #[must_use]
    pub fn with_message_id(mut self, value: bool) -> Self {
        self.message_id = value;
        self
    }
}

/// Independent byte bounds; work is linear in admitted input, excluding allocator failure.
///
/// Decoded-output bounds govern artifacts, not peak scratch allocation: transfer decoding may
/// temporarily retain input-sized buffers before checking the decoded bound. Raw input/header
/// bounds also constrain that temporary memory.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Supplied message bytes (default 64 MiB).
    pub max_input_bytes: usize,
    /// Raw outer and in-body header bytes, combined (default 1 MiB).
    pub max_header_bytes: usize,
    /// Outer and in-body fields, combined (default 1000).
    pub max_headers: usize,
    /// Transfer-decoded body plus decoded metadata bytes (default 64 MiB).
    pub max_decoded_bytes: usize,
    /// Returned log bytes, including Message-ID trailer (default 16 MiB).
    pub max_message_bytes: usize,
    /// Returned patch bytes (default 64 MiB).
    pub max_patch_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 << 20,
            max_header_bytes: 1 << 20,
            max_headers: 1000,
            max_decoded_bytes: 64 << 20,
            max_message_bytes: 16 << 20,
            max_patch_bytes: 64 << 20,
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
    /// Sets [`Self::max_header_bytes`].
    #[must_use]
    pub fn with_max_header_bytes(mut self, value: usize) -> Self {
        self.max_header_bytes = value;
        self
    }
    /// Sets [`Self::max_headers`].
    #[must_use]
    pub fn with_max_headers(mut self, value: usize) -> Self {
        self.max_headers = value;
        self
    }
    /// Sets [`Self::max_decoded_bytes`].
    #[must_use]
    pub fn with_max_decoded_bytes(mut self, value: usize) -> Self {
        self.max_decoded_bytes = value;
        self
    }
    /// Sets [`Self::max_message_bytes`].
    #[must_use]
    pub fn with_max_message_bytes(mut self, value: usize) -> Self {
        self.max_message_bytes = value;
        self
    }
    /// Sets [`Self::max_patch_bytes`].
    #[must_use]
    pub fn with_max_patch_bytes(mut self, value: usize) -> Self {
        self.max_patch_bytes = value;
        self
    }
}

/// Decoded mail metadata and unpublished output artifacts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Sender display name, when a From field is present.
    pub author: Option<String>,
    /// Sender address, when a From field is present.
    pub email: Option<String>,
    /// Decoded subject after the selected cleanup policy.
    pub subject: Option<String>,
    /// Original unfolded date text; date interpretation remains caller-owned.
    pub date: Option<String>,
    /// Unfolded Message-ID, whether or not its trailer was requested.
    pub message_id: Option<String>,
    /// Log body with leading blank lines removed and optional Message-ID trailer.
    pub message: Vec<u8>,
    /// Patch preamble and all subsequent bytes, including a signature if present.
    pub patch: Vec<u8>,
}

/// Mail extraction stopped without returning partial metadata or artifacts.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A named byte or field-count bound was exceeded.
    #[error("patch mail exceeds {0} limit")]
    Limit(&'static str),
    /// A MIME feature or character set is outside this parser's supported scope.
    #[error("unsupported patch mail {0}")]
    Unsupported(&'static str),
    /// Metadata or a transfer-encoded value is malformed.
    #[error("invalid patch mail {0}")]
    Malformed(&'static str),
    /// The caller requested cancellation.
    #[error("patch mail extraction cancelled")]
    Cancelled,
}
