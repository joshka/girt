//! Bounded annotated-tag resolution over verified objects.
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{
    IdSet, Object, ObjectId, ObjectKind, ObjectReadError, Objects, ReadLimits, Tag, TagError,
};

/// Work limits for resolving one object through a chain of annotated tags.
///
/// Choose edge count, total decoded payload and per-object work independently. A shallow tag chain
/// can still contain a large annotation; a low depth limit does not cap its bytes.
///
/// ```
/// use girt::{PeelLimits, ReadLimits};
/// let limits = PeelLimits::default()
///     .with_max_tags(8)
///     .with_max_bytes(1024 * 1024)
///     .with_read(ReadLimits::default().with_max_object_bytes(256 * 1024));
/// assert_eq!(limits.max_tags, 8);
/// assert!(limits.read.max_object_bytes < limits.max_bytes);
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct PeelLimits {
    /// Maximum tag edges followed. Zero still permits reading a terminal object.
    pub max_tags: usize,
    /// Total payload bytes read, including the terminal object.
    pub max_bytes: usize,
    /// Per-read object and delta-work limits.
    pub read: ReadLimits,
}

impl PeelLimits {
    /// Sets [`Self::max_tags`].
    #[must_use]
    pub fn with_max_tags(mut self, max_tags: usize) -> Self {
        self.max_tags = max_tags;
        self
    }

    /// Sets [`Self::max_bytes`].
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Sets [`Self::read`].
    #[must_use]
    pub fn with_read(mut self, read: ReadLimits) -> Self {
        self.read = read;
        self
    }
}
impl Default for PeelLimits {
    fn default() -> Self {
        Self {
            max_tags: 32,
            max_bytes: 64 * 1024 * 1024,
            read: ReadLimits::default(),
        }
    }
}

impl PeelLimits {
    /// Returns generous finite bounds for caller-approved tag chains.
    ///
    /// This permits 10,000 tag objects and uses [`ReadLimits::trusted`] for each object. Large byte
    /// limits can exceed available memory. Choose this after accepting that cost. To relax only one
    /// dimension, start from [`Self::default`] instead; the example deliberately uses the generous
    /// byte/read policy while keeping a smaller tag count.
    ///
    /// ```
    /// use girt::PeelLimits;
    /// let limits = PeelLimits::trusted().with_max_tags(100);
    /// assert_eq!(limits.max_tags, 100);
    /// ```
    pub const fn trusted() -> Self {
        Self {
            max_tags: 10_000,
            max_bytes: usize::MAX / 4,
            read: ReadLimits::trusted(),
        }
    }
}

/// Resolved identity and kind, retaining the original tip and every intervening tag identity.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{InitKind, ObjectFormat, ObjectKind, PackLimits, PeelLimits, Repository};
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r.git"),
///     InitKind::Bare,
/// )?;
///
/// let blob = repo.loose_objects().write_blob(b"manifest")?;
/// let objects = repo.objects_with(PackLimits::default())?;
/// let result = objects.peel(blob, PeelLimits::default(), &AtomicBool::new(false))?;
///
/// assert_eq!(result.original, blob);
/// assert_eq!(result.target, blob);
/// assert_eq!(result.kind, ObjectKind::Blob);
/// assert!(result.tags.is_empty()); // An already-terminal object has no intervening tags.
///
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PeeledObject {
    /// Original requested identity, also retained when it was already a terminal object.
    pub original: ObjectId,
    /// Tag identities in outermost-to-innermost order.
    pub tags: Vec<ObjectId>,
    /// First non-tag identity.
    pub target: ObjectId,
    /// Verified storage kind of the terminal target.
    pub kind: ObjectKind,
}

/// A failed read-only resolution, with the exact link being examined.
///
/// Use the original tip, failing ID, parent tag and depth together when reporting a failed chain.
/// No partial successful target is returned. Inspect the source category before deciding whether
/// storage recovery, a larger budget, or cancellation policy applies.
///
/// ```
/// use girt::{InitKind, ObjectFormat, ObjectId, PackLimits, PeelFailure, PeelLimits, Repository};
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r.git"),
///     InitKind::Bare,
/// )?;
///
/// let objects = repo.objects_with(PackLimits::default())?;
/// let missing = ObjectId::Sha1([1; 20]);
/// let error = objects
///     .peel(
///         missing,
///         PeelLimits::default(),
///         &std::sync::atomic::AtomicBool::new(false),
///     )
///     .expect_err("not stored");
///
/// assert_eq!(error.id, missing);
/// assert!(matches!(error.source.as_ref(), PeelFailure::Missing));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[error("peeling {original} at {id}")]
#[non_exhaustive]
pub struct PeelError {
    /// Original requested tip.
    pub original: ObjectId,
    /// Object whose read or interpretation failed, or the next object at cancellation/cycle.
    pub id: ObjectId,
    /// Tag declaring this target; absent while examining the original tip.
    pub from: Option<ObjectId>,
    /// Number of tag edges already followed.
    pub depth: usize,
    /// Structured reason for failure.
    #[source]
    pub source: Box<PeelFailure>,
}

