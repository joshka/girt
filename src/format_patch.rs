//! Deterministic format-patch mail framing for a commit and its prepared diff artifacts.
//!
//! [`Objects::format_patch`](crate::Objects::format_patch) reads an explicit oldest-first commit
//! selection and composes matching patches and statistics. [`Series::into_mbox`] assembles the
//! returned messages without publishing them. [`CoverLetter`] supplies explicit cover author/date
//! and message; [`SeriesOptions`] controls numbering, content policy and optional attributes.
//! Merge/gapped selections and distant bases needing prerequisite IDs are refused explicitly.
//!
//! [`Message`] borrows a commit, its [`Diffstat`] and patch bytes. It does not inspect storage or
//! verify that those artifacts describe the commit: the caller owns comparison and attributes.
//! Rendering returns a complete owned message before any caller publishes it. Dates come from
//! commit metadata; options, signature and numbering are explicit, with no process configuration.
//!
//! The envelope uses Git's fixed mbox date. Header names and subjects use UTF-8 RFC 2047 Q encoding
//! when needed, while the body and patch retain their bytes. Invalid UTF-8 metadata and header
//! injection are refused. The patch bytes must already use mail-suitable LF record framing.
//!
//! ```
//! use girt::diffstat::{Counts, Diffstat, FileStat};
//! use girt::format_patch::Message;
//! use girt::{Commit, EntryMode, ObjectFormat, ObjectId, Signature, Tree, TreeEntry};
//!
//! let author = Signature {
//!     name: b"Patch Author".to_vec(),
//!     email: b"author@example.com".to_vec(),
//!     seconds: 1_000_000_000,
//!     offset_minutes: 0,
//! };
//! let tree = Tree::new(
//!     ObjectFormat::Sha1,
//!     vec![TreeEntry {
//!         mode: EntryMode::Blob,
//!         name: b"hello".to_vec(),
//!         id: ObjectId::for_blob(ObjectFormat::Sha1, b"Hello\n"),
//!     }],
//! )?;
//! let commit = Commit::builder(tree.id(), author, b"Add greeting\n").build()?;
//! let stat = Diffstat::new(
//!     vec![FileStat {
//!         path: b"hello".to_vec(),
//!         source_path: None,
//!         counts: Counts::Text {
//!             added: 1,
//!             removed: 0,
//!         },
//!     }],
//!     Default::default(),
//! )?;
//! let patch = b"diff --git a/hello b/hello\nnew file mode 100644\n\
//!     --- /dev/null\n+++ b/hello\n@@ -0,0 +1 @@\n+Hello\n";
//!
//! // Comparison is caller-owned; the formatter does not install or inspect this commit.
//! let mail = Message::new(&commit, &stat, patch)
//!     .with_change_summary(b" create mode 100644 hello\n")
//!     .render()?;
//! assert!(String::from_utf8(mail)?.contains("Subject: [PATCH] Add greeting\n"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
use std::sync::atomic::{AtomicBool, Ordering};

mod series;
pub use series::{CoverLetter, Numbering, Series, SeriesError, SeriesLimits, SeriesOptions};

use crate::diffstat::{Diffstat, StatLayout};
use crate::{Commit, ObjectId};

/// A commit and caller-prepared artifacts, borrowed until rendering completes.
#[derive(Debug)]
pub struct Message<'a> {
    commit: &'a Commit,
    stat: &'a Diffstat,
    patch: &'a [u8],
    summary: &'a [u8],
}

/// Explicit presentation of one numbered message in a series.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Position {
    /// One-based patch number; zero is reserved for a cover letter.
    pub number: usize,
    /// Total patches, excluding the cover letter.
    pub total: usize,
}

impl Position {
    /// Constructs a one-based patch position within a nonempty series.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Position`] unless `1 <= number <= total`.
    pub fn new(number: usize, total: usize) -> Result<Self, Error> {
        if number == 0 || number > total {
            return Err(Error::Position);
        }
        Ok(Self { number, total })
    }
}

