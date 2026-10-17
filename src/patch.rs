//! Unified patches in the form `git diff` prints, written to an [`io::Write`].
//!
//! [`Objects::write_patch`] renders the entries of a [`Comparison`](crate::tree_diff::Comparison):
//! the `diff --git` header, mode, new/deleted, rename and copy headers, the `index` line with
//! abbreviated identities, `---`/`+++` names and unified hunks. [`write_hunks`] renders only the
//! hunks of one [`TextDiff`](crate::content_diff::TextDiff), for callers that compare content they
//! loaded themselves.
//!
//! Output is a byte stream, not text: content lines keep their exact bytes, including CR, invalid
//! UTF-8 and a missing final LF (marked `\ No newline at end of file`). Paths are quoted with
//! [`quote_path`]. Nothing is collected into one buffer; each line goes straight to the writer, so
//! pass a buffered writer such as [`io::BufWriter`] or a `Vec<u8>`.
//!
//! The output matches `git diff` for the same entries with these differences: hunks come from
//! girt's [Myers comparison](crate::content_diff::diff) with boundary compaction and indent-aware
//! scoring calibrated against Git, so some ambiguous changes may align differently; binary
//! detection scans whole blobs for NUL rather than a prefix; attributes, textconv, `diff.<driver>`
//! function names, whitespace options, colour and binary patch encoding are not provided. The hunk
//! header's function context uses Git's default rule.
//!
//! ```
//! use girt::tree::TreeEditor;
//! use girt::{InitKind, ObjectFormat, Repository};
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r"),
//!     InitKind::Bare,
//! )?;
//! let loose = repo.loose_objects();
//! let mut editor = TreeEditor::new(&loose);
//! editor.set(b"list.txt", loose.write_blob(b"apples\npears\n")?)?;
//! let old = editor.write()?;
//! editor.set(b"list.txt", loose.write_blob(b"apples\nplums\n")?)?;
//! let new = editor.write()?;
//!
//! let objects = repo.objects()?;
//! let comparison = objects.diff_trees(Some(old), Some(new))?;
//! let mut patch = Vec::new();
//! objects.write_patch(&mut patch, &comparison.entries)?;
//! assert_eq!(
//!     String::from_utf8(patch)?,
//!     "diff --git a/list.txt b/list.txt\n\
//!      index 33df257..5e18cd5 100644\n\
//!      --- a/list.txt\n\
//!      +++ b/list.txt\n\
//!      @@ -1,2 +1,2 @@\n \
//!      apples\n\
//!      -pears\n\
//!      +plums\n"
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
mod hunks;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

pub use hunks::write_hunks;

use crate::content_diff::{
    BinaryMode, BlobContent, ContentDiff, ContentReadError, DiffError, DiffLimits,
    diff_with_options,
};
use crate::display::quote_path;
use crate::rewrites::Kind;
use crate::tree_diff::Entry;
use crate::{
    ChangeStatus, EntryMode, ObjectId, ObjectReadError, Objects, ReadLimits, TreeChange, TreeValue,
};

/// Presentation choices for [`Objects::write_patch_with`]; the defaults match `git diff`.
///
/// ```
/// use girt::content_diff::BinaryMode;
/// use girt::patch::Options;
///
/// // `git diff --unified=0 --abbrev=12 --text`
/// let options = Options::default()
///     .with_context_lines(0)
///     .with_abbrev_len(Some(12))
///     .with_binary(BinaryMode::Text);
/// assert_eq!(options.context_lines, 0);
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Options {
    /// Unchanged lines shown around each change (default 3, as `--unified=<n>`). Changes whose
    /// gap is at most twice this many lines share one hunk.
    pub context_lines: usize,
    /// Minimum `index` line abbreviation (as `--abbrev=<n>`); `None` (the default) uses
    /// [`Objects::auto_abbrev_len`]. Identities lengthen when needed to stay unique; values are
    /// clamped as [`Objects::abbreviate`] does. Read `core.abbrev` yourself to honour it.
    pub abbrev_len: Option<usize>,
    /// How changed content is classified (default [`BinaryMode::Auto`]; `Text` is `--text`).
    pub binary: BinaryMode,
    /// Use indentation and blank separators to place equivalent changes (default true).
    /// Set false for placement analogous to Git's `--no-indent-heuristic`; compaction still
    /// applies. Some ambiguous placements differ from Git; see [`crate::content_diff::diff`].
    pub indent_heuristic: bool,
}

