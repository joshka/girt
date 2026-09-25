//! Bounded annotated-tag resolution over verified objects.
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{Object, ObjectId, ObjectKind, ObjectReadError, Objects, ReadLimits, Tag, TagError};

/// Work limits for resolving one object through a chain of annotated tags.
#[derive(Clone, Copy, Debug)]
pub struct PeelLimits {
    /// Maximum tag edges followed. Zero still permits reading a terminal object.
    pub max_tags: usize,
    /// Total payload bytes read, including the terminal object.
    pub max_bytes: usize,
    /// Per-read object and delta-work limits.
    pub read: ReadLimits,
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

/// Resolved identity and kind, retaining the original tip and every intervening tag identity.
#[derive(Clone, Debug, Eq, PartialEq)]
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
#[derive(Debug, thiserror::Error)]
#[error("peeling {original} at {id}: {source}")]
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
pub enum PeelFailure {
    /// The requested object is absent.
    #[error("missing object")]
    Missing,
    /// Object verification, I/O, format, or per-read resource failure.
    #[error(transparent)]
    Read(#[from] ObjectReadError),
    /// A tag's mandatory target records cannot be decoded.
    #[error(transparent)]
    Tag(#[from] TagError),
    /// A terminal commit's graph records cannot be decoded.
    #[error(transparent)]
    Commit(#[from] crate::CommitError),
    /// Stored kind disagrees with the referring tag's declaration.
    #[error("expected {expected:?}, found {actual:?}")]
    Kind {
        /// Declared kind.
        expected: ObjectKind,
        /// Kind verified by storage.
        actual: ObjectKind,
    },
    /// The next tag edge exceeds `max_tags`.
    #[error("tag depth limit")]
    Depth,
    /// The total payload budget is exhausted.
    #[error("tag-chain byte limit")]
    Bytes,
    /// A previously visited identity was encountered again.
    #[error("tag cycle")]
    Cycle,
    /// Cancellation observed before or after a synchronous object read.
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
    pub fn peel(
        &self,
        id: ObjectId,
        limits: PeelLimits,
        cancel: &AtomicBool,
    ) -> Result<PeeledObject, PeelError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(target: "girt", "objects.peel",
            outcome = "incomplete", failure_class = tracing::field::Empty);
        let operation = || peel(id, limits, cancel, |id, read| self.read(id, read));
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = operation();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::peel);
        result
    }
}

fn peel(
    original: ObjectId,
    limits: PeelLimits,
    cancel: &AtomicBool,
    mut read: impl FnMut(ObjectId, ReadLimits) -> Result<Option<Object>, ObjectReadError>,
) -> Result<PeeledObject, PeelError> {
    let mut id = original;
    let mut tags = Vec::new();
    let mut seen = HashSet::new();
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
                crate::Commit::parse(id.format(), object.data())
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
        let tag =
            Tag::parse(id.format(), object.data()).map_err(|e| context(PeelFailure::Tag(e)))?;
        expected = Some(tag.target_kind());
        tags.push(id);
        id = tag.target();
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
            data: tag.encode(),
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
