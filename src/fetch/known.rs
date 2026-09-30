use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::{FetchError as Error, FetchLimits, check_cancelled};
use crate::{Object, ObjectId, ObjectKind, Objects};

/// Verified local history offered to upload-pack, including declared shallow boundaries.
///
/// Construction reads and validates the reachable graph from explicit roots to any declared
/// shallow boundaries, including trees and typed tag targets, skipping external gitlinks. Missing
/// or corrupt local objects fail
/// before any have is sent. Roots may be any supported kind; only commits become have lines.
/// Commit discovery is breadth-first in caller root and payload edge order, capped by
/// [`FetchLimits::max_haves`]. A small cap can reduce negotiation effectiveness, never correctness.
/// No local references are inferred. Retained payloads freeze the validation evidence; callers
/// must still coordinate with GC until installation and any subsequent reference publication.
#[derive(Debug, Default)]
pub struct KnownHistory {
    pub(super) objects: HashMap<ObjectId, Object>,
    /// Destination store whose objects are trusted to have complete history, as Git trusts
    /// local objects. Consulted after `objects`.
    pub(super) store: Option<Arc<Objects>>,
    pub(super) haves: Vec<ObjectId>,
    pub(super) shallow: Vec<ObjectId>,
    pub(super) only_when_all_wants_known: bool,
}

/// Objects the destination already has, for delta bases and connectivity.
pub(crate) trait KnownObjects {
    /// Returns a known object.
    fn get(&self, id: ObjectId) -> Option<Cow<'_, Object>>;

    /// Whether a known object's history is trusted to be complete, so connectivity checks can
    /// stop at it rather than walking its history.
    fn trusted_complete(&self) -> bool {
        false
    }
}

impl KnownObjects for HashMap<ObjectId, Object> {
    fn get(&self, id: ObjectId) -> Option<Cow<'_, Object>> {
        HashMap::get(self, &id).map(Cow::Borrowed)
    }
}

impl KnownObjects for KnownHistory {
    fn get(&self, id: ObjectId) -> Option<Cow<'_, Object>> {
        if let Some(object) = self.objects.get(&id) {
            return Some(Cow::Borrowed(object));
        }
        let limits = crate::ReadLimits {
            max_object_bytes: usize::MAX / 4,
            max_delta_bytes: usize::MAX / 4,
            max_decode_bytes: usize::MAX / 4,
            max_input_bytes: usize::MAX / 4,
            max_delta_depth: 10_000,
        };
        self.store
            .as_ref()?
            .read(id, limits)
            .ok()
            .flatten()
            .map(Cow::Owned)
    }

    fn trusted_complete(&self) -> bool {
        self.store.is_some()
    }
}

impl KnownHistory {
    /// Describes a destination store for negotiation without reading its complete history.
    ///
    /// Up to [`FetchLimits::max_haves`] commits reachable from `tips` (breadth-first, peeling
    /// tags) are offered as haves. The store's shallow roots are declared. Like Git, objects
    /// already in the store are trusted to have complete history: received objects may depend on
    /// any of them, and connectivity checks stop there. Callers must keep those objects
    /// available (e.g. exclude pruning) until the fetch publishes its references.
    ///
    /// # Errors
    ///
    /// Fails on cancellation or when a tip can't be read. Unreadable ancestors end the walk.
    pub fn from_store(
        store: Objects,
        tips: &[ObjectId],
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let shallow: Vec<_> = store.shallow_roots().iter().collect();
        let shallow_set: HashSet<_> = shallow.iter().copied().collect();
        let mut haves = Vec::new();
        let mut seen = HashSet::new();
        let mut pending = VecDeque::new();
        for &tip in tips {
            check_cancelled(cancel)?;
            let Ok(peeled) = store.peel(tip, crate::PeelLimits::default(), cancel) else {
                continue;
            };
            if peeled.kind == ObjectKind::Commit && seen.insert(peeled.target) {
                pending.push_back(peeled.target);
            }
        }
        while let Some(id) = pending.pop_front() {
            if haves.len() >= limits.max_haves {
                break;
            }
            check_cancelled(cancel)?;
            let Ok(Some(object)) = store.read(id, limits.known_read) else {
                continue;
            };
            if object.kind() != ObjectKind::Commit {
                continue;
            }
            haves.push(id);
            if shallow_set.contains(&id) {
                continue;
            }
            let Ok(commit) = crate::Commit::parse(store.object_format(), object.data()) else {
                continue;
            };
            for &parent in commit.parents() {
                if seen.insert(parent) {
                    pending.push_back(parent);
                }
            }
        }
        Ok(Self {
            objects: HashMap::new(),
            store: Some(Arc::new(store)),
            haves,
            shallow,
            only_when_all_wants_known: false,
        })
    }