impl Options {
    /// Sets [`Self::context_lines`].
    #[must_use]
    pub fn with_context_lines(mut self, context_lines: usize) -> Self {
        self.context_lines = context_lines;
        self
    }

    /// Sets [`Self::abbrev_len`].
    #[must_use]
    pub fn with_abbrev_len(mut self, abbrev_len: Option<usize>) -> Self {
        self.abbrev_len = abbrev_len;
        self
    }

    /// Sets [`Self::indent_heuristic`].
    #[must_use]
    pub fn with_indent_heuristic(mut self, enabled: bool) -> Self {
        self.indent_heuristic = enabled;
        self
    }

    /// Sets [`Self::binary`].
    #[must_use]
    pub fn with_binary(mut self, binary: BinaryMode) -> Self {
        self.binary = binary;
        self
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            context_lines: 3,
            abbrev_len: None,
            binary: BinaryMode::Auto,
            indent_heuristic: true,
        }
    }
}

/// Resource bounds applied to each file of a patch; the whole patch is unbounded.
///
/// Each file pair is read under `read`, with the pair's combined payload also capped by
/// `diff.max_input_bytes`, and then compared under `diff`. Output is streamed and not retained,
/// so these bound memory and work per file rather than for the entries slice.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct Limits {
    /// Bounds for reading each blob.
    pub read: ReadLimits,
    /// Bounds for comparing each file pair.
    pub diff: DiffLimits,
}

impl Limits {
    /// Sets [`Self::read`].
    #[must_use]
    pub fn with_read(mut self, read: ReadLimits) -> Self {
        self.read = read;
        self
    }

    /// Sets [`Self::diff`].
    #[must_use]
    pub fn with_diff(mut self, diff: DiffLimits) -> Self {
        self.diff = diff;
        self
    }
}

/// Rendering a patch stopped; lines already written remain in the writer.
///
/// Each content failure names the destination byte path of the entry being rendered, so a
/// partial patch can be reported against the file that stopped it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The writer failed.
    #[error("writing patch")]
    Write(#[from] io::Error),
    /// A blob side could not be loaded; trees and other unsupported modes are reported here.
    #[error("reading content of {}", quote_path(.path))]
    Read {
        /// Destination path of the entry.
        path: Vec<u8>,
        /// Storage, kind, mode or limit failure, boxed to keep this error small.
        #[source]
        source: Box<ContentReadError>,
    },
    /// Content comparison exceeded a bound.
    #[error("comparing content of {}", quote_path(.path))]
    Diff {
        /// Destination path of the entry.
        path: Vec<u8>,
        /// Exceeded bound.
        #[source]
        source: DiffError,
    },
    /// An identity could not be abbreviated against storage.
    #[error("abbreviating {id}")]
    Abbreviate {
        /// Identity being abbreviated.
        id: ObjectId,
        /// Storage inspection failure, boxed to keep this error small.
        #[source]
        source: Box<ObjectReadError>,
    },
    /// The caller's cancellation flag was observed between or within entries.
    #[error("patch rendering cancelled")]
    Cancelled,
}

