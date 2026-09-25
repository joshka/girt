use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicBool;

use super::{FetchError as Error, FetchLimits, check_cancelled};
use crate::{Object, ObjectId, ObjectKind, Objects};

/// Verified local history offered to upload-pack as knowledge of complete commit ancestry.
///
/// Construction reads and validates the entire reachable graph from explicit roots, including
/// trees and typed tag targets, skipping external gitlinks. Missing or corrupt local objects fail
/// before any have is sent. Roots may be any supported kind; only commits become have lines.
/// Commit discovery is breadth-first in caller root and payload edge order, capped by
/// [`FetchLimits::max_haves`]. A small cap can reduce negotiation effectiveness, never correctness.
/// No local references are inferred. Retained payloads freeze the validation evidence; callers
/// must still coordinate with GC until installation and any subsequent reference publication.
#[derive(Debug, Default)]
pub struct KnownHistory {
    pub(super) objects: HashMap<ObjectId, Object>,
    pub(super) haves: Vec<ObjectId>,
}

impl KnownHistory {
    /// Reads a bounded, identity-verified local graph without changing storage.
    ///
    /// Local object count, retained bytes, edges and per-read decoding use the `max_known_*` and
    /// `known_read` bounds. Root occurrences use `max_wants`. These budgets are separate from
    /// transfer/import budgets. Cancellation is checked between reads and edges; an individual
    /// filesystem read, decode, hash or parse cannot be interrupted.
    ///
    /// # Errors
    ///
    /// Fails on missing/corrupt objects, malformed payloads, mistyped edges, cancellation or
    /// bounds. SHA-256 stores are refused because fetch negotiation is currently SHA-1-only.
    /// Shallow snapshots are refused until boundary negotiation is implemented. To request a
    /// full transfer into a complete destination, explicitly use [`KnownHistory::default`].
    pub fn new(
        store: &Objects,
        roots: &[ObjectId],
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        check_cancelled(cancel)?;
        if store.object_format() != crate::ObjectFormat::Sha1 {
            return Err(Error::Unsupported("SHA-256 fetch negotiation"));
        }
        if !store.shallow_roots().is_empty() {
            return Err(Error::Unsupported("shallow fetch negotiation"));
        }
        if roots.len() > limits.max_wants {
            return Err(Error::Limit("known roots"));
        }
        let mut result = Self::default();
        let mut pending = VecDeque::new();
        let mut expected = HashMap::new();
        for &id in roots {
            enqueue(id, None, &mut expected, &mut pending, limits)?;
        }
        let mut bytes = limits.max_known_bytes;
        let mut edges = limits.max_known_edges;
        while let Some(id) = pending.pop_front() {
            check_cancelled(cancel)?;
            let mut read = limits.known_read;
            read.max_object_bytes = read.max_object_bytes.min(bytes);
            let object = store
                .read(id, read)
                .map_err(|source| Error::LocalRead { id, source })?
                .ok_or(Error::Missing(id))?;
            if expected[&id].is_some_and(|kind| kind != object.kind()) {
                return Err(Error::Kind(id));
            }
            bytes = bytes
                .checked_sub(object.data().len())
                .ok_or(Error::Limit("known bytes"))?;
            let edge = |target, kind| {
                check_cancelled(cancel)?;
                edges = edges.checked_sub(1).ok_or(Error::Limit("known edges"))?;
                if result
                    .objects
                    .get(&target)
                    .is_some_and(|o| o.kind() != kind)
                {
                    return Err(Error::Kind(target));
                }
                enqueue(target, Some(kind), &mut expected, &mut pending, limits)
            };
            crate::edges::visit(id, &object, edge)?;
            if object.kind() == ObjectKind::Commit && result.haves.len() < limits.max_haves {
                result.haves.push(id);
            }
            if expected[&id].is_some_and(|kind| kind != object.kind()) {
                return Err(Error::Kind(id));
            }
            result.objects.insert(id, object);
        }
        Ok(result)
    }

    /// Number of distinct verified local objects retained for connectivity checks.
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Verified commits available for the single have batch, in discovery order.
    pub fn haves(&self) -> &[ObjectId] {
        &self.haves
    }
}

