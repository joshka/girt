//! Stored commit-series composition; output publication stays with the caller.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Error, Limits, MailKind, Message, Options, Position};
use crate::patch::{DiffAttributes, MailBounds};
use crate::{Commit, ObjectId, Objects, ReadLimits, Signature};

/// Automatic, unconditional or suppressed patch numbering.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub enum Numbering {
    /// Number multiple patches or a series with a cover letter (the default).
    #[default]
    Auto,
    /// Number every patch, including a single-patch series.
    Always,
    /// Omit numbering from patches and the cover.
    Never,
}

/// Caller-selected cover author/date and subject/body, independent of the current process.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct CoverLetter<'a> {
    /// Identity and timestamp used for the cover's headers.
    pub author: &'a Signature,
    /// First paragraph is the cover subject; following paragraphs precede the generated shortlog.
    pub message: &'a [u8],
}

impl<'a> CoverLetter<'a> {
    /// Borrows explicit cover metadata. Validation occurs before rendering.
    pub fn new(author: &'a Signature, message: &'a [u8]) -> Self {
        Self { author, message }
    }
}

/// Reproducible series presentation and content policies.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SeriesOptions<'a> {
    /// Mail presentation. `position` must be `None`: numbering is assigned by the series.
    /// An explicit base must be the first patch's direct parent; prerequisite patch IDs are not
    /// yet generated for a more distant base.
    pub mail: Options<'a>,
    /// Patch numbering policy (default automatic).
    pub numbering: Numbering,
    /// Optional cover metadata; no author, date or message is guessed.
    pub cover: Option<CoverLetter<'a>>,
    /// Patch content presentation, defaulting to encoded binary payloads.
    pub patch: crate::patch::Options,
    /// Rename/copy inference policy, independent of attributes and configuration.
    pub rewrites: crate::rewrites::Options,
}

impl Default for SeriesOptions<'_> {
    fn default() -> Self {
        Self {
            mail: Options::default(),
            numbering: Numbering::Auto,
            cover: None,
            patch: crate::patch::Options::default().with_binary_patch(true),
            rewrites: crate::rewrites::Options::default(),
        }
    }
}

impl<'a> SeriesOptions<'a> {
    /// Sets [`Self::mail`].
    #[must_use]
    pub fn with_mail(mut self, mail: Options<'a>) -> Self {
        self.mail = mail;
        self
    }
    /// Sets [`Self::numbering`].
    #[must_use]
    pub fn with_numbering(mut self, numbering: Numbering) -> Self {
        self.numbering = numbering;
        self
    }
    /// Sets [`Self::cover`].
    #[must_use]
    pub fn with_cover(mut self, cover: Option<CoverLetter<'a>>) -> Self {
        self.cover = cover;
        self
    }
    /// Sets [`Self::patch`].
    #[must_use]
    pub fn with_patch(mut self, patch: crate::patch::Options) -> Self {
        self.patch = patch;
        self
    }
    /// Sets [`Self::rewrites`].
    #[must_use]
    pub fn with_rewrites(mut self, rewrites: crate::rewrites::Options) -> Self {
        self.rewrites = rewrites;
        self
    }
}

/// Aggregate series bounds plus independent storage, comparison and message limits.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct SeriesLimits {
    /// Selected commits (default 1000); an empty selection yields no messages.
    pub max_commits: usize,
    /// Aggregate raw selected/baseline commits and caller cover metadata (default 16 MiB).
    /// Generated shortlog bytes are bounded separately by `mail.max_metadata_bytes`.
    pub max_metadata_bytes: usize,
    /// Sum of returned message bytes and mbox separators (default 64 MiB).
    pub max_output_bytes: usize,
    /// Per-commit reconstruction bounds; object size is also capped by remaining metadata.
    pub read: ReadLimits,
    /// Per-comparison tree/rename work, reused for the aggregate cover comparison.
    pub rewrites: crate::rewrites::Limits,
    /// Per-file content comparison and binary encoding bounds.
    pub patch: crate::patch::Limits,
    /// Per-comparison summary input/output bounds.
    pub stat: crate::diffstat::Limits,
    /// Independent bounds for each framed message.
    pub mail: Limits,
}

