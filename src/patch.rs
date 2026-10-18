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
//! [`DiffAttributes`] applies `.gitattributes` and `diff.<driver>.*` configuration per path:
//! `binary`, `-diff` and `diff=<driver>` choose binary or text output and the hunk header's
//! function line, as `git diff` applies them. [`Objects::write_patch`] applies none, because
//! object storage alone has no attributes; [`Repo::write_patch`](crate::Repo::write_patch) loads
//! them by default, and [`Objects::write_patch_with`] takes them explicitly.
//!
//! The output matches `git diff` for the same entries and attributes with these differences:
//! hunks come from girt's [Myers comparison](crate::content_diff::diff) with boundary compaction
//! and indent-aware scoring calibrated against Git, so some ambiguous changes may align
//! differently; binary detection scans whole blobs for NUL rather than a prefix; girt runs no
//! textconv or external diff commands (drivers that configure them are refused, see
//! [`External`]); Git's built-in diff drivers other than `rust` use the default function rule
//! (see [`FunctionLines::builtin`]); whitespace options and colour are not provided. Binary payload
//! encoding is selected with [`Options::with_binary_patch`]; its compression and delta choices can
//! differ from Git while preserving exactly the same blobs.
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
//!
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
//!
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
mod binary;
mod drivers;
mod function;
mod hunks;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

pub use drivers::DiffAttributes;
use drivers::{Content, Driver};
pub use function::{FunctionLines, PatternError};
pub use hunks::{write_hunks, write_hunks_with};

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
    /// Emit forward/reverse `GIT binary patch` blocks instead of binary summary lines.
    /// Also uses full object IDs for encoded binary index headers, as `git diff --binary` does.
    /// Default false; text classification and attributes still apply.
    pub binary_patch: bool,
    /// Use indentation and blank separators to place equivalent changes (default true).
    /// Set false for placement analogous to Git's `--no-indent-heuristic`; compaction still
    /// applies. Some ambiguous placements differ from Git; see [`crate::content_diff::diff`].
    pub indent_heuristic: bool,
    /// What to do with a diff driver that configures `textconv` or `command` (default
    /// [`External::Refuse`]).
    pub external: External,
}

/// The handling of diff drivers that would run a command, which girt never does
/// ([issue 462](https://github.com/joshka/girt/issues/462)).
///
/// ```
/// use girt::patch::{External, Options};
///
/// // `git diff --no-textconv --no-ext-diff`
/// let options = Options::default().with_external(External::Ignore);
/// assert_eq!(options.external, External::Ignore);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum External {
    /// Fail with [`Error::External`] before writing the file's patch, since Git would run the
    /// driver's `diff.<driver>.textconv` or `diff.<driver>.command`.
    #[default]
    Refuse,
    /// Ignore those settings and diff the stored content, as `--no-textconv --no-ext-diff` do.
    /// The driver's other settings still apply.
    Ignore,
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

    /// Sets [`Self::binary_patch`].
    #[must_use]
    pub fn with_binary_patch(mut self, enabled: bool) -> Self {
        self.binary_patch = enabled;
        self
    }

    /// Sets [`Self::external`].
    #[must_use]
    pub fn with_external(mut self, external: External) -> Self {
        self.external = external;
        self
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            context_lines: 3,
            abbrev_len: None,
            binary: BinaryMode::Auto,
            binary_patch: false,
            indent_heuristic: true,
            external: External::Refuse,
        }
    }
}

/// Resource bounds applied to each file of a patch; the whole patch is unbounded.
///
/// Each file pair is read under `read`, with the pair's combined payload also capped by
/// `diff.max_input_bytes`, and then compared under `diff`. Output is streamed and not retained,
/// so these bound memory and work per file rather than for the entries slice.
///
/// Binary payload encoding admits the same combined input bound. It additionally retains
/// compressed literal and delta candidates, bounded by a constant multiple of admitted input
/// plus zlib scratch and a 32 KiB anchor table. `binary_delta_work` bounds delta matching across
/// both directions: one unit per indexed anchor, scanned position or compared byte. Exhaustion uses
/// a literal block. Compression and output are linear in the admitted inputs and generated
/// instructions, with cancellation checks between chunks; compressed bytes can vary with backend
/// versions. These are algorithmic bounds, not a total-process heap or wall-clock limit.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Bounds for reading each blob.
    pub read: ReadLimits,
    /// Bounds for comparing each file pair.
    pub diff: DiffLimits,
    /// Matching work shared by forward/reverse binary deltas; default 64 Mi units.
    /// Zero disables delta search and emits literals.
    pub binary_delta_work: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            read: ReadLimits::default(),
            diff: DiffLimits::default(),
            binary_delta_work: 64 * 1024 * 1024,
        }
    }
}