impl Objects {
    /// Writes `entries` as a unified patch with default options and limits, without cancellation.
    ///
    /// Wraps [`Self::write_patch_with`] with [`Options::default`] and [`Limits::default`]. See
    /// the [module documentation](crate::patch) for the output format and its differences from
    /// `git diff`.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for write, storage, limit and abbreviation failures.
    pub fn write_patch(&self, out: impl io::Write, entries: &[Entry]) -> Result<(), Error> {
        self.write_patch_with(
            out,
            entries,
            Options::default(),
            Limits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Writes `entries` as a unified patch under explicit options, limits and cancellation.
    ///
    /// Entries are rendered in slice order;
    /// [`Comparison::entries`](crate::tree_diff::Comparison::entries) already uses Git's order.
    /// A change between a regular file or symlink and a different type (Git's `T` status)
    /// is written as a deletion followed by an addition, as Git does. Gitlinks render as one
    /// `Subproject commit <id>` line per side, as `git diff` does without `--submodule`. A rename
    /// or copy prints its similarity from the entry. Identical identities print headers only.
    ///
    /// Blob reads are synchronous. Cancellation is checked before each entry and inside each
    /// read and comparison; a single storage read or writer call cannot be interrupted.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on the first write, storage, limit, abbreviation or cancellation failure,
    /// after writing the complete patches of earlier entries and possibly part of the failing
    /// entry. Nothing is written to storage.
    pub fn write_patch_with(
        &self,
        mut out: impl io::Write,
        entries: &[Entry],
        options: Options,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        let mut renderer = Renderer {
            objects: self,
            out: &mut out,
            options,
            limits,
            abbrev_len: options.abbrev_len.unwrap_or_else(|| self.auto_abbrev_len()),
            cancel,
        };
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            renderer.entry(entry)?;
        }
        Ok(())
    }
}

/// How a file pair is related, for its extended header lines.
struct Rewrite {
    kind: Kind,
    similarity: u8,
}

struct Renderer<'a, W> {
    objects: &'a Objects,
    out: &'a mut W,
    options: Options,
    limits: Limits,
    abbrev_len: usize,
    cancel: &'a AtomicBool,
}

