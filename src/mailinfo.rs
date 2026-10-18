//! Extract commit metadata, a log message and patch bytes from one patch mail.
//!
//! Pass a complete message from [`crate::mailbox::split`] to [`parse`]. Headers are unfolded and
//! RFC 2047 Q/B words decoded; subject cleanup removes reply and bracket prefixes. [`parse_with`]
//! selects subject retention, scissors and Message-ID trailers. This layer writes no files and
//! does not validate or apply the extracted patch.
//!
//! Plain and multipart mail supports 7bit, 8bit, binary, base64 and quoted-printable transfer
//! encodings. Leaf content stays in wire order, including HTML and opaque attachments. UTF-8/ASCII
//! body character sets and RFC 2047 UTF-8/ASCII/Latin-1 metadata are supported; other character
//! sets, encoded multipart containers and `message/rfc822` containers are explicit refusals. MIME
//! nesting uses an explicit stack with part/depth bounds. Logical part boundaries stay separate
//! even when a decoded part has no final LF; no separator byte is invented between parts.
//!
//! Scissors uses Git's structural cut-line recognition; marker-bearing text that does not qualify
//! stays in the log message. Bounds cover raw input, headers, decoded content and each returned
//! artifact. No partial result is returned on failure.
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
mod headers;
mod mime;
mod types;

pub use extract::{parse, parse_with};
pub use types::{Error, Limits, Message, Options, Subject};

fn check(cancel: &std::sync::atomic::AtomicBool) -> Result<(), Error> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
