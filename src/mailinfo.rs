//! Extract commit metadata, a log message and patch bytes from one patch mail.
//!
//! Pass a complete message from [`crate::mailbox::split`] to [`parse`]. Headers are unfolded and
//! RFC 2047 Q/B words decoded; subject cleanup removes reply and bracket prefixes. [`parse_with`]
//! selects subject retention, scissors and Message-ID trailers. This layer writes no files and
//! does not validate or apply the extracted patch.
//!
//! Single-part UTF-8/ASCII text mail supports 7bit, 8bit, binary, base64 and quoted-printable
//! transfer encodings. RFC 2047 metadata additionally supports Latin-1. Other character sets and
//! multipart MIME are explicit refusals. Scissors accepts standard dashed cut lines; other
//! marker-bearing lines are refused until broader heuristic compatibility is implemented. Bounds
//! cover raw input, headers, decoded content and each returned artifact. No partial result is
//! returned on failure.
//!
//! ```
//! let mail = b"From: A Writer <a@example.com>\nSubject: [PATCH] Fix\n\nBody\n---\npatch\n";
//! let info = girt::mailinfo::parse(mail)?;
//! assert_eq!(info.subject.as_deref(), Some("Fix"));
//! assert_eq!(info.message, b"Body\n");
//! assert_eq!(info.patch, b"---\npatch\n");
//! # Ok::<(), girt::mailinfo::Error>(())
//! ```

mod encoding;
mod extract;
mod types;

pub use extract::{parse, parse_with};
pub use types::{Error, Limits, Message, Options, Subject};