impl Limits {
    /// Sets [`Self::binary_delta_work`].
    #[must_use]
    pub fn with_binary_delta_work(mut self, binary_delta_work: usize) -> Self {
        self.binary_delta_work = binary_delta_work;
        self
    }

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
    /// A caller-selected owned patch output bound was exceeded.
    #[error("patch output exceeds byte limit")]
    OutputLimit,
    /// Summary input or output exceeded its independent bounds.
    #[error("collecting patch statistics")]
    Stat(#[from] crate::diffstat::Error),
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
    /// The attributes of a path could not be read.
    #[error("reading attributes of {}", quote_path(.path))]
    Attributes {
        /// Path whose attributes were requested.
        path: Vec<u8>,
        /// File, storage or limit failure, boxed to keep this error small.
        #[source]
        source: Box<crate::attributes::LoadError>,
    },
    /// A diff driver's function-name pattern could not be compiled.
    #[error("compiling the function pattern of diff driver {}", String::from_utf8_lossy(.driver))]
    Function {
        /// Driver name from the `diff` attribute.
        driver: Vec<u8>,
        /// Compilation failure.
        #[source]
        source: PatternError,
    },
    /// A path's diff driver configures a command girt does not run; nothing of this file's patch
    /// was written. Use [`External::Ignore`] to diff stored content instead.
    #[error(
        "diff driver {} of {} configures {kind}, which girt does not run",
        String::from_utf8_lossy(.driver),
        quote_path(.path)
    )]
    External {
        /// Path whose driver configures the command.
        path: Vec<u8>,
        /// Driver name from the `diff` attribute.
        driver: Vec<u8>,
        /// `textconv` or `command`.
        kind: &'static str,
    },
}