/// Mail presentation independent of environment and repository configuration.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Options<'a> {
    /// Subject prefix inside brackets; default `PATCH`. Empty omits an unnumbered bracket.
    /// Must be ASCII without CR, LF or NUL.
    pub subject_prefix: &'a [u8],
    /// Numbering inside brackets; `None` (the default) emits only the subject prefix.
    pub position: Option<Position>,
    /// Signature below `-- `; `None` omits the signature. Default identifies this girt version.
    pub signature: Option<&'a [u8]>,
    /// Explicit base identity, printed before the signature. Its ancestry is caller-owned.
    pub base: Option<ObjectId>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            subject_prefix: b"PATCH",
            position: None,
            signature: Some(concat!("girt ", env!("CARGO_PKG_VERSION")).as_bytes()),
            base: None,
        }
    }
}

impl<'a> Options<'a> {
    /// Sets [`Self::subject_prefix`].
    #[must_use]
    pub fn with_subject_prefix(mut self, prefix: &'a [u8]) -> Self {
        self.subject_prefix = prefix;
        self
    }

    /// Sets [`Self::position`]. Invalid positions are refused during rendering.
    #[must_use]
    pub fn with_position(mut self, number: usize, total: usize) -> Self {
        self.position = Some(Position { number, total });
        self
    }

    /// Sets [`Self::signature`].
    #[must_use]
    pub fn with_signature(mut self, signature: Option<&'a [u8]>) -> Self {
        self.signature = signature;
        self
    }

    /// Sets [`Self::base`].
    #[must_use]
    pub fn with_base(mut self, base: Option<ObjectId>) -> Self {
        self.base = base;
        self
    }
}

/// Independent bounds for borrowed metadata and the returned mail artifact.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Entire raw commit payload, prefix and signature bytes; default 1 MiB.
    pub max_metadata_bytes: usize,
    /// Complete returned mail bytes, including supplied patch/stat; default 64 MiB.
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_metadata_bytes: 1024 * 1024,
            max_output_bytes: 64 * 1024 * 1024,
        }
    }
}

impl Limits {
    /// Sets [`Self::max_metadata_bytes`].
    #[must_use]
    pub fn with_max_metadata_bytes(mut self, bytes: usize) -> Self {
        self.max_metadata_bytes = bytes;
        self
    }

    /// Sets [`Self::max_output_bytes`].
    #[must_use]
    pub fn with_max_output_bytes(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes;
        self
    }
}