impl<W: io::Write> Renderer<'_, W> {
    fn entry(&mut self, entry: &Entry) -> Result<(), Error> {
        match entry {
            Entry::Change(change) => match (change.old, change.new) {
                (Some(old), Some(new)) if change.status() == ChangeStatus::TypeChanged => {
                    let path = &change.path;
                    self.file((path, path), Some(old), None, None)?;
                    self.file((path, path), None, Some(new), None)
                }
                (old, new) => self.file((&change.path, &change.path), old, new, None),
            },
            Entry::Rewrite {
                kind,
                similarity,
                source,
                target,
                old,
                new,
            } => self.file(
                (source, target),
                Some(*old),
                Some(*new),
                Some(Rewrite {
                    kind: *kind,
                    similarity: *similarity,
                }),
            ),
        }
    }

    fn file(
        &mut self,
        (old_path, new_path): (&[u8], &[u8]),
        old: Option<TreeValue>,
        new: Option<TreeValue>,
        rewrite: Option<Rewrite>,
    ) -> Result<(), Error> {
        let old_name = quote_path(&[b"a/", old_path].concat()).to_string();
        let new_name = quote_path(&[b"b/", new_path].concat()).to_string();
        let out = &mut *self.out;
        writeln!(out, "diff --git {old_name} {new_name}")?;
        match (old, new) {
            (None, Some(new)) => writeln!(out, "new file mode {}", new.mode.octal())?,
            (Some(old), None) => writeln!(out, "deleted file mode {}", old.mode.octal())?,
            (Some(old), Some(new)) if old.mode != new.mode => writeln!(
                out,
                "old mode {}\nnew mode {}",
                old.mode.octal(),
                new.mode.octal()
            )?,
            _ => {}
        }
        if let Some(Rewrite { kind, similarity }) = rewrite {
            let verb = match kind {
                Kind::Rename => "rename",
                Kind::Copy => "copy",
            };
            writeln!(out, "similarity index {similarity}%")?;
            writeln!(out, "{verb} from {}", quote_path(old_path))?;
            writeln!(out, "{verb} to {}", quote_path(new_path))?;
        }
        let (old_id, new_id) = (old.map(|value| value.id), new.map(|value| value.id));
        if old_id == new_id {
            return Ok(());
        }
        let old_abbrev = self.abbreviate(old_id)?;
        let new_abbrev = self.abbreviate(new_id)?;
        let out = &mut *self.out;
        write!(out, "index {old_abbrev}..{new_abbrev}")?;
        match (old, new) {
            (Some(old), Some(new)) if old.mode == new.mode => {
                writeln!(out, " {}", new.mode.octal())?;
            }
            _ => writeln!(out)?,
        }

        let content = self.read(new_path, old, new)?;
        let content_diff = diff_with_options(
            &content.old,
            &content.new,
            crate::content_diff::Options::default()
                .with_binary(self.options.binary)
                .with_indent_heuristic(self.options.indent_heuristic),
            self.limits.diff,
            self.cancel,
        )
        .map_err(|source| match source {
            DiffError::Cancelled => Error::Cancelled,
            source => Error::Diff {
                path: new_path.to_vec(),
                source,
            },
        })?;
        let old_name = if old.is_some() {
            &old_name
        } else {
            "/dev/null"
        };
        let new_name = if new.is_some() {
            &new_name
        } else {
            "/dev/null"
        };
        let out = &mut *self.out;
        match content_diff {
            ContentDiff::Unchanged => {}
            ContentDiff::Binary { .. } => {
                writeln!(out, "Binary files {old_name} and {new_name} differ")?;
            }
            ContentDiff::Text(text) => {
                // Like GNU diff, Git ends a name containing a space with a tab so it parses back.
                let tab = |name: &str| if name.contains(' ') { "\t" } else { "" };
                writeln!(out, "--- {old_name}{}", tab(old_name))?;
                writeln!(out, "+++ {new_name}{}", tab(new_name))?;
                write_hunks(out, &text, self.options.context_lines)?;
            }
        }
        Ok(())
    }

    /// Abbreviates an identity, or prints the null identity at the minimum length.
    fn abbreviate(&self, id: Option<ObjectId>) -> Result<String, Error> {
        match id {
            Some(id) => self
                .objects
                .abbreviate(id, self.abbrev_len)
                .map(|prefix| prefix.to_string())
                .map_err(|source| Error::Abbreviate {
                    id,
                    source: Box::new(source),
                }),
            None => {
                let width = self.objects.object_format().digest_len() * 2;
                Ok("0".repeat(self.abbrev_len.clamp(4, width)))
            }
        }
    }

    /// Loads both sides; gitlinks become the `Subproject commit` line Git compares instead.
    fn read(
        &self,
        path: &[u8],
        old: Option<TreeValue>,
        new: Option<TreeValue>,
    ) -> Result<BlobContent, Error> {
        let blobs = TreeChange {
            path: path.to_vec(),
            old: old.filter(|value| value.mode != EntryMode::Gitlink),
            new: new.filter(|value| value.mode != EntryMode::Gitlink),
        };
        let mut content = BlobContent::read(
            self.objects,
            &blobs,
            self.limits.read,
            self.limits.diff.max_input_bytes,
            self.cancel,
        )
        .map_err(|source| match source {
            ContentReadError::Cancelled => Error::Cancelled,
            source => Error::Read {
                path: path.to_vec(),
                source: Box::new(source),
            },
        })?;
        let subproject = |value: Option<TreeValue>| {
            value
                .filter(|value| value.mode == EntryMode::Gitlink)
                .map(|value| format!("Subproject commit {}\n", value.id).into_bytes())
        };
        if let Some(line) = subproject(old) {
            content.old = line;
        }
        if let Some(line) = subproject(new) {
            content.new = line;
        }
        Ok(content)
    }
}

#[cfg(test)]
mod tests;