impl Objects {
    /// Writes `entries` as a unified patch with default options and limits, without attributes or
    /// cancellation.
    ///
    /// Wraps [`Self::write_patch_with`] with [`Options::default`], no [`DiffAttributes`] and
    /// [`Limits::default`]. See the [module documentation](crate::patch) for the output format and
    /// its differences from `git diff`; [`Repo::write_patch`](crate::Repo::write_patch) applies the
    /// repository's attributes.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for write, storage, limit and abbreviation failures.
    pub fn write_patch(&self, out: impl io::Write, entries: &[Entry]) -> Result<(), Error> {
        self.write_patch_with(
            out,
            entries,
            Options::default(),
            None,
            Limits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Writes `entries` as a unified patch under explicit options, attributes, limits and
    /// cancellation.
    ///
    /// Entries are rendered in slice order;
    /// [`Comparison::entries`](crate::tree_diff::Comparison::entries) already uses Git's order.
    /// A change between a regular file or symlink and a different type (Git's `T` status)
    /// is written as a deletion followed by an addition, as Git does. Gitlinks render as one
    /// `Subproject commit <id>` line per side, as `git diff` does without `--submodule`. A rename
    /// or copy prints its similarity from the entry. Identical identities print headers only.
    ///
    /// With `attributes`, each side's driver comes from its own path (the old path for the old
    /// side of a rename); see [`DiffAttributes`]. Gitlink sides have no driver. Attribute files are
    /// read as paths first need them.
    ///
    /// Blob reads are synchronous. Cancellation is checked before each entry and inside each
    /// read and comparison; a single storage read or writer call cannot be interrupted.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on the first write, storage, attribute, driver, limit, abbreviation or
    /// cancellation failure, after writing the complete patches of earlier entries and possibly
    /// part of the failing entry. Attribute and driver failures occur before the entry's first
    /// line. Nothing is written to storage.
    pub fn write_patch_with(
        &self,
        mut out: impl io::Write,
        entries: &[Entry],
        options: Options,
        attributes: Option<&mut DiffAttributes>,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        let mut renderer = Renderer {
            objects: self,
            out: &mut out,
            options,
            attributes,
            stats: None,
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
    attributes: Option<&'a mut DiffAttributes>,
    stats: Option<Vec<crate::diffstat::FileStat>>,
    limits: Limits,
    abbrev_len: usize,
    cancel: &'a AtomicBool,
}

impl<W: io::Write> Renderer<'_, W> {
    fn entry(&mut self, entry: &Entry) -> Result<(), Error> {
        let (path, source, summary) = match entry {
            Entry::Change(change) => {
                let summary = match (change.old, change.new) {
                    (Some(old), Some(new)) if change.status() == ChangeStatus::TypeChanged => {
                        let path = &change.path;
                        let removed = self.file((path, path), Some(old), None, None)?;
                        let added = self.file((path, path), None, Some(new), None)?;
                        removed.combine(added)?
                    }
                    (old, new) => self.file((&change.path, &change.path), old, new, None)?,
                };
                (&change.path, None, summary)
            }
            Entry::Rewrite {
                kind,
                similarity,
                source,
                target,
                old,
                new,
            } => {
                let summary = self.file(
                    (source, target),
                    Some(*old),
                    Some(*new),
                    Some(Rewrite {
                        kind: *kind,
                        similarity: *similarity,
                    }),
                )?;
                (target, Some(source), summary)
            }
        };
        if let Some(stats) = &mut self.stats {
            stats.push(crate::diffstat::FileStat {
                path: path.clone(),
                source_path: source.cloned(),
                counts: summary.counts(),
            });
        }
        Ok(())
    }

    fn file(
        &mut self,
        (old_path, new_path): (&[u8], &[u8]),
        old: Option<TreeValue>,
        new: Option<TreeValue>,
        rewrite: Option<Rewrite>,
    ) -> Result<FileSummary, Error> {
        let old_driver = self.driver(old_path, old)?;
        let new_driver = self.driver(new_path, new)?;
        if self.options.external == External::Refuse
            && let Some(driver) = [&old_driver, &new_driver]
                .into_iter()
                .find(|driver| driver.external.is_some())
        {
            return Err(Error::External {
                path: new_path.to_vec(),
                driver: driver.name.clone().unwrap_or_default(),
                kind: driver.external.unwrap_or_default(),
            });
        }
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
            return Ok(FileSummary::default());
        }
        let content = self.read(new_path, old, new)?;
        let binary = match (self.options.binary, old_driver.content, new_driver.content) {
            (BinaryMode::Auto, Content::Auto, Content::Auto) | (BinaryMode::Text, ..) => {
                self.options.binary
            }
            (BinaryMode::Auto, old_content, new_content) => {
                let binary = |content: Content, bytes: &[u8]| match content {
                    Content::Auto => bytes.contains(&0),
                    Content::Text => false,
                    Content::Binary => true,
                };
                if binary(old_content, &content.old) || binary(new_content, &content.new) {
                    BinaryMode::Binary
                } else {
                    BinaryMode::Text
                }
            }
            (mode, ..) => mode,
        };
        let content_diff = diff_with_options(
            &content.old,
            &content.new,
            crate::content_diff::Options::default()
                .with_binary(binary)
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
        let summary = FileSummary::new(&content_diff, content.old.len(), content.new.len());
        let full = self.options.binary_patch && matches!(content_diff, ContentDiff::Binary { .. });
        let old_abbrev = self.abbreviate(old_id, full)?;
        let new_abbrev = self.abbreviate(new_id, full)?;
        let out = &mut *self.out;
        write!(out, "index {old_abbrev}..{new_abbrev}")?;
        match (old, new) {
            (Some(old), Some(new)) if old.mode == new.mode => {
                writeln!(out, " {}", new.mode.octal())?;
            }
            _ => writeln!(out)?,
        }

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
                if self.options.binary_patch {
                    binary::write(
                        out,
                        &content.old,
                        &content.new,
                        self.limits.binary_delta_work,
                        self.cancel,
                    )?;
                } else {
                    writeln!(out, "Binary files {old_name} and {new_name} differ")?;
                }
            }
            ContentDiff::Text(text) => {
                // Like GNU diff, Git ends a name containing a space with a tab so it parses back.
                let tab = |name: &str| if name.contains(' ') { "\t" } else { "" };
                writeln!(out, "--- {old_name}{}", tab(old_name))?;
                writeln!(out, "+++ {new_name}{}", tab(new_name))?;
                let function = old_driver
                    .function
                    .as_ref()
                    .or(new_driver.function.as_ref());
                let default = FunctionLines::default();
                write_hunks_with(
                    out,
                    &text,
                    self.options.context_lines,
                    function.unwrap_or(&default),
                )?;
            }
        }
        Ok(summary)
    }

    /// The driver of one side; absent sides, gitlinks and renders without attributes have none.
    fn driver(&mut self, path: &[u8], value: Option<TreeValue>) -> Result<Driver, Error> {
        match (&mut self.attributes, value) {
            (Some(attributes), Some(value)) if value.mode != EntryMode::Gitlink => {
                attributes.driver(path)
            }
            _ => Ok(Driver::default()),
        }
    }

    /// Abbreviates an identity, or prints the null identity at the minimum length.
    fn abbreviate(&self, id: Option<ObjectId>, full: bool) -> Result<String, Error> {
        if full {
            return Ok(id.map_or_else(
                || "0".repeat(self.objects.object_format().digest_len() * 2),
                |id| id.to_string(),
            ));
        }
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

/// Counts retain both sizes even for text, so a type change can combine its two rendered records.
#[derive(Default)]
struct FileSummary {
    added: u64,
    removed: u64,
    old_bytes: u64,
    new_bytes: u64,
    binary: bool,
}

impl FileSummary {
    fn new(diff: &ContentDiff<'_>, old: usize, new: usize) -> Self {
        let mut summary = Self {
            old_bytes: old as u64,
            new_bytes: new as u64,
            ..Self::default()
        };
        match diff {
            ContentDiff::Binary { .. } => summary.binary = true,
            ContentDiff::Text(text) => {
                // Edits partition admitted input lines, whose combined count fits usize.
                summary.added = text
                    .edits()
                    .iter()
                    .map(|edit| edit.new_lines.len() as u64)
                    .sum();
                summary.removed = text
                    .edits()
                    .iter()
                    .map(|edit| edit.old_lines.len() as u64)
                    .sum();
            }
            ContentDiff::Unchanged => {}
        }
        summary
    }

    fn combine(self, other: Self) -> Result<Self, Error> {
        let sum = |a: u64, b: u64| {
            a.checked_add(b)
                .ok_or(Error::Stat(crate::diffstat::Error::Limit("counts")))
        };
        Ok(Self {
            added: sum(self.added, other.added)?,
            removed: sum(self.removed, other.removed)?,
            old_bytes: sum(self.old_bytes, other.old_bytes)?,
            new_bytes: sum(self.new_bytes, other.new_bytes)?,
            binary: self.binary || other.binary,
        })
    }

    fn counts(self) -> crate::diffstat::Counts {
        if self.binary {
            crate::diffstat::Counts::Binary {
                old_bytes: self.old_bytes,
                new_bytes: self.new_bytes,
            }
        } else {
            crate::diffstat::Counts::Text {
                added: self.added,
                removed: self.removed,
            }
        }
    }
}

pub(crate) struct MailBounds {
    pub patch: Limits,
    pub stat: crate::diffstat::Limits,
    pub output: Option<usize>,
}

pub(crate) struct MailPatch {
    pub patch: Vec<u8>,
    pub stat: crate::diffstat::Diffstat,
    pub summary: Vec<u8>,
}

/// Collects an owned patch and its counts from the same attributed comparison.
pub(crate) fn mail_patch(
    objects: &Objects,
    entries: &[Entry],
    options: Options,
    attributes: Option<&mut DiffAttributes>,
    bounds: MailBounds,
    cancel: &AtomicBool,
) -> Result<MailPatch, Error> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    if entries.len() > bounds.stat.max_files {
        return Err(crate::diffstat::Error::Limit("files").into());
    }
    let mut path_bytes = 0usize;
    for entry in entries {
        let paths = match entry {
            Entry::Change(change) => (change.path.as_slice(), &[][..]),
            Entry::Rewrite { source, target, .. } => (source.as_slice(), target.as_slice()),
        };
        path_bytes = path_bytes
            .checked_add(paths.0.len())
            .and_then(|n| n.checked_add(paths.1.len()))
            .ok_or(crate::diffstat::Error::Limit("path bytes"))?;
        if path_bytes > bounds.stat.max_path_bytes {
            return Err(crate::diffstat::Error::Limit("path bytes").into());
        }
    }
    let mut output = BoundedOutput {
        bytes: Vec::new(),
        limit: bounds.output,
        exceeded: false,
    };
    let result = {
        let mut renderer = Renderer {
            objects,
            out: &mut output,
            options,
            attributes,
            stats: Some(Vec::new()),
            limits: bounds.patch,
            abbrev_len: options
                .abbrev_len
                .unwrap_or_else(|| objects.auto_abbrev_len()),
            cancel,
        };
        let result = entries.iter().try_for_each(|entry| {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            renderer.entry(entry)
        });
        result.map(|()| renderer.stats.take().unwrap_or_default())
    };
    if output.exceeded {
        return Err(Error::OutputLimit);
    }
    let records = result?;
    let summary_limit = bounds
        .output
        .map(|limit| limit.saturating_sub(output.bytes.len()));
    let summary = change_summary(entries, &records, summary_limit)?;
    let stat = crate::diffstat::Diffstat::new(records, bounds.stat)?;
    Ok(MailPatch {
        patch: output.bytes,
        stat,
        summary,
    })
}

struct BoundedOutput {
    bytes: Vec<u8>,
    limit: Option<usize>,
    exceeded: bool,
}
impl io::Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(limit) = self.limit else {
            return Ok(bytes.len());
        };
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > limit)
        {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "patch output bound",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn change_summary(
    entries: &[Entry],
    records: &[crate::diffstat::FileStat],
    limit: Option<usize>,
) -> Result<Vec<u8>, Error> {
    use io::Write as _;
    let Some(limit) = limit else {
        return Ok(Vec::new());
    };
    let mut out = BoundedOutput {
        bytes: Vec::new(),
        limit: Some(limit),
        exceeded: false,
    };
    let result = (|| -> io::Result<()> {
        for (entry, record) in entries.iter().zip(records) {
            match entry {
                Entry::Change(change) => match (change.old, change.new) {
                    (None, Some(new)) => writeln!(
                        out,
                        " create mode {} {}",
                        new.mode.octal(),
                        quote_path(&change.path)
                    )?,
                    (Some(old), None) => writeln!(
                        out,
                        " delete mode {} {}",
                        old.mode.octal(),
                        quote_path(&change.path)
                    )?,
                    (Some(old), Some(new)) if old.mode != new.mode => writeln!(
                        out,
                        " mode change {} => {} {}",
                        old.mode.octal(),
                        new.mode.octal(),
                        quote_path(&change.path)
                    )?,
                    _ => {}
                },
                Entry::Rewrite {
                    kind,
                    similarity,
                    old,
                    new,
                    ..
                } => {
                    let verb = match kind {
                        Kind::Rename => "rename",
                        Kind::Copy => "copy",
                    };
                    writeln!(
                        out,
                        " {verb} {} ({similarity}%)",
                        crate::diffstat::text_path(record)
                    )?;
                    if old.mode != new.mode {
                        writeln!(
                            out,
                            " mode change {} => {}",
                            old.mode.octal(),
                            new.mode.octal()
                        )?;
                    }
                }
            }
        }
        Ok(())
    })();
    if out.exceeded {
        return Err(Error::OutputLimit);
    }
    result?;
    Ok(out.bytes)
}