/// Mail rendering failed without publishing any output or changing repository state.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Commit author metadata is malformed.
    #[error("reading patch mail author")]
    Commit(#[from] crate::CommitError),
    /// The supplied summary cannot be rendered under its own limits.
    #[error("rendering patch mail statistics")]
    Stat(#[from] crate::diffstat::Error),
    /// A named caller-selected bound was exceeded.
    #[error("patch mail exceeds {0} limit")]
    Limit(&'static str),
    /// A required author or subject is absent, invalid UTF-8, or contains header delimiters.
    #[error("invalid patch mail metadata: {0}")]
    Metadata(&'static str),
    /// A numbered patch must have 1 <= number <= total.
    #[error("invalid patch mail series position")]
    Position,
    /// The author date is outside the supported civil date range.
    #[error("patch mail author date is out of range")]
    Date,
    /// Cancellation was observed before a complete artifact was returned.
    #[error("patch mail rendering cancelled")]
    Cancelled,
}

impl<'a> Message<'a> {
    /// Borrows a commit and its prepared summary and patch. No validation or I/O occurs here.
    pub fn new(commit: &'a Commit, stat: &'a Diffstat, patch: &'a [u8]) -> Self {
        Self {
            commit,
            stat,
            patch,
            summary: b"",
        }
    }

    /// Supplies caller-prepared change-summary lines below diffstat, such as `create mode`.
    ///
    /// Bytes must already be LF-framed. No parsing or consistency validation occurs; stored
    /// series composition supplies these from the same comparison as the patch.
    #[must_use]
    pub fn with_change_summary(mut self, summary: &'a [u8]) -> Self {
        self.summary = summary;
        self
    }

    /// Renders an unnumbered message with default prefix, signature and bounds.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for invalid metadata, an unrenderable summary or exceeded bounds.
    pub fn render(&self) -> Result<Vec<u8>, Error> {
        self.render_with(
            Options::default(),
            Limits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Renders complete mbox bytes with explicit presentation, bounds and cancellation.
    ///
    /// The caller supplies a matching patch and summary; their generation limits remain separate.
    /// Summary rendering can retain up to its own output bound before mail output admission. Other
    /// temporary strings are linear in the admitted metadata; output growth is checked before
    /// each append. No partial artifact is returned on failure. Empty patches still receive mail
    /// framing; selecting or omitting empty commits belongs to the caller.
    ///
    /// # Errors
    ///
    /// Refuses missing or malformed author metadata, invalid UTF-8 names/messages, header
    /// injection, invalid series positions, mismatched base object formats, out-of-range dates,
    /// exceeded limits and cancellation. No repository state or external writer is changed.
    pub fn render_with(
        &self,
        options: Options<'_>,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, Error> {
        self.render_as(options, limits, cancel, MailKind::Patch)
    }

    fn render_as(
        &self,
        options: Options<'_>,
        limits: Limits,
        cancel: &AtomicBool,
        kind: MailKind,
    ) -> Result<Vec<u8>, Error> {
        let mut out = Output {
            bytes: Vec::new(),
            limit: limits.max_output_bytes,
            cancel,
        };
        out.check()?;
        let headers = prepare_headers(
            self.commit,
            options,
            limits,
            matches!(kind, MailKind::Cover { .. }),
        )?;
        let envelope = match kind {
            MailKind::Patch => self.commit.id(),
            MailKind::Cover { envelope, .. } => envelope,
        };
        out.append(format!("From {envelope} Mon Sep 17 00:00:00 2001\n").as_bytes())?;
        out.append(headers.from.as_bytes())?;
        out.append(format!("Date: {}\n", headers.date).as_bytes())?;
        out.append(headers.title.as_bytes())?;
        if headers.mime {
            out.append(b"MIME-Version: 1.0\nContent-Type: text/plain; charset=UTF-8\nContent-Transfer-Encoding: 8bit\n")?;
        }
        out.append(b"\n")?;
        let body = headers.body;
        if !body.is_empty() {
            out.append(body.as_bytes())?;
            if !body.ends_with('\n') {
                out.append(b"\n")?;
            }
        }
        if let MailKind::Cover { include_stat, .. } = kind {
            if include_stat {
                out.append(&self.stat.stat(StatLayout::default())?)?;
                out.append(b"\n")?;
            }
        } else if !self.patch.is_empty() || !self.stat.files().is_empty() {
            out.append(b"---\n")?;
            out.append(&self.stat.stat(StatLayout::default())?)?;
            out.append(self.summary)?;
            out.append(b"\n")?;
            out.append(self.patch)?;
        }
        if let Some(base) = options.base {
            out.append(format!("\nbase-commit: {base}\n").as_bytes())?;
        }
        if let Some(signature) = options.signature {
            out.append(b"-- \n")?;
            out.append(signature)?;
            if !signature.ends_with(b"\n") {
                out.append(b"\n")?;
            }
            out.append(b"\n")?;
        }
        out.check()?;
        Ok(out.bytes)
    }
}

enum MailKind {
    Patch,
    Cover {
        envelope: ObjectId,
        include_stat: bool,
    },
}

struct PreparedHeaders<'a> {
    from: String,
    date: String,
    title: String,
    body: &'a str,
    mime: bool,
}

fn prepare_headers<'a>(
    commit: &'a Commit,
    options: Options<'_>,
    limits: Limits,
    cover: bool,
) -> Result<PreparedHeaders<'a>, Error> {
    let size = [
        commit.as_bytes(),
        options.subject_prefix,
        options.signature.unwrap_or_default(),
    ]
    .iter()
    .try_fold(0usize, |size, bytes| size.checked_add(bytes.len()))
    .ok_or(Error::Limit("metadata bytes"))?;
    if size > limits.max_metadata_bytes {
        return Err(Error::Limit("metadata bytes"));
    }
    if let Some(encoding) = commit.header(b"encoding")
        && !encoding.value.eq_ignore_ascii_case(b"UTF-8")
    {
        return Err(Error::Metadata("unsupported commit encoding"));
    }
    let author = commit.author()?.ok_or(Error::Metadata("missing author"))?;
    let name = header(author.name, "author name")?;
    header(author.email, "author email")?;
    if !author.email.is_ascii()
        || author
            .email
            .iter()
            .any(|b| matches!(b, b'<' | b'>' | b' ' | b'\t'))
    {
        return Err(Error::Metadata("author email"));
    }
    let prefix = if options.subject_prefix.is_empty() {
        ""
    } else {
        header(options.subject_prefix, "subject prefix")?
    };
    if !prefix.is_ascii() {
        return Err(Error::Metadata("subject prefix"));
    }
    let text =
        std::str::from_utf8(commit.message()).map_err(|_| Error::Metadata("message UTF-8"))?;
    if text.contains('\0') {
        return Err(Error::Metadata("message delimiters"));
    }
    let (subject, body) = split_message(commit, text)?;
    if subject.contains('\r') {
        return Err(Error::Metadata("subject delimiters"));
    }
    if subject.is_empty() {
        return Err(Error::Metadata("missing subject"));
    }
    if options
        .base
        .is_some_and(|id| id.format() != commit.id().format())
    {
        return Err(Error::Metadata("base object format"));
    }
    let numbering = match options.position {
        Some(Position { number, total })
            if (number != 0 || cover) && total != 0 && number <= total =>
        {
            format!(
                "{}{number:0width$}/{total}",
                if prefix.is_empty() { "" } else { " " },
                width = total.to_string().len(),
            )
        }
        Some(_) => return Err(Error::Position),
        None => String::new(),
    };
    let date = author.date()?;
    let offset = jiff::tz::Offset::from_seconds(i32::from(date.offset_minutes) * 60)
        .map_err(|_| Error::Date)?;
    let timestamp = jiff::Timestamp::from_second(date.seconds).map_err(|_| Error::Date)?;
    let date = timestamp
        .to_zoned(jiff::tz::TimeZone::fixed(offset))
        .strftime("%a, %-d %b %Y %T %z")
        .to_string();
    let mut from = String::from("From: ");
    if needs_encoding(name) {
        encoded(&mut from, name);
    } else {
        quoted_name(&mut from, name);
    }
    from.push_str(" <");
    from.push_str(std::str::from_utf8(author.email).map_err(|_| Error::Metadata("author email"))?);
    from.push_str(">\n");
    let mut title = if prefix.is_empty() && numbering.is_empty() {
        String::from("Subject: ")
    } else {
        format!("Subject: [{prefix}{numbering}] ")
    };
    if !needs_encoding(&subject) {
        folded(&mut title, &subject);
    } else {
        encoded(&mut title, &subject);
    }
    title.push('\n');
    Ok(PreparedHeaders {
        from,
        date,
        title,
        body,
        mime: !name.is_ascii() || !text.is_ascii(),
    })
}

fn header<'a>(bytes: &'a [u8], field: &'static str) -> Result<&'a str, Error> {
    if bytes.is_empty() || bytes.iter().any(|b| matches!(b, 0 | b'\r' | b'\n')) {
        return Err(Error::Metadata(field));
    }
    std::str::from_utf8(bytes).map_err(|_| Error::Metadata(field))
}

fn split_message<'a>(commit: &Commit, text: &'a str) -> Result<(String, &'a str), Error> {
    // Commit::subject owns Git's paragraph normalization; only locate the borrowed body here.
    let subject =
        String::from_utf8(commit.subject()).map_err(|_| Error::Metadata("subject UTF-8"))?;
    let mut started = false;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if line.trim_ascii_end().is_empty() {
            if started {
                return Ok((subject, trim_blank_lines(&text[offset..])));
            }
        } else {
            started = true;
        }
    }
    Ok((subject, ""))
}