fn enqueue(
    id: ObjectId,
    kind: Option<ObjectKind>,
    expected: &mut HashMap<ObjectId, Option<ObjectKind>>,
    pending: &mut VecDeque<ObjectId>,
    limits: FetchLimits,
) -> Result<(), Error> {
    if let Some(previous) = expected.get_mut(&id) {
        if previous.is_some() && kind.is_some() && *previous != kind {
            return Err(Error::Kind(id));
        }
        if kind.is_some() {
            *previous = kind;
        }
    } else {
        if expected.len() >= limits.max_known_objects {
            return Err(Error::Limit("known objects"));
        }
        expected.insert(id, kind);
        pending.push_back(id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        Commit, CommitFields, EntryMode, LooseObjects, ObjectFormat, PackLimits, Signature, Tree,
        TreeEntry,
    };

    fn graph() -> (tempfile::TempDir, Objects, ObjectId, ObjectId) {
        let root = tempfile::tempdir().unwrap();
        let loose = LooseObjects::new(root.path(), ObjectFormat::Sha1);
        let blob = loose.write_blob(b"known blob").unwrap();
        let tree = loose
            .write_tree(
                &Tree::new(
                    crate::ObjectFormat::Sha1,
                    vec![
                        TreeEntry {
                            mode: EntryMode::Blob,
                            name: b"blob".to_vec(),
                            id: blob,
                        },
                        TreeEntry {
                            mode: EntryMode::Gitlink,
                            name: b"external".to_vec(),
                            id: ObjectId::for_blob(crate::ObjectFormat::Sha1, b"external"),
                        },
                    ],
                )
                .unwrap(),
            )
            .unwrap();
        let author = Signature {
            name: b"A".to_vec(),
            email: b"a@example.com".to_vec(),
            seconds: 0,
            offset_minutes: 0,
        };
        let commit = loose
            .write_commit(
                &Commit::new(CommitFields {
                    tree,
                    parents: vec![],
                    author: author.clone(),
                    committer: author,
                    extra_headers: vec![],
                    message: vec![],
                })
                .unwrap(),
            )
            .unwrap();
        let objects = Objects::open(
            crate::ObjectFormat::Sha1,
            root.path(),
            PackLimits::default(),
        )
        .unwrap();
        (root, objects, commit, blob)
    }

    #[rstest]
    #[case::objects(FetchLimits { max_known_objects: 2, ..FetchLimits::default() })]
    #[case::bytes(FetchLimits { max_known_bytes: 1, ..FetchLimits::default() })]
    #[case::edges(FetchLimits { max_known_edges: 1, ..FetchLimits::default() })]
    #[case::roots(FetchLimits { max_wants: 0, ..FetchLimits::default() })]
    fn rejects_local_work_bounds(#[case] limits: FetchLimits) {
        let (_root, objects, commit, _) = graph();
        assert!(KnownHistory::new(&objects, &[commit], limits, &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn exact_count_and_edges_skip_gitlinks_and_bound_haves() {
        let (_root, objects, commit, _) = graph();
        let limits = FetchLimits {
            max_known_objects: 3,
            max_known_edges: 2,
            max_haves: 0,
            ..FetchLimits::default()
        };
        let known = KnownHistory::new(&objects, &[commit, commit], limits, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(known.object_count(), 3);
        assert!(known.haves().is_empty());
    }

    fn damage(root: &std::path::Path, blob: ObjectId, corrupt: bool) {
        let id = blob.to_string();
        let path = root.join(&id[..2]).join(&id[2..]);
        if corrupt {
            std::fs::write(path, b"corrupt").unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }

    #[rstest]
    #[case::missing(false)]
    #[case::corrupt(true)]
    fn incomplete_history_cannot_be_offered(#[case] corrupt: bool) {
        let (root, objects, commit, blob) = graph();
        damage(root.path(), blob, corrupt);
        assert!(
            KnownHistory::new(
                &objects,
                &[commit],
                FetchLimits::default(),
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }

    #[test]
    fn corrupt_local_dependency_reports_identity_and_storage_cause() {
        let (root, objects, commit, blob) = graph();
        damage(root.path(), blob, true);
        let error = KnownHistory::new(
            &objects,
            &[commit],
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::LocalRead { id, source: crate::ObjectReadError::Loose(_) } if id == blob)
        );
    }

    #[test]
    fn cancelled_preparation_retains_no_partial_knowledge() {
        let (_root, objects, commit, _) = graph();
        assert!(matches!(
            KnownHistory::new(
                &objects,
                &[commit],
                FetchLimits::default(),
                &AtomicBool::new(true)
            ),
            Err(Error::Cancelled)
        ));
    }
}