impl Default for SeriesLimits {
    fn default() -> Self {
        Self {
            max_commits: 1000,
            max_metadata_bytes: 16 * 1024 * 1024,
            max_output_bytes: 64 * 1024 * 1024,
            read: ReadLimits::default(),
            rewrites: crate::rewrites::Limits::default(),
            patch: crate::patch::Limits::default(),
            stat: crate::diffstat::Limits::default(),
            mail: Limits::default(),
        }
    }
}

impl SeriesLimits {
    /// Sets [`Self::max_commits`].
    #[must_use]
    pub fn with_max_commits(mut self, value: usize) -> Self {
        self.max_commits = value;
        self
    }
    /// Sets [`Self::max_metadata_bytes`].
    #[must_use]
    pub fn with_max_metadata_bytes(mut self, value: usize) -> Self {
        self.max_metadata_bytes = value;
        self
    }
    /// Sets [`Self::max_output_bytes`].
    #[must_use]
    pub fn with_max_output_bytes(mut self, value: usize) -> Self {
        self.max_output_bytes = value;
        self
    }
    /// Sets [`Self::read`].
    #[must_use]
    pub fn with_read(mut self, value: ReadLimits) -> Self {
        self.read = value;
        self
    }
    /// Sets [`Self::rewrites`].
    #[must_use]
    pub fn with_rewrites(mut self, value: crate::rewrites::Limits) -> Self {
        self.rewrites = value;
        self
    }
    /// Sets [`Self::patch`].
    #[must_use]
    pub fn with_patch(mut self, value: crate::patch::Limits) -> Self {
        self.patch = value;
        self
    }
    /// Sets [`Self::stat`].
    #[must_use]
    pub fn with_stat(mut self, value: crate::diffstat::Limits) -> Self {
        self.stat = value;
        self
    }
    /// Sets [`Self::mail`].
    #[must_use]
    pub fn with_mail(mut self, value: Limits) -> Self {
        self.mail = value;
        self
    }
}

/// Complete unpublished mail artifacts in the caller's oldest-first order.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Series {
    /// Optional cover, followed by the patch messages during mbox assembly.
    pub cover: Option<Vec<u8>>,
    /// Standalone messages, including explicitly selected empty commits.
    pub patches: Vec<Vec<u8>>,
}

