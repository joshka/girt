//! Listing every object reachable from a set of tips, like `git rev-list --objects`.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::AtomicBool;

use super::{Objects, PromisedObjects};
use crate::pack::reader::check_cancelled;
use crate::{ListLimits, ObjectId, ObjectKind, ObjectReadError, ReadLimits};

/// What a walk does when a reachable object is not stored, like `git rev-list --missing`.
///
/// Every policy that continues past a missing object records it in [`ObjectWalk::missing`], so
/// callers can fetch the skipped objects in one batch later. `--missing=print` corresponds to
/// [`Self::AllowAny`] and reading that list. The walk never fetches.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum MissingObjects {
    /// Stop at the first missing object, as `--missing=error` (Git's default) does.
    #[default]
    Error,
    /// Continue past objects a promisor pack references, as `--missing=allow-promisor` does,
    /// and stop at any other missing object. See [`Objects::promised_objects`].
    ///
    /// As with Git, a missing parent commit stops the walk even when promised, and nothing is
    /// promised outside a partial clone.
    AllowPromisor,
    /// Continue past every missing object, as `--missing=allow-any` does.
    AllowAny,
}

/// Bounds for [`Objects::walk_objects`].
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct ObjectWalkLimits {
    /// Maximum distinct objects visited, stored or missing.
    pub max_objects: usize,
    /// Bounds for each object read.
    pub read: ReadLimits,
    /// Bounds for building the promised set under [`MissingObjects::AllowPromisor`].
    pub promised: ListLimits,
}

impl ObjectWalkLimits {
    /// Sets [`Self::max_objects`].
    #[must_use]
    pub fn with_max_objects(mut self, max_objects: usize) -> Self {
        self.max_objects = max_objects;
        self
    }

    /// Sets [`Self::read`].
    #[must_use]
    pub fn with_read(mut self, read: ReadLimits) -> Self {
        self.read = read;
        self
    }

    /// Sets [`Self::promised`].
    #[must_use]
    pub fn with_promised(mut self, promised: ListLimits) -> Self {
        self.promised = promised;
        self
    }
}

impl Default for ObjectWalkLimits {
    fn default() -> Self {
        Self {
            max_objects: 1 << 27,
            read: ReadLimits::default(),
            promised: ListLimits::default(),
        }
    }
}

/// Objects found by [`Objects::walk_objects`], in discovery order.
///
/// Discovery order is breadth-first from the tips; it is not Git's output order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct ObjectWalk {
    /// Stored objects reached from the tips, each once, with its kind.
    pub objects: Vec<(ObjectId, ObjectKind)>,
    /// Objects the policy allowed to be missing, each once, with the kind the referencing
    /// object expects.
    pub missing: Vec<(ObjectId, ObjectKind)>,
}

/// An object walk stopped before reaching every object.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ObjectWalkError {
    /// A tip is not stored. Tips are never allowed to be missing.
    #[error("missing tip object {0}")]
    MissingTip(ObjectId),
    /// A reachable object is not stored and the policy does not allow it.
    #[error("missing {} {id}", kind.as_str())]
    Missing {
        /// Missing identity.
        id: ObjectId,
        /// Kind the referencing object expects.
        kind: ObjectKind,
    },
    /// A reference names an object of another kind.
    #[error("{id} is a {} but is referenced as a {}", actual.as_str(), expected.as_str())]
    WrongKind {
        /// Referenced identity.
        id: ObjectId,
        /// Kind the referencing object expects.
        expected: ObjectKind,
        /// Kind actually stored.
        actual: ObjectKind,
    },
    /// A commit, tree or tag payload does not parse, so its references are unknown.
    #[error("malformed {} {id}", kind.as_str())]
    Malformed {
        /// Unparseable object.
        id: ObjectId,
        /// Its stored kind.
        kind: ObjectKind,
    },
    /// [`ObjectWalkLimits::max_objects`] ran out.
    #[error("object walk limit exceeded")]
    Limit,
    /// Reading an object or building the promised set failed, or the walk was cancelled.
    #[error(transparent)]
    Read(#[from] ObjectReadError),
}

impl From<crate::edges::Error> for ObjectWalkError {
    fn from(error: crate::edges::Error) -> Self {
        let (id, kind) = match error {
            crate::edges::Error::Commit { id, .. } => (id, ObjectKind::Commit),
            crate::edges::Error::Tree { id, .. } => (id, ObjectKind::Tree),
            crate::edges::Error::Tag { id, .. } => (id, ObjectKind::Tag),
        };
        Self::Malformed { id, kind }
    }
}