fn trim_blank_lines(mut body: &str) -> &str {
    while let Some((line, remaining)) = body.split_once('\n') {
        if !line.trim_ascii_end().is_empty() {
            break;
        }
        body = remaining;
    }
    if body.trim_ascii_end().is_empty() {
        ""
    } else {
        body
    }
}

fn needs_encoding(text: &str) -> bool {
    !text.is_ascii() || text.contains("=?")
}

fn quoted_name(out: &mut String, name: &str) {
    let quote = name.bytes().any(|b| b"\"\\,;<>@()[].:".contains(&b));
    if quote {
        out.push('"');
    }
    for ch in name.chars() {
        if quote && matches!(ch, '"' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    if quote {
        out.push('"');
    }
}

fn folded(out: &mut String, text: &str) {
    let mut column = out.rsplit('\n').next().unwrap_or_default().len();
    for (index, word) in text.split(' ').enumerate() {
        let space = usize::from(index != 0);
        if column + space + word.len() > 78 && index != 0 {
            out.push_str("\n ");
            column = 1;
        } else if space != 0 {
            out.push(' ');
            column += 1;
        }
        out.push_str(word);
        column += word.len();
    }
}

fn encoded(out: &mut String, text: &str) {
    let mut column = out.rsplit('\n').next().unwrap_or_default().len();
    out.push_str("=?UTF-8?q?");
    column += 10;
    for ch in text.chars() {
        let mut bytes = [0; 4];
        let bytes = ch.encode_utf8(&mut bytes).as_bytes();
        let mut group = String::new();
        for byte in bytes {
            if (b'!'..=b'~').contains(byte) && !b"=?_".contains(byte) {
                group.push(char::from(*byte));
            } else {
                group.push_str(&format!("={byte:02X}"));
            }
        }
        if column + group.len() + 2 > 76 {
            out.push_str("?=\n =?UTF-8?q?");
            column = 11;
        }
        out.push_str(&group);
        column += group.len();
    }
    out.push_str("?=");
}

struct Output<'a> {
    bytes: Vec<u8>,
    limit: usize,
    cancel: &'a AtomicBool,
}
impl Output<'_> {
    fn check(&self) -> Result<(), Error> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.check()?;
        let size = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(Error::Limit("output bytes"))?;
        if size > self.limit {
            return Err(Error::Limit("output bytes"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{ObjectFormat, Signature, Tree};

    fn commit() -> Commit {
        let author = Signature {
            name: b"Author".to_vec(),
            email: b"author@example.com".to_vec(),
            seconds: 1_000_000_000,
            offset_minutes: 0,
        };
        let tree = Tree::new(ObjectFormat::Sha1, vec![]).unwrap();
        Commit::builder(tree.id(), author, b"Subject\n\nBody\n")
            .build()
            .unwrap()
    }

    #[rstest]
    #[case::metadata(Limits::default().with_max_metadata_bytes(0), "metadata bytes")]
    #[case::output(Limits::default().with_max_output_bytes(1), "output bytes")]
    fn bounds_refuse_an_artifact(#[case] limits: Limits, #[case] bound: &str) {
        let commit = commit();
        let stat = Diffstat::new(vec![], Default::default()).unwrap();
        let result = Message::new(&commit, &stat, b"patch\n").render_with(
            Options::default(),
            limits,
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(Error::Limit(name)) if name == bound));
    }

    #[rstest]
    #[case::line_feed(b"PATCH\nInjected: yes")]
    #[case::carriage_return(b"PATCH\rInjected: yes")]
    #[case::nul(b"PATCH\0")]
    fn header_injection_is_refused(#[case] prefix: &[u8]) {
        let commit = commit();
        let stat = Diffstat::new(vec![], Default::default()).unwrap();
        let result = Message::new(&commit, &stat, b"patch\n").render_with(
            Options::default().with_subject_prefix(prefix),
            Limits::default(),
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(Error::Metadata("subject prefix"))));
    }

    #[test]
    fn cancelled_rendering_returns_no_artifact() {
        let commit = commit();
        let stat = Diffstat::new(vec![], Default::default()).unwrap();
        let result = Message::new(&commit, &stat, b"patch\n").render_with(
            Options::default(),
            Limits::default(),
            &AtomicBool::new(true),
        );
        assert!(matches!(result, Err(Error::Cancelled)));
    }

    #[test]
    fn invalid_series_position_is_refused() {
        let commit = commit();
        let stat = Diffstat::new(vec![], Default::default()).unwrap();
        let result = Message::new(&commit, &stat, b"patch\n").render_with(
            Options::default().with_position(2, 1),
            Limits::default(),
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(Error::Position)));
    }
}