    /// Uses this history only when it contains every selected advertised tip.
    ///
    /// A selection with any unknown tip instead requests a complete transfer. This avoids
    /// producing a pack that depends on local objects when the caller can retain only the new
    /// pack against concurrent collection. The caller must still keep verified local objects
    /// available until a known-only result publishes its references.
    /// Explicit depth requests retain the snapshot even for unknown tips: validation includes
    /// reachable local dependencies in the resulting pack before retained installation.
    pub fn only_when_all_wants_known(mut self) -> Self {
        self.only_when_all_wants_known = true;
        self
    }

    pub(super) fn applies_to(&self, wants: &[ObjectId]) -> bool {
        !self.only_when_all_wants_known || wants.iter().all(|id| self.objects.contains_key(id))
    }

    /// Keep the destination's shallow declarations without offering local objects as haves.
    /// An unknown selected tip still needs a self-contained retained pack, but the server must
    /// see existing boundaries so it can report any roots it removes during deepening.
    pub(super) fn shallow_only(&self) -> Self {
        Self {
            shallow: self.shallow.clone(),
            store: self.store.clone(),
            ..Self::default()
        }
    }
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
    /// bounds. Declared shallow commits stop parent traversal and are sent to the peer. To request
    /// a full transfer into a complete destination, explicitly use [`KnownHistory::default`].
    pub fn new(
        store: &Objects,
        roots: &[ObjectId],
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        Self::new_local(store, roots, limits, cancel)
    }

    /// Validates local history in either object format for native transfer decisions.
    ///
    /// Uses [`Self::new`]'s graph, resource and cancellation contract. The caller must keep
    /// verified roots available through publication.
    pub fn new_local(
        store: &Objects,
        roots: &[ObjectId],
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let shallow: Vec<_> = store.shallow_roots().iter().collect();
        Self::new_with_boundaries(store, roots, &shallow, limits, cancel)
    }

    pub(super) fn new_with_boundaries(
        store: &Objects,
        roots: &[ObjectId],
        shallow: &[ObjectId],
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        check_cancelled(cancel)?;
        if roots.len() > limits.max_wants {
            return Err(Error::Limit("known roots"));
        }
        let mut result = Self {
            shallow: shallow.to_vec(),
            ..Self::default()
        };
        if result.shallow.len() > limits.max_shallow_roots {
            return Err(Error::Limit("shallow roots"));
        }
        let shallow_set: HashSet<_> = shallow.iter().copied().collect();
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
            if shallow_set.contains(&id) && object.kind() != ObjectKind::Commit {
                return Err(Error::Kind(id));
            }
            if expected[&id].is_some_and(|kind| kind != object.kind()) {
                return Err(Error::Kind(id));
            }
            bytes = bytes
                .checked_sub(object.data().len())
                .ok_or(Error::Limit("known bytes"))?;
            let shallow_commit = object.kind() == ObjectKind::Commit && shallow_set.contains(&id);
            let edge = |target, kind| {
                if shallow_commit && kind == ObjectKind::Commit {
                    return Ok(());
                }
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
        result.shallow.retain(|id| result.objects.contains_key(id));
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
            crate::AlternateLimits::default(),
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

    #[test]
    fn store_history_offers_commits_and_reads_objects_on_demand() {
        let (_root, objects, commit, blob) = graph();
        let known = KnownHistory::from_store(
            objects,
            &[commit],
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(known.haves(), &[commit]);
        assert_eq!(known.object_count(), 0);
        assert!(known.trusted_complete());
        assert_eq!(known.get(blob).unwrap().kind(), ObjectKind::Blob);
    }

    #[test]
    fn connectivity_stops_at_trusted_store_objects() {
        let (_root, objects, commit, _blob) = graph();
        let known = KnownHistory::from_store(
            objects,
            &[commit],
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let dependencies = super::super::connectivity::validate_with_boundaries(
            &HashMap::new(),
            &known,
            &[commit],
            &[],
            FetchLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(dependencies, [commit]);
    }
}