impl Objects {
    /// Lists every object reachable from `tips`, like `git rev-list --objects <tips>`.
    ///
    /// Follows commit parents and trees, tree entries and tag targets. Gitlinks name commits in
    /// other repositories and are not followed. Parents of shallow commits are not followed.
    /// Blob payloads are not read; their headers establish existence and kind. `missing`
    /// decides whether an object that is not stored stops the walk; see [`MissingObjects`].
    /// Reading never fetches from a promisor remote. The promised set is built only when the
    /// first missing object needs classifying, so complete repositories never pay for it.
    ///
    /// # Errors
    ///
    /// Returns the first missing object the policy refuses, a kind mismatch, an unparseable
    /// object, storage failures, [`ObjectWalkError::Limit`] and cancellation as
    /// [`ObjectReadError::Cancelled`].
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::{InitKind, MissingObjects, ObjectFormat, ObjectWalkLimits, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    /// let missing = girt::ObjectId::for_blob(ObjectFormat::Sha1, b"not stored");
    /// let entry = girt::TreeEntry {
    ///     mode: girt::EntryMode::Blob,
    ///     name: b"file".to_vec(),
    ///     id: missing,
    /// };
    /// let tree = girt::Tree::new(ObjectFormat::Sha1, vec![entry])?;
    /// let tree = repo.loose_objects().write_tree(&tree)?;
    ///
    /// let objects = repo.objects()?;
    /// let cancel = AtomicBool::new(false);
    /// let limits = ObjectWalkLimits::default();
    /// assert!(
    ///     objects
    ///         .walk_objects(&[tree], MissingObjects::Error, limits, &cancel)
    ///         .is_err()
    /// );
    /// let walk = objects.walk_objects(&[tree], MissingObjects::AllowAny, limits, &cancel)?;
    /// assert_eq!(walk.objects, [(tree, girt::ObjectKind::Tree)]);
    /// assert_eq!(walk.missing, [(missing, girt::ObjectKind::Blob)]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn walk_objects(
        &self,
        tips: &[ObjectId],
        missing: MissingObjects,
        limits: ObjectWalkLimits,
        cancelled: &AtomicBool,
    ) -> Result<ObjectWalk, ObjectWalkError> {
        let mut walk = ObjectWalk::default();
        let mut seen = HashSet::new();
        let mut pending = VecDeque::new();
        let mut promised: Option<PromisedObjects> = None;
        // Each queued object is charged when it is first seen, so the queue never outgrows the
        // budget. The flag marks commit parents, which Git never forgives as promised.
        let enqueue = |seen: &mut HashSet<ObjectId>,
                       pending: &mut VecDeque<(ObjectId, Option<ObjectKind>, bool)>,
                       entry: (ObjectId, Option<ObjectKind>, bool)|
         -> Result<(), ObjectWalkError> {
            check_cancelled(cancelled)?;
            if seen.contains(&entry.0) {
                return Ok(());
            }
            if seen.len() >= limits.max_objects {
                return Err(ObjectWalkError::Limit);
            }
            seen.insert(entry.0);
            pending.push_back(entry);
            Ok(())
        };
        for &tip in tips {
            enqueue(&mut seen, &mut pending, (tip, None, false))?;
        }
        while let Some((id, expected, parent)) = pending.pop_front() {
            check_cancelled(cancelled)?;
            let stored = if expected == Some(ObjectKind::Blob) {
                self.header(id, limits.read)?
                    .map(|header| (header.kind(), None))
            } else {
                self.read_controlled(id, limits.read, cancelled)?
                    .map(|object| (object.kind(), Some(object)))
            };
            let Some((kind, object)) = stored else {
                let Some(kind) = expected else {
                    return Err(ObjectWalkError::MissingTip(id));
                };
                let allowed = match missing {
                    MissingObjects::Error => false,
                    MissingObjects::AllowAny => true,
                    MissingObjects::AllowPromisor if parent => false,
                    MissingObjects::AllowPromisor => {
                        let promised = match &mut promised {
                            Some(promised) => promised,
                            empty => empty.insert(self.promised_objects(
                                limits.promised,
                                limits.read,
                                cancelled,
                            )?),
                        };
                        promised.contains(id)
                    }
                };
                if !allowed {
                    return Err(ObjectWalkError::Missing { id, kind });
                }
                walk.missing.push((id, kind));
                continue;
            };
            if let Some(expected) = expected.filter(|expected| *expected != kind) {
                return Err(ObjectWalkError::WrongKind {
                    id,
                    expected,
                    actual: kind,
                });
            }
            walk.objects.push((id, kind));
            let Some(object) = object else {
                continue;
            };
            let shallow = kind == ObjectKind::Commit && self.shallow_roots().contains(id);
            crate::edges::visit(id, kind, object.data(), |target, target_kind| {
                let parent = kind == ObjectKind::Commit && target_kind == ObjectKind::Commit;
                if parent && shallow {
                    return Ok(());
                }
                enqueue(&mut seen, &mut pending, (target, Some(target_kind), parent))
            })?;
        }
        Ok(walk)
    }
}