/// Reasons a tag chain cannot be resolved. No failure changes storage.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PeelFailure {
    /// The requested object is absent.
    /// Obtain the missing object or report incomplete storage; no missing-object fetch is implicit.
    #[error("missing object")]
    Missing,
    /// Object verification, I/O, format, or per-read resource failure.
    /// Inspect the storage source and distinguish its I/O, corruption, and budget failures.
    #[error(transparent)]
    Read(#[from] ObjectReadError),
    /// A tag's mandatory target records cannot be decoded.
    /// Report the offending tag ID and parser source; do not silently skip the tag.
    #[error(transparent)]
    Tag(#[from] TagError),
    /// A terminal commit's graph records cannot be decoded.
    /// Report the terminal commit ID and unreadable graph records.
    #[error(transparent)]
    Commit(#[from] crate::CommitError),
    /// Stored kind disagrees with the referring tag's declaration.
    /// Inspect the referring tag and actual target; neither the declaration nor a ref hint
    /// overrides verified kind.
    #[error("expected {expected:?}, found {actual:?}")]
    Kind {
        /// Declared kind.
        expected: ObjectKind,
        /// Kind verified by storage.
        actual: ObjectKind,
    },
    /// The next tag edge exceeds `max_tags`.
    /// Choose a larger tag-edge budget only after considering the input and resource cost.
    #[error("tag depth limit")]
    Depth,
    /// The total payload budget is exhausted.
    /// Choose an aggregate payload budget independently of each per-read limit.
    #[error("tag-chain byte limit")]
    Bytes,
    /// A previously visited identity was encountered again.
    /// Report the cyclic tag graph; restarting with larger limits cannot resolve it.
    #[error("tag cycle")]
    Cycle,
    /// Cancellation observed before or after a synchronous object read.
    /// Retry only with a new cancellation decision; no partial target was returned.
    #[error("tag peeling cancelled")]
    Cancelled,
}

impl Objects {
    /// Resolves tags to a terminal object, checking each declared target kind.
    ///
    /// Reads verified storage objects and decodes tag targets in the store's format. Terminal
    /// commits must have decodable graph records; terminal trees remain uninterpreted, matching
    /// Git peeling. Metadata and canonical construction rules are separate. Cancellation is checked
    /// before and after each synchronous read, not during it. Memory is bounded by tag count
    /// and one per-read payload; total payload and per-read delta work are bounded separately.
    /// No files or references are changed and no partial result is returned.
    ///
    /// # Errors
    ///
    /// Returns contextual [`PeelError`] for missing/corrupt/wrong-kind targets, malformed tags,
    /// cycles, limits or cancellation. Existing corrupt objects never become an absent result.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::{
    ///     InitKind, ObjectFormat, ObjectKind, PackLimits, PeelLimits, Repository, Tag, TagFields,
    /// };
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let loose = repo.loose_objects();
    /// let blob = loose.write_blob(b"manifest")?;
    /// let tag = Tag::new(TagFields {
    ///     target: blob,
    ///     target_kind: ObjectKind::Blob,
    ///     name: b"v1".to_vec(),
    ///     tagger: None,
    ///     extra_headers: vec![],
    ///     message: b"Reviewed".to_vec(),
    /// })?;
    /// let inner = loose.write_tag(&tag)?;
    ///
    /// let outer = Tag::new(TagFields {
    ///     target: inner,
    ///     target_kind: ObjectKind::Tag,
    ///     name: b"release".to_vec(),
    ///     tagger: None,
    ///     extra_headers: vec![],
    ///     message: b"Release pointer".to_vec(),
    /// })?;
    /// let tip = loose.write_tag(&outer)?;
    ///
    /// let objects = repo.objects_with(PackLimits::default())?;
    /// let peeled = objects.peel(tip, PeelLimits::default(), &AtomicBool::new(false))?;
    ///
    /// assert_eq!(peeled.tags, [tip, inner]);
    /// assert_eq!(peeled.target, blob);
    /// assert_eq!(peeled.kind, ObjectKind::Blob);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn peel(
        &self,
        id: ObjectId,
        limits: PeelLimits,
        cancel: &AtomicBool,
    ) -> Result<PeeledObject, PeelError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(target: "girt", "objects.peel",
            outcome = "incomplete", failure_class = tracing::field::Empty);
        let operation = || peel(id, limits, cancel, |id, read| self.read_with(id, read));
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = operation();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::peel);
        result
    }
}