impl Series {
    /// Consumes the messages into Git's `--stdout` framing.
    ///
    /// Adds one separator LF between patch messages; the cover already ends with its separator.
    /// The total was admitted before this value was returned. Peak allocation can include both
    /// the owned messages and their assembled copy; this is not a total-process heap bound.
    #[must_use]
    pub fn into_mbox(self) -> Vec<u8> {
        let mut bytes = self.cover.unwrap_or_default();
        for (index, patch) in self.patches.into_iter().enumerate() {
            if index != 0 {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(&patch);
        }
        bytes
    }
}

/// Stored series computation failed without returning or publishing partial mail.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SeriesError {
    /// A selected commit or the first commit's baseline could not be read.
    #[error("reading patch series commit")]
    Read(#[from] crate::ReadCommitError),
    /// Tree comparison or rename inference failed.
    #[error("comparing patch series trees")]
    Compare(#[from] crate::tree_diff::Error),
    /// Attributed content comparison, summary or patch encoding failed.
    #[error("rendering patch series content")]
    Patch(#[from] crate::patch::Error),
    /// A message's metadata, date or output admission failed.
    #[error("framing patch series mail")]
    Mail(#[from] Error),
    /// Explicit cover metadata could not construct a commit-shaped header source.
    #[error("constructing patch series cover metadata")]
    Cover(#[from] crate::CommitError),
    /// A named aggregate series bound was exceeded.
    #[error("patch series exceeds {0} limit")]
    Limit(&'static str),
    /// Selected commits must form an oldest-first chain with exactly the preceding parent.
    #[error("selected patch series is not a linear oldest-first chain at {0}")]
    Nonlinear(ObjectId),
    /// Merge commits require a caller-selected linearization, not an inferred parent.
    #[error("merge commit {0} is unsupported in patch series")]
    Merge(ObjectId),
    /// A more distant explicit base needs prerequisite patch IDs, which are not generated yet.
    #[error("patch series base must be the first commit's direct parent")]
    BasePrerequisites,
    /// A root-starting series has no parent to use as an explicit base.
    #[error("a root-starting patch series cannot have an explicit base")]
    RootBase,
    /// Candidate policy would silently omit part of rename inference.
    #[error("patch series rename inference was skipped by its candidate limit")]
    InferenceSkipped,
}

impl Objects {
    /// Computes mail for an explicit oldest-first linear commit selection without attributes.
    ///
    /// Wraps [`Self::format_patch_with`] with default options/bounds and no cancellation. Empty
    /// commits are retained when explicitly selected. Nothing is installed or published.
    ///
    /// ```
    /// use girt::{Commit, EntryMode, InitKind, ObjectFormat, Repository, Signature, Tree, TreeEntry};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let format = ObjectFormat::Sha1;
    /// let repo = Repository::init(format, directory.path().join("r"), InitKind::Bare)?;
    /// let loose = repo.loose_objects();
    /// let blob = loose.write_blob(b"Hello\n")?;
    /// let tree = loose.write_tree(&Tree::new(
    ///     format,
    ///     vec![TreeEntry {
    ///         mode: EntryMode::Blob,
    ///         name: b"hello".to_vec(),
    ///         id: blob,
    ///     }],
    /// )?)?;
    /// let author = Signature {
    ///     name: b"Patch Author".to_vec(),
    ///     email: b"author@example.com".to_vec(),
    ///     seconds: 1_000_000_000,
    ///     offset_minutes: 0,
    /// };
    /// let id = loose.write_commit(&Commit::builder(tree, author, b"Add greeting\n").build()?)?;
    ///
    /// let mail = repo.objects()?.format_patch(&[id])?.into_mbox();
    /// assert!(String::from_utf8(mail)?.contains("Subject: [PATCH] Add greeting\n"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`SeriesError`] for unreadable/nonlinear inputs or comparison/framing failures.
    pub fn format_patch(&self, commits: &[ObjectId]) -> Result<Series, SeriesError> {
        self.format_patch_with(
            commits,
            SeriesOptions::default(),
            None,
            SeriesLimits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Composes attributed patches, matching statistics and optional cover mail from stored
    /// commits.
    ///
    /// Selection, revision traversal, mail configuration and publication stay caller-owned. A
    /// supplied attribute context is reused for every comparison, including the cover baseline;
    /// default storage-only rendering consults none. Read-only computation returns no partial
    /// series. It retains selected commit metadata and admitted mail, plus one bounded patch/stat
    /// comparison at a time. Bounds describe algorithmic work and artifacts, not allocator failure
    /// or a wall-clock deadline; synchronous storage reads cannot be interrupted mid-call.
    ///
    /// # Errors
    ///
    /// Refuses merge/noncontiguous selections, skipped rename inference, preassigned mail
    /// positions, root or distant explicit bases, non-UTF-8 metadata and unsupported declared
    /// encodings, exceeded
    /// bounds and cancellation. No repository state or external writer changes on any path.
    pub fn format_patch_with(
        &self,
        ids: &[ObjectId],
        options: SeriesOptions<'_>,
        attributes: Option<&mut DiffAttributes>,
        limits: SeriesLimits,
        cancel: &AtomicBool,
    ) -> Result<Series, SeriesError> {
        check(cancel)?;
        if ids.len() > limits.max_commits {
            return Err(SeriesError::Limit("commits"));
        }
        if options.mail.position.is_some() {
            return Err(Error::Position.into());
        }
        if ids.is_empty() {
            return Ok(Series {
                cover: None,
                patches: vec![],
            });
        }
        let selection = selection(self, ids, &options, limits, cancel)?;
        SeriesRenderer {
            objects: self,
            options: &options,
            limits,
            cancel,
        }
        .render(selection, attributes)
    }
}

struct Selection {
    commits: Vec<Commit>,
    baseline: Option<ObjectId>,
}

fn selection(
    objects: &Objects,
    ids: &[ObjectId],
    options: &SeriesOptions<'_>,
    limits: SeriesLimits,
    cancel: &AtomicBool,
) -> Result<Selection, SeriesError> {
    let mut metadata = limits.max_metadata_bytes;
    admit_cover(options.cover, &mut metadata)?;
    let commits = read_selection(objects, ids, limits, &mut metadata, cancel)?;
    let first = commits.first().ok_or(SeriesError::Limit("commits"))?;
    let parent = first.parents().first().copied();
    if options.mail.base.is_some() && parent.is_none() {
        return Err(SeriesError::RootBase);
    }
    if options.mail.base.is_some() && options.mail.base != parent {
        return Err(SeriesError::BasePrerequisites);
    }
    let baseline = parent
        .map(|id| {
            read_commit(objects, id, limits, &mut metadata, cancel).map(|commit| commit.tree())
        })
        .transpose()?;
    Ok(Selection { commits, baseline })
}

struct SeriesRenderer<'a, 'options> {
    objects: &'a Objects,
    options: &'a SeriesOptions<'options>,
    limits: SeriesLimits,
    cancel: &'a AtomicBool,
}

impl SeriesRenderer<'_, '_> {
    fn render(
        &self,
        selection: Selection,
        mut attributes: Option<&mut DiffAttributes>,
    ) -> Result<Series, SeriesError> {
        let numbered = match self.options.numbering {
            Numbering::Auto => selection.commits.len() > 1 || self.options.cover.is_some(),
            Numbering::Always => true,
            Numbering::Never => false,
        };
        let mut remaining = self
            .limits
            .max_output_bytes
            .checked_sub(selection.commits.len().saturating_sub(1))
            .ok_or(SeriesError::Limit("output bytes"))?;
        let mut patches = Vec::new();
        let mut old = selection.baseline;
        for (index, commit) in selection.commits.iter().enumerate() {
            check(self.cancel)?;
            let content = self.content(
                old,
                commit.tree(),
                attributes.as_deref_mut(),
                Some(remaining),
            )?;
            let mut mail = self.options.mail;
            mail.position = numbered.then_some(Position {
                number: index + 1,
                total: selection.commits.len(),
            });
            if index != 0 || self.options.cover.is_some() {
                mail.base = None;
            }
            let bytes = Message::new(commit, &content.stat, &content.patch)
                .with_change_summary(&content.summary)
                .render_with(mail, self.mail_limits(remaining), self.cancel)?;
            remaining = remaining
                .checked_sub(bytes.len())
                .ok_or(SeriesError::Limit("output bytes"))?;
            patches.push(bytes);
            old = Some(commit.tree());
        }
        let cover = self
            .options
            .cover
            .map(|cover| self.cover(cover, &selection, attributes, remaining))
            .transpose()?;
        check(self.cancel)?;
        Ok(Series { cover, patches })
    }

    fn mail_limits(&self, remaining: usize) -> Limits {
        self.limits
            .mail
            .with_max_output_bytes(self.limits.mail.max_output_bytes.min(remaining))
    }

    fn cover(
        &self,
        cover: CoverLetter<'_>,
        selection: &Selection,
        attributes: Option<&mut DiffAttributes>,
        remaining: usize,
    ) -> Result<Vec<u8>, SeriesError> {
        let last = selection
            .commits
            .last()
            .ok_or(SeriesError::Limit("commits"))?;
        let stat = if selection.baseline.is_some() {
            self.content(selection.baseline, last.tree(), attributes, None)?
                .stat
        } else {
            crate::diffstat::Diffstat::new(vec![], self.limits.stat).map_err(Error::Stat)?
        };
        let message = cover_message(
            cover.message,
            &selection.commits,
            self.limits.mail.max_metadata_bytes,
            self.cancel,
        )?;
        let commit = Commit::builder(last.tree(), cover.author.clone(), message).build()?;
        let mut mail = self.options.mail;
        mail.position = (!matches!(self.options.numbering, Numbering::Never)).then_some(Position {
            number: 0,
            total: selection.commits.len(),
        });
        Ok(Message::new(&commit, &stat, b"").render_as(
            mail,
            self.mail_limits(remaining),
            self.cancel,
            MailKind::Cover {
                envelope: last.id(),
                include_stat: selection.baseline.is_some(),
            },
        )?)
    }
}

fn check(cancel: &AtomicBool) -> Result<(), SeriesError> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled.into())
    } else {
        Ok(())
    }
}

fn read_commit(
    objects: &Objects,
    id: ObjectId,
    limits: SeriesLimits,
    remaining: &mut usize,
    cancel: &AtomicBool,
) -> Result<Commit, SeriesError> {
    check(cancel)?;
    let read = limits.read.with_max_object_bytes(
        limits
            .read
            .max_object_bytes
            .min(limits.mail.max_metadata_bytes)
            .min(*remaining),
    );
    let commit = objects.read_commit_with(id, read)?;
    *remaining = remaining
        .checked_sub(commit.as_bytes().len())
        .ok_or(SeriesError::Limit("metadata bytes"))?;
    Ok(commit)
}

fn read_selection(
    objects: &Objects,
    ids: &[ObjectId],
    limits: SeriesLimits,
    remaining: &mut usize,
    cancel: &AtomicBool,
) -> Result<Vec<Commit>, SeriesError> {
    let mut commits = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let commit = read_commit(objects, *id, limits, remaining, cancel)?;
        if commit.parents().len() > 1 {
            return Err(SeriesError::Merge(*id));
        }
        if index != 0 && commit.parents() != &ids[index - 1..index] {
            return Err(SeriesError::Nonlinear(*id));
        }
        commits.push(commit);
    }
    Ok(commits)
}

fn admit_cover(cover: Option<CoverLetter<'_>>, remaining: &mut usize) -> Result<(), SeriesError> {
    if let Some(cover) = cover {
        let size = [cover.message, &cover.author.name, &cover.author.email]
            .iter()
            .try_fold(0usize, |sum, bytes| sum.checked_add(bytes.len()))
            .ok_or(SeriesError::Limit("metadata bytes"))?;
        *remaining = remaining
            .checked_sub(size)
            .ok_or(SeriesError::Limit("metadata bytes"))?;
    }
    Ok(())
}

// One comparison owns both the patch and its summary; callers never reconstruct counts from a
// different attributes/classification policy. Cover comparison work is independently bounded.
impl SeriesRenderer<'_, '_> {
    // One comparison owns both artifacts, retaining the caller's attributed classification.
    fn content(
        &self,
        old: Option<ObjectId>,
        new: ObjectId,
        attributes: Option<&mut DiffAttributes>,
        output: Option<usize>,
    ) -> Result<crate::patch::MailPatch, SeriesError> {
        let comparison = self.objects.diff_trees_with(
            old,
            Some(new),
            self.options.rewrites,
            self.limits.rewrites,
            self.cancel,
        )?;
        if comparison.inexact_skipped.is_some() {
            return Err(SeriesError::InferenceSkipped);
        }
        Ok(crate::patch::mail_patch(
            self.objects,
            &comparison.entries,
            self.options.patch,
            attributes,
            MailBounds {
                patch: self.limits.patch,
                stat: self.limits.stat,
                output,
            },
            self.cancel,
        )?)
    }
}

fn cover_message(
    message: &[u8],
    commits: &[Commit],
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, SeriesError> {
    let mut groups: BTreeMap<Vec<u8>, Vec<Vec<u8>>> = BTreeMap::new();
    for commit in commits {
        check(cancel)?;
        let author = commit.author()?.ok_or(Error::Metadata("missing author"))?;
        groups
            .entry(author.name.to_vec())
            .or_default()
            .push(commit.subject());
    }
    let mut out = super::Output {
        bytes: Vec::new(),
        limit,
        cancel,
    };
    out.append(message)?;
    if !message.ends_with(b"\n") {
        out.append(b"\n")?;
    }
    if !message.ends_with(b"\n\n") {
        out.append(b"\n")?;
    }
    for (author, subjects) in groups {
        out.append(&author)?;
        out.append(format!(" ({}):\n", subjects.len()).as_bytes())?;
        for subject in subjects {
            let subject =
                std::str::from_utf8(&subject).map_err(|_| Error::Metadata("subject UTF-8"))?;
            shortlog_subject(&mut out, subject)?;
        }
        out.append(b"\n")?;
    }
    Ok(out.bytes)
}

// Shortlog uses a narrower body layout than RFC header folding.
fn shortlog_subject(out: &mut super::Output<'_>, subject: &str) -> Result<(), Error> {
    out.append(b"  ")?;
    let mut column = 2;
    for (index, word) in subject.split_whitespace().enumerate() {
        let space = usize::from(index != 0);
        let width = word
            .chars()
            .map(|ch| unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0))
            .sum::<usize>();
        if column + space + width > 72 && index != 0 {
            out.append(b"\n    ")?;
            column = 4;
        } else if space != 0 {
            out.append(b" ")?;
            column += 1;
        }
        out.append(word.as_bytes())?;
        column += width;
    }
    out.append(b"\n")
}