pub(crate) fn peel(
    original: ObjectId,
    limits: PeelLimits,
    cancel: &AtomicBool,
    mut read: impl FnMut(ObjectId, ReadLimits) -> Result<Option<Object>, ObjectReadError>,
) -> Result<PeeledObject, PeelError> {
    let mut id = original;
    let mut tags = Vec::new();
    let mut seen = IdSet::default();
    let mut expected = None;
    let mut remaining = limits.max_bytes;
    loop {
        let context = |source| PeelError {
            original,
            id,
            from: tags.last().copied(),
            depth: tags.len(),
            source: Box::new(source),
        };
        if cancel.load(Ordering::Relaxed) {
            return Err(context(PeelFailure::Cancelled));
        }
        if !seen.insert(id) {
            return Err(context(PeelFailure::Cycle));
        }
        let mut read_limits = limits.read;
        read_limits.max_object_bytes = read_limits.max_object_bytes.min(remaining);
        let object = read(id, read_limits);
        if cancel.load(Ordering::Relaxed) {
            return Err(context(PeelFailure::Cancelled));
        }
        let object = object
            .map_err(|e| context(PeelFailure::Read(e)))?
            .ok_or_else(|| context(PeelFailure::Missing))?;
        remaining = remaining
            .checked_sub(object.data().len())
            .ok_or_else(|| context(PeelFailure::Bytes))?;
        if let Some(expected) = expected
            && expected != object.kind()
        {
            return Err(context(PeelFailure::Kind {
                expected,
                actual: object.kind(),
            }));
        }
        if object.kind() != ObjectKind::Tag {
            if object.kind() == ObjectKind::Commit {
                crate::Commit::parse_graph(id.format(), object.data())
                    .map_err(|e| context(PeelFailure::Commit(e)))?;
            }
            return Ok(PeeledObject {
                original,
                tags,
                target: id,
                kind: object.kind(),
            });
        }
        if tags.len() == limits.max_tags {
            return Err(context(PeelFailure::Depth));
        }
        let (target, kind) = Tag::parse_target(id.format(), object.data())
            .map_err(|e| context(PeelFailure::Tag(e)))?;
        expected = Some(kind);
        tags.push(id);
        id = target;
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{ObjectFormat, TagFields};

    fn link(format: ObjectFormat, target: ObjectId) -> Object {
        let tag = Tag::new(TagFields {
            target,
            target_kind: ObjectKind::Tag,
            name: b"v".to_vec(),
            tagger: None,
            extra_headers: vec![],
            message: vec![],
        })
        .unwrap();
        Object {
            format,
            kind: ObjectKind::Tag,
            data: tag.encode().into(),
        }
    }

    // Verified content-addressed cycles cannot feasibly be manufactured. Inject a reader only at
    // this private boundary to exercise the traversal guard without weakening public storage.
    #[rstest]
    fn cycle_retains_failing_link(
        #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    ) {
        let id = ObjectId::null(format);
        let error = peel(
            id,
            PeelLimits::default(),
            &AtomicBool::new(false),
            |_, _| Ok(Some(link(format, id))),
        )
        .unwrap_err();
        assert_eq!(
            (error.original, error.id, error.from, error.depth),
            (id, id, Some(id), 1)
        );
        assert!(matches!(*error.source, PeelFailure::Cycle));
    }

    #[test]
    fn cancellation_after_read_prevents_target_following() {
        let cancel = AtomicBool::new(false);
        let id = ObjectId::null(ObjectFormat::Sha1);
        let error = peel(id, PeelLimits::default(), &cancel, |_, _| {
            cancel.store(true, Ordering::Relaxed);
            Ok(Some(link(ObjectFormat::Sha1, id)))
        })
        .unwrap_err();
        assert_eq!(error.depth, 0);
        assert!(matches!(*error.source, PeelFailure::Cancelled));
    }

    #[test]
    fn byte_budget_is_checked_even_for_a_misbehaving_reader() {
        let id = ObjectId::null(ObjectFormat::Sha1);
        let error = peel(
            id,
            PeelLimits {
                max_bytes: 0,
                ..PeelLimits::default()
            },
            &AtomicBool::new(false),
            |_, _| Ok(Some(link(ObjectFormat::Sha1, id))),
        )
        .unwrap_err();
        assert!(matches!(*error.source, PeelFailure::Bytes));
    }
}
